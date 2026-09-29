//! The name index — declarations and macro definitions, **inverted** from "per file" to "per name".
//!
//! # Why this exists
//!
//! [`ProjectIndex`](super::ProjectIndex) stores what each file *says*: a list of declarations per file. Every
//! question a language server asks is the other way round — "which files declare `Widget`", "what does `ns::C`
//! hold", "who defines this macro". Answering those from the per-file lists is a scan of the whole project, and
//! the scan grows with the project instead of with the answer: on a large workspace one keystroke in the
//! workspace-symbol box walked every declaration of every file, allocating two strings for each.
//!
//! This is the table that turns the question around. It is **derived** from the summaries and the cooked readings
//! and never authoritative: [`ProjectIndex`](super::ProjectIndex) is the only writer, it updates the table in the
//! same call that changes a summary, and a posting only *points* at a fact (`file`, `slot`), so the fact itself is
//! read from where it lives and can never disagree with the table about what it says.
//!
//! # What is keyed
//!
//! ```text
//! by_name    bare name       → the facts that declare it        (locals are left out, as every name query does)
//! by_scope   qualified scope → the facts written directly in it (what a member list is made of)
//! definers   macro name      → the files that `#define` it
//! sorted     lowercased name → the spellings it has           (what a workspace search walks, in order)
//! ```
//!
//! # Postings are ordered, and that is a contract
//!
//! A posting names a **file by its sequence number** — the number the index gave the file when it first appeared,
//! never reused — and a list is sorted by `(file, slot)`. So a query that walks a list visits files in insertion
//! order, which is the order the per-file scan visited them in, and two runs over one project answer in one order.
//! Within a file the raw reading's facts come before the cooked reading's (the high bit of the slot), because the
//! cooked reading is deduplicated *against* the raw one and needs it first.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Bound;

use crate::summary::{DeclFact, MacroFact};

/// The high bit of a slot: this posting points into the file's cooked reading rather than its own summary.
const COOKED: u32 = 1 << 31;

/// Where one fact lives: which file (by sequence number) and which entry of which list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Posting {
    pub file: u32,
    slot: u32,
}

impl Posting {
    fn new(file: u32, index: usize, cooked: bool) -> Self {
        debug_assert!(index < COOKED as usize, "a file with two billion declarations");
        let slot = index as u32 | if cooked { COOKED } else { 0 };
        Posting { file, slot }
    }

    /// Does this point into the cooked reading?
    pub fn is_cooked(self) -> bool {
        self.slot & COOKED != 0
    }

    /// The position in the list it points into.
    pub fn index(self) -> usize {
        (self.slot & !COOKED) as usize
    }
}

#[derive(Debug, Default)]
pub(super) struct NameIndex {
    by_name: HashMap<Box<str>, Vec<Posting>>,
    by_scope: HashMap<Box<str>, Vec<Posting>>,
    definers: HashMap<Box<str>, Vec<u32>>,
    /// The distinct names, keyed by their lowercase form and **in that order**. A symbol search is case-insensitive
    /// and reports alphabetically, so walking this map is walking the answer: the search stops after the first
    /// `limit` hits instead of ranking every name in the project. Kept in step with `by_name` — a key is here
    /// exactly while its posting list is.
    sorted: BTreeMap<Box<str>, Vec<Box<str>>>,
}

impl NameIndex {
    /// Add the facts of one file's reading — its own summary (`cooked: false`) or its cooked reading.
    pub fn add_declarations(&mut self, file: u32, cooked: bool, facts: &[DeclFact]) {
        for (index, fact) in facts.iter().enumerate() {
            let posting = Posting::new(file, index, cooked);

            // A local names nothing another file can see, so it is not indexed by name — `matches` in the project
            // index refuses one for the same reason. A scope is different: the scope of a local is `None`, so there
            // is no scope to key it under and nothing to leave out.
            if !fact.local {
                self.add_name(&fact.name, posting);
            }
            if let Some(scope) = &fact.scope {
                insert_sorted(&mut self.by_scope, scope, posting);
            }
        }
    }

    fn add_name(&mut self, name: &str, posting: Posting) {
        let is_new = !self.by_name.contains_key(name);
        insert_sorted(&mut self.by_name, name, posting);

        if is_new {
            self.sorted.entry(lowercase(name).into_boxed_str()).or_default().push(Box::from(name));
        }
    }

    fn drop_name(&mut self, name: &str, file: u32, cooked: bool) {
        if !remove_range(&mut self.by_name, name, file, cooked) {
            return;
        }

        let lowered = lowercase(name);
        if let Some(spellings) = self.sorted.get_mut(lowered.as_str()) {
            spellings.retain(|spelling| &**spelling != name);
            if spellings.is_empty() {
                self.sorted.remove(lowered.as_str());
            }
        }
    }

    /// Take back what [`NameIndex::add_declarations`] added for the same file and reading.
    ///
    /// Takes the facts rather than a file number alone because that is what makes it cheap: the keys to visit are
    /// exactly the names in the list being dropped, so an edit to one file touches the lists of the names *it*
    /// declared and not every list in the project.
    pub fn remove_declarations(&mut self, file: u32, cooked: bool, facts: &[DeclFact]) {
        let mut names: HashSet<&str> = HashSet::new();
        let mut scopes: HashSet<&str> = HashSet::new();

        for fact in facts {
            if !fact.local {
                names.insert(fact.name.as_str());
            }
            if let Some(scope) = &fact.scope {
                scopes.insert(scope.as_str());
            }
        }

        for name in names {
            self.drop_name(name, file, cooked);
        }
        for scope in scopes {
            let _ = remove_range(&mut self.by_scope, scope, file, cooked);
        }
    }

    /// Record which macros a file defines.
    pub fn add_macros(&mut self, file: u32, macros: &[MacroFact]) {
        for fact in macros.iter().filter(|fact| fact.kind.is_definition()) {
            let files = match self.definers.get_mut(fact.name.as_str()) {
                Some(files) => files,
                None => self.definers.entry(Box::from(fact.name.as_str())).or_default(),
            };

            match files.binary_search(&file) {
                Ok(_) => {}
                Err(at) => files.insert(at, file),
            }
        }
    }

    /// Take back what [`NameIndex::add_macros`] added for this file.
    pub fn remove_macros(&mut self, file: u32, macros: &[MacroFact]) {
        for name in macros
            .iter()
            .filter(|fact| fact.kind.is_definition())
            .map(|fact| fact.name.as_str())
            .collect::<HashSet<_>>()
        {
            let Some(files) = self.definers.get_mut(name) else {
                continue;
            };
            if let Ok(at) = files.binary_search(&file) {
                files.remove(at);
            }
            if files.is_empty() {
                self.definers.remove(name);
            }
        }
    }

    /// Every fact whose bare name is `name`, in `(file, slot)` order.
    pub fn named(&self, name: &str) -> &[Posting] {
        self.by_name.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every fact written directly in the scope `scope`, in `(file, slot)` order.
    pub fn scoped(&self, scope: &str) -> &[Posting] {
        self.by_scope.get(scope).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The files that define the macro `name`, by sequence number, ascending.
    pub fn definers_of(&self, name: &str) -> &[u32] {
        self.definers.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The spellings whose lowercase form is exactly `lowered`.
    pub fn spellings_of(&self, lowered: &str) -> Option<&[Box<str>]> {
        self.sorted.get(lowered).map(Vec::as_slice)
    }

    /// Every lowercase name from `lowered` on, in order, with its spellings.
    pub fn lowered_from<'a>(
        &'a self,
        lowered: &str,
    ) -> impl Iterator<Item = (&'a str, &'a [Box<str>])> + 'a {
        self.sorted
            .range::<str, _>((Bound::Included(lowered), Bound::Unbounded))
            .map(|(key, spellings)| (&**key, spellings.as_slice()))
    }

    /// How many distinct names are indexed — for the probes' tables.
    pub fn distinct_names(&self) -> usize {
        self.by_name.len()
    }
}

/// Insert keeping the list sorted. A file that is new has the largest sequence number, so the common case is a push.
fn insert_sorted(map: &mut HashMap<Box<str>, Vec<Posting>>, key: &str, posting: Posting) {
    let list = match map.get_mut(key) {
        Some(list) => list,
        None => map.entry(Box::from(key)).or_default(),
    };

    match list.last() {
        Some(last) if *last >= posting => {
            let at = list.partition_point(|held| *held < posting);
            list.insert(at, posting);
        }
        _ => list.push(posting),
    }
}

/// Drop one file's postings of one reading from `key`'s list — they are contiguous, so this is one `drain`.
///
/// Answers whether the list is now empty and was dropped.
fn remove_range(map: &mut HashMap<Box<str>, Vec<Posting>>, key: &str, file: u32, cooked: bool) -> bool {
    let Some(list) = map.get_mut(key) else {
        return false;
    };

    let start = list.partition_point(|held| held.file < file);
    let end = start + list[start..].partition_point(|held| held.file == file);
    // Raw slots sort before cooked ones, so the file's block is `[raw…, cooked…]`.
    let split = start + list[start..end].partition_point(|held| !held.is_cooked());
    let range = if cooked { split..end } else { start..split };

    list.drain(range);

    if list.is_empty() {
        map.remove(key);
        return true;
    }

    false
}

/// `text` lowercased — the form the sorted map is keyed by, and the form a search is compared in.
pub(super) fn lowercase(text: &str) -> String {
    if text.is_ascii() {
        text.to_ascii_lowercase()
    } else {
        text.to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::SummaryKey;
    use crate::index::summarize;
    use std::path::Path;

    fn facts_of(source: &str) -> Vec<DeclFact> {
        summarize(Path::new("/p/a.h"), source, SummaryKey::new(0, 0)).declarations
    }

    fn held(index: &NameIndex, name: &str) -> Vec<(u32, bool)> {
        index
            .named(name)
            .iter()
            .map(|posting| (posting.file, posting.is_cooked()))
            .collect()
    }

    #[test]
    fn a_list_stays_in_file_order_whatever_order_files_arrive_in() {
        let facts = facts_of("int x;\n");
        let mut index = NameIndex::default();
        index.add_declarations(3, false, &facts);
        index.add_declarations(1, false, &facts);
        index.add_declarations(2, true, &facts);
        index.add_declarations(2, false, &facts);

        assert_eq!(held(&index, "x"), vec![(1, false), (2, false), (2, true), (3, false)]);
    }

    #[test]
    fn removing_one_reading_of_a_file_leaves_the_other() {
        let facts = facts_of("namespace ns { int x; int y; }\n");
        let mut index = NameIndex::default();
        index.add_declarations(1, false, &facts);
        index.add_declarations(1, true, &facts);
        index.add_declarations(2, false, &facts);
        let scoped = index.scoped("ns").len();
        assert_eq!(scoped, 6, "two facts in the scope, three readings");

        index.remove_declarations(1, true, &facts);
        assert_eq!(held(&index, "x"), vec![(1, false), (2, false)]);
        assert_eq!(index.scoped("ns").len(), 4);

        index.remove_declarations(1, false, &facts);
        index.remove_declarations(2, false, &facts);
        assert!(index.named("x").is_empty());
        assert!(index.scoped("ns").is_empty());
        assert_eq!(index.distinct_names(), 0, "an emptied list is dropped, not kept as an empty entry");
    }

    #[test]
    fn a_local_has_no_name_entry() {
        let facts = facts_of("void f() { int inner; }\n");
        assert!(facts.iter().any(|fact| fact.name == "inner" && fact.local), "the fixture has a local");

        let mut index = NameIndex::default();
        index.add_declarations(1, false, &facts);

        assert!(index.named("inner").is_empty());
        assert_eq!(index.named("f").len(), 1);
    }

    #[test]
    fn a_posting_points_at_the_slot_it_was_made_for() {
        let facts = facts_of("int a;\nint b;\n");
        let cooked = facts_of("int b;\n");
        let mut index = NameIndex::default();
        index.add_declarations(7, false, &facts);
        index.add_declarations(7, true, &cooked);

        let b = index.named("b");
        assert_eq!((b[0].index(), b[0].is_cooked()), (1, false));
        assert_eq!((b[1].index(), b[1].is_cooked()), (0, true));
    }

    #[test]
    fn the_sorted_names_are_in_step_with_the_lists() {
        let upper = facts_of("int Size;\n");
        let lower = facts_of("int size;\n");
        let mut index = NameIndex::default();
        index.add_declarations(1, false, &upper);
        index.add_declarations(2, false, &lower);
        index.add_declarations(3, false, &lower);

        let spellings = |index: &NameIndex| -> Vec<String> {
            index
                .spellings_of("size")
                .map(|held| held.iter().map(|spelling| spelling.to_string()).collect())
                .unwrap_or_default()
        };
        assert_eq!(spellings(&index).len(), 2, "two spellings, one lowercase key: {:?}", spellings(&index));

        index.remove_declarations(2, false, &lower);
        assert_eq!(spellings(&index).len(), 2, "file 3 still declares `size`");

        index.remove_declarations(3, false, &lower);
        assert_eq!(spellings(&index), vec!["Size".to_string()]);

        index.remove_declarations(1, false, &upper);
        assert!(index.spellings_of("size").is_none());
        assert_eq!(index.lowered_from("").count(), 0);
    }

    #[test]
    fn a_walk_from_a_key_is_in_lowercase_order() {
        let mut index = NameIndex::default();
        for (file, source) in ["int banana;\n", "int Apple;\n", "int cherry;\n", "int apple2;\n"].iter().enumerate() {
            index.add_declarations(file as u32, false, &facts_of(source));
        }

        let keys: Vec<&str> = index.lowered_from("apple").map(|(key, _)| key).collect();
        assert_eq!(keys, vec!["apple", "apple2", "banana", "cherry"]);
    }

    #[test]
    fn the_definers_of_a_macro_are_the_files_that_define_it() {
        let define = summarize(Path::new("/p/a.h"), "#define M 1\n#define M 2\n", SummaryKey::new(0, 0));
        let use_only = summarize(Path::new("/p/b.h"), "#ifdef M\n#endif\n", SummaryKey::new(0, 0));

        let mut index = NameIndex::default();
        index.add_macros(5, &define.macros);
        index.add_macros(2, &define.macros);
        index.add_macros(9, &use_only.macros);
        assert_eq!(index.definers_of("M"), &[2, 5], "sorted, once per file, and a use is not a definition");

        index.remove_macros(5, &define.macros);
        assert_eq!(index.definers_of("M"), &[2]);
        index.remove_macros(2, &define.macros);
        assert!(index.definers_of("M").is_empty());
    }
}
