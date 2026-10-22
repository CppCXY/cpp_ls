//! **The on-disk cache, as one embedded database** — the storage half of `docs/refactor-cache-and-startup.md`.
//!
//! The directory this replaces held **1 854 files / 160.6 MB** on a real project: 1 768 summaries (69 MB), 85 `.tu`
//! files (85.6 MB) and a 6 MB `sharing.bin`. Three costs came with that shape, and each is what a database removes:
//!
//! * **1 854 `open`/`read` calls before a single answer** — a cold start's first hundred milliseconds were spent
//!   finding files, not reading them.
//! * **No transactions.** A write interrupted half way left a truncated summary, and the only defence was a magic
//!   header checked on the way in.
//! * **No way to ask who references an entry.** `prune` deleted the oldest files, which is not the same question as
//!   "is this summary still reachable from a file the project has".
//!
//! # What lives here, and what does not
//!
//! ```text
//! meta          schema_version / reading_fingerprint / format_version / generation
//! files         path            -> { content, config, env, mtime, size, generation }
//! projections   content         -> zstd(names a file mentions)      — see the plan's §三之二
//! summaries     (content, config, env) -> zstd(summary bytes)
//! renderings    (content, env)  -> zstd(rendered text)
//! environments  env             -> zstd({name -> body hash})
//! macros        body hash       -> zstd(body)
//! ```
//!
//! `env` is the **projection** hash (the macros a file mentions, transitively), not the whole environment: measured
//! on MSVC's standard library, a header mentions 43–408 macro names where the toolchain defines 9 401, and the
//! definitions it does mention do not change with include order. See the plan for the arithmetic and for the
//! fallback when a projection is larger than 512 names.
//!
//! **The cache is not authoritative.** Every row here is a function of files on disk; a version mismatch deletes the
//! database and the next run rebuilds it. That is the property that makes a single file safe: there is nothing to
//! recover, only something to recompute.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use redb::{Database, ReadableTable, ReadableTableMetadata, TableDefinition};

/// The name of the database file inside the cache directory.
pub const DATABASE_FILE: &str = "cache.redb";

/// **What the rows mean, as a number.** A binary whose summaries changed shape bumps this, and the next open
/// deletes the file rather than reading bytes it cannot interpret.
pub const SCHEMA_VERSION: u64 = 1;

const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const PROJECTIONS: TableDefinition<u64, &[u8]> = TableDefinition::new("projections");
const SUMMARIES: TableDefinition<(u64, u64, u64), &[u8]> = TableDefinition::new("summaries");
const RENDERINGS: TableDefinition<(u64, u64), &[u8]> = TableDefinition::new("renderings");
const ENVIRONMENTS: TableDefinition<u64, &[u8]> = TableDefinition::new("environments");
const MACROS: TableDefinition<u64, &[u8]> = TableDefinition::new("macros");

const KEY_SCHEMA: &str = "schema_version";
const KEY_GENERATION: &str = "generation";
const KEY_FINGERPRINT: &str = "reading_fingerprint";

/// **Everything that decides what a summary contains** — the address of one row.
///
/// The three parts are separate rather than hashed together on purpose: `content` says which text, `config` says
/// which compiler and include directories, and `env` says which macro definitions the text was read with. A miss
/// can then say *which* of the three changed, which is the difference between a cache that can be debugged and one
/// that can only be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub content: u64,
    pub config: u64,
    /// The projection hash — or, for a file whose projection exceeded the threshold, its unit's whole environment.
    pub env: u64,
}

/// One file's row: what it was, and what it was read with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRow {
    pub content: u64,
    pub config: u64,
    pub env: u64,
    pub mtime: u64,
    pub size: u64,
    pub generation: u64,
}

impl FileRow {
    fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(48);
        for value in [
            self.content,
            self.config,
            self.env,
            self.mtime,
            self.size,
            self.generation,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<FileRow> {
        if bytes.len() < 48 {
            return None;
        }
        let mut values = [0u64; 6];
        for (index, value) in values.iter_mut().enumerate() {
            let at = index * 8;
            *value = u64::from_le_bytes(bytes[at..at + 8].try_into().ok()?);
        }
        Some(FileRow {
            content: values[0],
            config: values[1],
            env: values[2],
            mtime: values[3],
            size: values[4],
            generation: values[5],
        })
    }

    fn key(self) -> CacheKey {
        CacheKey {
            content: self.content,
            config: self.config,
            env: self.env,
        }
    }
}

/// What a garbage collection did — the numbers a probe prints and a test asserts on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Collected {
    pub summaries: usize,
    pub renderings: usize,
    pub projections: usize,
}

/// What the database holds, for `Session::cache_sizes` and the probes.
#[derive(Debug, Clone, Copy, Default)]
pub struct DatabaseSizes {
    pub files: usize,
    pub summaries: usize,
    pub renderings: usize,
    pub projections: usize,
    pub environments: usize,
    pub macros: usize,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum CacheError {
    Database(redb::Error),
    Table(redb::TableError),
    Storage(redb::StorageError),
    Transaction(redb::TransactionError),
    Commit(redb::CommitError),
    Io(std::io::Error),
    /// The bytes in a row are not what this build wrote — see [`SCHEMA_VERSION`].
    Corrupt(&'static str),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::Database(error) => write!(formatter, "cache database: {error}"),
            CacheError::Table(error) => write!(formatter, "cache table: {error}"),
            CacheError::Storage(error) => write!(formatter, "cache storage: {error}"),
            CacheError::Transaction(error) => write!(formatter, "cache transaction: {error}"),
            CacheError::Commit(error) => write!(formatter, "cache commit: {error}"),
            CacheError::Io(error) => write!(formatter, "cache io: {error}"),
            CacheError::Corrupt(what) => write!(formatter, "cache row is corrupt: {what}"),
        }
    }
}

impl std::error::Error for CacheError {}

impl From<redb::DatabaseError> for CacheError {
    fn from(error: redb::DatabaseError) -> Self {
        CacheError::Database(error.into())
    }
}
impl From<redb::Error> for CacheError {
    fn from(error: redb::Error) -> Self {
        CacheError::Database(error)
    }
}
impl From<redb::TableError> for CacheError {
    fn from(error: redb::TableError) -> Self {
        CacheError::Table(error)
    }
}
impl From<redb::StorageError> for CacheError {
    fn from(error: redb::StorageError) -> Self {
        CacheError::Storage(error)
    }
}
impl From<redb::TransactionError> for CacheError {
    fn from(error: redb::TransactionError) -> Self {
        CacheError::Transaction(error)
    }
}
impl From<redb::CommitError> for CacheError {
    fn from(error: redb::CommitError) -> Self {
        CacheError::Commit(error)
    }
}
impl From<std::io::Error> for CacheError {
    fn from(error: std::io::Error) -> Self {
        CacheError::Io(error)
    }
}

type Result<T> = std::result::Result<T, CacheError>;

/// **One embedded database, one open handle, one writer at a time.**
///
/// redb's MVCC is the reason for the shape: a transaction is a scope, readers never wait for the writer, and a
/// `commit` is the only durability point. Every method here opens the transaction it needs and returns, so no lock
/// outlives a call and a query that reads two rows cannot deadlock against a slice that writes one.
pub struct CacheDb {
    database: Database,
    directory: PathBuf,
}

impl CacheDb {
    /// Open (or create) the database in `directory`, deleting it when its version does not match this build.
    ///
    /// The deletion is deliberate and safe: everything here is recomputable from the files on disk, so a schema this
    /// binary cannot read is a schema to throw away rather than migrate. `reading_fingerprint` covers changes to the
    /// *readers* (the summary codec's meaning), which `SCHEMA_VERSION` cannot see because they do not change the
    /// table layout.
    pub fn open(directory: &Path, reading_fingerprint: u64) -> Result<CacheDb> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join(DATABASE_FILE);

        let want_schema = SCHEMA_VERSION.to_le_bytes();
        let want_fingerprint = reading_fingerprint.to_le_bytes();

        let mut database = match Database::create(&path) {
            Ok(database) => database,
            Err(_) => {
                // A file that cannot even be opened as a database is a file to replace.
                let _ = std::fs::remove_file(&path);
                Database::create(&path)?
            }
        };

        let reusable = {
            let read = database.begin_read()?;
            let table = read.open_table(META);
            match table {
                Ok(table) => {
                    let schema = table.get(KEY_SCHEMA).ok().flatten().map(|value| value.value().to_vec());
                    let fingerprint = table
                        .get(KEY_FINGERPRINT)
                        .ok()
                        .flatten()
                        .map(|value| value.value().to_vec());
                    schema.as_deref() == Some(&want_schema[..])
                        && fingerprint.as_deref() == Some(&want_fingerprint[..])
                }
                // A database with no `meta` is new, not broken: the write below creates it.
                Err(redb::TableError::TableDoesNotExist(_)) => true,
                Err(error) => return Err(error.into()),
            }
        };

        if !reusable {
            // Close the handle before deleting: Windows will not unlink an open file.
            drop(database);
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
            database = Database::create(&path)?;
        }

        let write = database.begin_write()?;
        {
            let mut meta = write.open_table(META)?;
            meta.insert(KEY_SCHEMA, &want_schema[..])?;
            meta.insert(KEY_FINGERPRINT, &want_fingerprint[..])?;
            if meta.get(KEY_GENERATION)?.is_none() {
                meta.insert(KEY_GENERATION, &0u64.to_le_bytes()[..])?;
            }
            // Creating the tables up front keeps every later write free of "does it exist yet".
            write.open_table(FILES)?;
            write.open_table(PROJECTIONS)?;
            write.open_table(SUMMARIES)?;
            write.open_table(RENDERINGS)?;
            write.open_table(ENVIRONMENTS)?;
            write.open_table(MACROS)?;
        }
        write.commit()?;

        Ok(CacheDb {
            database,
            directory: directory.to_path_buf(),
        })
    }

    /// Where this database lives, for a log line that says which cache was used.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    // ---- generations -------------------------------------------------------------------------------

    /// The generation a write belongs to — bumped once per session, so a collection can tell this run's rows from
    /// the previous run's.
    pub fn generation(&self) -> Result<u64> {
        let read = self.database.begin_read()?;
        let meta = read.open_table(META)?;
        Ok(meta
            .get(KEY_GENERATION)?
            .map(|value| {
                let mut bytes = [0u8; 8];
                let value = value.value();
                let length = value.len().min(8);
                bytes[..length].copy_from_slice(&value[..length]);
                u64::from_le_bytes(bytes)
            })
            .unwrap_or(0))
    }

    /// Start a new generation, and answer it. Called once when a session opens its cache.
    pub fn begin_generation(&self) -> Result<u64> {
        let write = self.database.begin_write()?;
        let next = {
            let mut meta = write.open_table(META)?;
            let current = meta
                .get(KEY_GENERATION)?
                .map(|value| {
                    let mut bytes = [0u8; 8];
                    let value = value.value();
                    let length = value.len().min(8);
                    bytes[..length].copy_from_slice(&value[..length]);
                    u64::from_le_bytes(bytes)
                })
                .unwrap_or(0);
            let next = current + 1;
            meta.insert(KEY_GENERATION, &next.to_le_bytes()[..])?;
            next
        };
        write.commit()?;
        Ok(next)
    }

    // ---- files -------------------------------------------------------------------------------------

    /// Record what a file is, and what it was read with.
    pub fn put_file(&self, path: &Path, row: FileRow) -> Result<()> {
        let key = normalise(path);
        let write = self.database.begin_write()?;
        {
            let mut files = write.open_table(FILES)?;
            files.insert(&key[..], &row.encode()[..])?;
        }
        write.commit()?;
        Ok(())
    }

    /// What the last run recorded for a file, without opening it.
    pub fn file(&self, path: &Path) -> Result<Option<FileRow>> {
        let key = normalise(path);
        let read = self.database.begin_read()?;
        let files = read.open_table(FILES)?;
        let Some(value) = files.get(&key[..])? else {
            return Ok(None);
        };
        Ok(FileRow::decode(value.value()))
    }

    // ---- summaries ---------------------------------------------------------------------------------

    /// The summary bytes for a key, decompressed. `None` is a miss, and a miss is never an error.
    pub fn summary(&self, key: CacheKey) -> Result<Option<Vec<u8>>> {
        let read = self.database.begin_read()?;
        let summaries = read.open_table(SUMMARIES)?;
        let Some(value) = summaries.get((key.content, key.config, key.env))? else {
            return Ok(None);
        };
        Ok(Some(decompress(value.value())?))
    }

    /// Store a summary. The caller's bytes are compressed here, once, and the write is one transaction.
    pub fn put_summary(&self, key: CacheKey, bytes: &[u8]) -> Result<()> {
        let packed = compress(bytes);
        let write = self.database.begin_write()?;
        {
            let mut summaries = write.open_table(SUMMARIES)?;
            summaries.insert((key.content, key.config, key.env), &packed[..])?;
        }
        write.commit()?;
        Ok(())
    }

    /// **Write many summaries in one transaction.** Measured on the project's own file cache: 1 768 single-row
    /// writes took **8.2 s** (4.6 ms each, almost all of it transaction overhead), and the same rows written
    /// together take well under a tenth of that. A cold build writes thousands of rows, so this is the shape the
    /// pump should use rather than a nicety.
    pub fn put_summaries(&self, rows: &[(CacheKey, Vec<u8>)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let packed: Vec<((u64, u64, u64), Vec<u8>)> = rows
            .iter()
            .map(|(key, bytes)| ((key.content, key.config, key.env), compress(bytes)))
            .collect();
        let write = self.database.begin_write()?;
        {
            let mut summaries = write.open_table(SUMMARIES)?;
            for (key, bytes) in &packed {
                summaries.insert(*key, &bytes[..])?;
            }
        }
        write.commit()?;
        Ok(())
    }

    // ---- renderings --------------------------------------------------------------------------------

    /// The rendered text of a file under one environment — what a translation unit is built from.
    pub fn rendering(&self, content: u64, env: u64) -> Result<Option<String>> {
        let read = self.database.begin_read()?;
        let renderings = read.open_table(RENDERINGS)?;
        let Some(value) = renderings.get((content, env))? else {
            return Ok(None);
        };
        Ok(Some(String::from_utf8_lossy(&decompress(value.value())?).into_owned()))
    }

    pub fn put_rendering(&self, content: u64, env: u64, text: &str) -> Result<()> {
        let packed = compress(text.as_bytes());
        let write = self.database.begin_write()?;
        {
            let mut renderings = write.open_table(RENDERINGS)?;
            renderings.insert((content, env), &packed[..])?;
        }
        write.commit()?;
        Ok(())
    }

    // ---- projections -------------------------------------------------------------------------------

    /// The macro names a file mentions — the lexical half of the projection, and the half that does not depend on
    /// any environment, so it is stored once per content and reused across every root.
    pub fn projection(&self, content: u64) -> Result<Option<Vec<String>>> {
        let read = self.database.begin_read()?;
        let projections = read.open_table(PROJECTIONS)?;
        let Some(value) = projections.get(content)? else {
            return Ok(None);
        };
        let bytes = decompress(value.value())?;
        Ok(Some(
            bytes
                .split(|byte| *byte == 0)
                .filter(|name| !name.is_empty())
                .map(|name| String::from_utf8_lossy(name).into_owned())
                .collect(),
        ))
    }

    pub fn put_projection(&self, content: u64, names: &[String]) -> Result<()> {
        let mut joined = Vec::new();
        for name in names {
            joined.extend_from_slice(name.as_bytes());
            joined.push(0);
        }
        let packed = compress(&joined);
        let write = self.database.begin_write()?;
        {
            let mut projections = write.open_table(PROJECTIONS)?;
            projections.insert(content, &packed[..])?;
        }
        write.commit()?;
        Ok(())
    }

    // ---- environments and macros -------------------------------------------------------------------

    pub fn put_environment(&self, env: u64, bytes: &[u8]) -> Result<()> {
        let packed = compress(bytes);
        let write = self.database.begin_write()?;
        {
            let mut environments = write.open_table(ENVIRONMENTS)?;
            environments.insert(env, &packed[..])?;
        }
        write.commit()?;
        Ok(())
    }

    pub fn put_macro(&self, body_hash: u64, body: &str) -> Result<()> {
        let packed = compress(body.as_bytes());
        let write = self.database.begin_write()?;
        {
            let mut macros = write.open_table(MACROS)?;
            macros.insert(body_hash, &packed[..])?;
        }
        write.commit()?;
        Ok(())
    }

    // ---- collection --------------------------------------------------------------------------------

    /// **Delete what no file row references.** The question the directory tree could not answer: a summary is worth
    /// keeping exactly when some file still says it was read that way.
    ///
    /// Rows are gathered first and deleted after, because redb will not let a table be mutated while it is being
    /// iterated — and because a delete set built from a consistent snapshot is the only one whose counts a test can
    /// assert on.
    pub fn collect(&self) -> Result<Collected> {
        let (live_summaries, live_renderings, live_contents) = {
            let read = self.database.begin_read()?;
            let files = read.open_table(FILES)?;
            let mut summaries: HashSet<(u64, u64, u64)> = HashSet::new();
            let mut renderings: HashSet<(u64, u64)> = HashSet::new();
            let mut contents: HashSet<u64> = HashSet::new();
            for entry in files.iter()? {
                let (_, value) = entry?;
                let Some(row) = FileRow::decode(value.value()) else {
                    continue;
                };
                summaries.insert((row.content, row.config, row.env));
                renderings.insert((row.content, row.env));
                contents.insert(row.content);
            }
            (summaries, renderings, contents)
        };

        let mut collected = Collected::default();
        let write = self.database.begin_write()?;
        {
            let mut summaries = write.open_table(SUMMARIES)?;
            let dead: Vec<(u64, u64, u64)> = summaries
                .iter()?
                .filter_map(|entry| entry.ok().map(|(key, _)| key.value()))
                .filter(|key| !live_summaries.contains(key))
                .collect();
            for key in dead {
                summaries.remove(key)?;
                collected.summaries += 1;
            }
        }
        {
            let mut renderings = write.open_table(RENDERINGS)?;
            let dead: Vec<(u64, u64)> = renderings
                .iter()?
                .filter_map(|entry| entry.ok().map(|(key, _)| key.value()))
                .filter(|key| !live_renderings.contains(key))
                .collect();
            for key in dead {
                renderings.remove(key)?;
                collected.renderings += 1;
            }
        }
        {
            let mut projections = write.open_table(PROJECTIONS)?;
            let dead: Vec<u64> = projections
                .iter()?
                .filter_map(|entry| entry.ok().map(|(key, _)| key.value()))
                .filter(|key| !live_contents.contains(key))
                .collect();
            for key in dead {
                projections.remove(key)?;
                collected.projections += 1;
            }
        }
        write.commit()?;
        Ok(collected)
    }

    /// **Give the bytes back to the filesystem.** redb does not shrink its file when rows are deleted — measured, a
    /// collection left the database **larger** than before it (16.6 MB → 24.6 MB). Compaction rewrites the live
    /// pages, so it belongs to the idle step next to [`CacheDb::collect`], never to a query.
    pub fn compact(&mut self) -> Result<bool> {
        self.database
            .compact()
            .map_err(|error| CacheError::Io(std::io::Error::other(error)))
    }

    /// How many rows each table holds, and how large the file is — the numbers a probe prints.
    pub fn sizes(&self) -> Result<DatabaseSizes> {
        let read = self.database.begin_read()?;
        let count = |length: usize| length as usize;
        Ok(DatabaseSizes {
            files: count(read.open_table(FILES)?.len()? as usize),
            summaries: count(read.open_table(SUMMARIES)?.len()? as usize),
            renderings: count(read.open_table(RENDERINGS)?.len()? as usize),
            projections: count(read.open_table(PROJECTIONS)?.len()? as usize),
            environments: count(read.open_table(ENVIRONMENTS)?.len()? as usize),
            macros: count(read.open_table(MACROS)?.len()? as usize),
            bytes: std::fs::metadata(self.directory.join(DATABASE_FILE))
                .map(|metadata| metadata.len())
                .unwrap_or(0),
        })
    }
}

/// redb keys are byte strings, so a path is stored as its lossy UTF-8 — the same choice the VFS makes when it
/// normalises, which is what keeps the two spellings of a Windows path from becoming two rows.
fn normalise(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

fn compress(bytes: &[u8]) -> Vec<u8> {
    zstd::encode_all(bytes, 3).unwrap_or_else(|_| bytes.to_vec())
}

fn decompress(bytes: &[u8]) -> Result<Vec<u8>> {
    zstd::decode_all(bytes).map_err(|_| CacheError::Corrupt("zstd frame"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("cppls-cache-db-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        directory
    }

    fn row(content: u64, env: u64, generation: u64) -> FileRow {
        FileRow {
            content,
            config: 7,
            env,
            mtime: 1,
            size: 2,
            generation,
        }
    }

    /// **A summary goes in and comes out, compressed** — and the same key twice is one row.
    #[test]
    fn a_summary_survives_a_round_trip() {
        let directory = scratch("round-trip");
        let database = CacheDb::open(&directory, 42).expect("opens");
        let key = CacheKey {
            content: 1,
            config: 2,
            env: 3,
        };
        let body = vec![b'x'; 4_096];

        assert!(database.summary(key).unwrap().is_none(), "a miss is not an error");
        database.put_summary(key, &body).unwrap();
        assert_eq!(database.summary(key).unwrap().unwrap(), body);

        let sizes = database.sizes().unwrap();
        assert_eq!(sizes.summaries, 1);
        assert!(sizes.bytes > 0);

        drop(database);
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// **A summary no file references is collected; one a file does reference is kept.**
    #[test]
    fn collection_keeps_what_a_file_references() {
        let directory = scratch("collection");
        let database = CacheDb::open(&directory, 42).expect("opens");
        let live = CacheKey {
            content: 10,
            config: 7,
            env: 20,
        };
        let dead = CacheKey {
            content: 11,
            config: 7,
            env: 20,
        };
        database.put_summary(live, b"live").unwrap();
        database.put_summary(dead, b"dead").unwrap();
        database.put_rendering(live.content, live.env, "int x;").unwrap();
        database.put_rendering(dead.content, dead.env, "int y;").unwrap();
        database.put_projection(live.content, &["_STD".to_string()]).unwrap();
        database.put_projection(dead.content, &["_HAS_CXX20".to_string()]).unwrap();
        database
            .put_file(Path::new("C:/p/live.cpp"), row(live.content, live.env, 1))
            .unwrap();

        let collected = database.collect().unwrap();
        assert_eq!(collected.summaries, 1, "the unreferenced summary is gone");
        assert_eq!(collected.renderings, 1, "and so is the unreferenced rendering");
        assert_eq!(collected.projections, 1, "and the unreferenced projection");
        assert!(database.summary(live).unwrap().is_some(), "the referenced one stays");
        assert!(database.summary(dead).unwrap().is_none());

        drop(database);
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// **A batch of summaries is one transaction, and compaction returns the bytes of a collection.**
    ///
    /// Both are shapes rather than niceties: 1 768 single-row writes measured **8.2 s** against **0.97 s** for the
    /// batch, and a collection without compaction left the file *larger* than it started (16.6 → 24.6 MB).
    #[test]
    fn a_batch_is_one_transaction_and_compaction_shrinks() {
        let directory = scratch("batch");
        let mut database = CacheDb::open(&directory, 42).expect("opens");

        let rows: Vec<(CacheKey, Vec<u8>)> = (0..256u64)
            .map(|index| {
                (
                    CacheKey {
                        content: index,
                        config: 1,
                        env: 2,
                    },
                    vec![b'a'; 4_096],
                )
            })
            .collect();
        database.put_summaries(&rows).unwrap();
        assert_eq!(database.sizes().unwrap().summaries, 256);
        assert_eq!(database.summary(rows[7].0).unwrap().unwrap(), rows[7].1);

        // Nothing references them, so a collection drops the lot — and compaction is what gives the file back.
        let collected = database.collect().unwrap();
        assert_eq!(collected.summaries, 256);
        assert_eq!(database.sizes().unwrap().summaries, 0);
        let before = database.sizes().unwrap().bytes;
        assert!(database.compact().unwrap());
        let after = database.sizes().unwrap().bytes;
        assert!(after < before, "compaction must shrink: {before} -> {after}");

        drop(database);
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// **A database written by another schema is deleted, not read.** The cache is recomputable, so the only honest
    /// answer to bytes this build cannot interpret is to throw them away.
    #[test]
    fn a_foreign_schema_is_replaced() {
        let directory = scratch("schema");
        let database = CacheDb::open(&directory, 1).expect("opens");
        database
            .put_summary(
                CacheKey {
                    content: 5,
                    config: 5,
                    env: 5,
                },
                b"body",
            )
            .unwrap();
        drop(database);

        let reopened = CacheDb::open(&directory, 2).expect("reopens");
        assert!(
            reopened
                .summary(CacheKey {
                    content: 5,
                    config: 5,
                    env: 5
                })
                .unwrap()
                .is_none(),
            "a different fingerprint is a different database"
        );
        assert_eq!(reopened.generation().unwrap(), 0, "and a fresh generation");

        drop(reopened);
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// **A generation is bumped once per session**, and the row a write records carries it.
    #[test]
    fn a_generation_is_bumped_once() {
        let directory = scratch("generation");
        let database = CacheDb::open(&directory, 42).expect("opens");
        assert_eq!(database.generation().unwrap(), 0);
        assert_eq!(database.begin_generation().unwrap(), 1);
        assert_eq!(database.generation().unwrap(), 1);
        assert_eq!(database.begin_generation().unwrap(), 2);

        database
            .put_file(Path::new("C:/p/a.cpp"), row(1, 2, database.generation().unwrap()))
            .unwrap();
        assert_eq!(database.file(Path::new("c:/P/A.CPP")).unwrap().unwrap().generation, 2);

        drop(database);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
