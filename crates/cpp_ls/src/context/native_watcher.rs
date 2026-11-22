use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use cpp_code_analysis::WatchFilter;
use lsp_types::{DidChangeWatchedFilesParams, FileChangeType, FileEvent};
use notify::event::{CreateKind, MetadataKind, ModifyKind, RemoveKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{Mutex, mpsc};

use super::{ServerContextSnapshot, UpdateEvent};
use crate::util::path_to_uri;

/// **How many events may wait for the batcher** — and the number the storm was made of.
///
/// At 256, `inotify` delivers a kernel tree's churn faster than one 100 ms batch drains it: measured on a Linux
/// kernel checkout, **332 overflow reports in fifty seconds**, each of which used to re-open the whole workspace. The
/// queue is now only a buffer in front of a batcher that coalesces lost events, so it is sized for the bursts a
/// `make` produces rather than for a quiet editor — a few hundred kilobytes of `PathBuf`, drained one `try_recv` per
/// event.
const CAPACITY: usize = 8_192;
const BATCH_DELAY: Duration = Duration::from_millis(100);

/// **How often a lost-event report may turn into a reconcile.** A storm reports hundreds; asking the disk once per
/// window answers all of them, and two seconds is short enough that a build's output is seen — and long enough that
/// the reconcile (a `stat` per project file and a walk) is never the thing that burns a core.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct NativeFileWatcher {
    state: Mutex<Option<WatcherState>>,
}

struct WatcherState {
    _watcher: RecommendedWatcher,
    task: tokio::task::JoinHandle<()>,
    root: PathBuf,
    /// See [`NativeFileWatcher::start`]'s `also`.
    also: Vec<PathBuf>,
    filter: Arc<RwLock<WatchFilter>>,
    faulted: Arc<AtomicBool>,
}

impl Drop for WatcherState {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl NativeFileWatcher {
    pub async fn start(
        &self,
        context: ServerContextSnapshot,
        root: PathBuf,
        filter: WatchFilter,
        // **Files outside the root that still configure the project** — a `--config` a caller pointed elsewhere. A
        // recursive watch of the root cannot see one, and a configuration that is only read when the session opens is
        // exactly what this watcher exists to notice changing (`docs/language-server-review.md` P1-5).
        also: Vec<PathBuf>,
    ) -> notify::Result<()> {
        let mut current = self.state.lock().await;
        if let Some(state) = current.as_ref()
            && state.root == root
            && state.also == also
            && !state.faulted.load(Ordering::Acquire)
            && !state.task.is_finished()
        {
            *state
                .filter
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = filter;
            return Ok(());
        }

        let (sender, receiver) = mpsc::channel(CAPACITY);
        let overflow = Arc::new(AtomicBool::new(false));
        let faulted = Arc::new(AtomicBool::new(false));
        let filter = Arc::new(RwLock::new(filter));
        let callback_overflow = overflow.clone();
        let callback_faulted = faulted.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            if event.is_err() {
                callback_faulted.store(true, Ordering::Release);
            }
            if let Err(mpsc::error::TrySendError::Full(event)) = sender.try_send(event) {
                if let Err(error) = event {
                    log::error!("native file watcher error: {error}");
                }
                callback_overflow.store(true, Ordering::Release);
            }
        })?;
        watcher.watch(&root, RecursiveMode::Recursive)?;
        for path in &also {
            // One file, non-recursively: what a caller named is a configuration file, not a second workspace.
            if let Err(error) = watcher.watch(path, RecursiveMode::NonRecursive) {
                log::warn!("cannot watch {}: {error}", path.display());
            }
        }

        let task = tokio::spawn(watch_events(context, receiver, overflow, filter.clone()));
        let previous = current.replace(WatcherState {
            _watcher: watcher,
            task,
            root: root.clone(),
            also,
            filter,
            faulted,
        });
        drop(previous);
        log::info!("watching {} with the native file watcher", root.display());
        Ok(())
    }

    pub async fn stop(&self) {
        drop(self.state.lock().await.take());
    }
}

async fn watch_events(
    context: ServerContextSnapshot,
    mut receiver: mpsc::Receiver<notify::Result<Event>>,
    overflow: Arc<AtomicBool>,
    filter: Arc<RwLock<WatchFilter>>,
) {
    // **The coalescing of lost-event reports**, and the reason it is here rather than in the handler: this loop is
    // the one task that sees every batch, so one counter is enough — a storm reports hundreds of overflows and the
    // disk gets asked once per window. The window is short enough that a build's output is picked up within a couple
    // of seconds and long enough that a kernel tree's `make` cannot turn into a rescan loop.
    let mut last_reconcile: Option<std::time::Instant> = None;
    let mut folded: u32 = 0;
    // **When a report was folded, not dropped.** An overflow that arrives while the window is still open cannot be
    // answered by the reconcile that just ran — the files it was about may not have existed yet — and a report that is
    // simply discarded is a change nobody ever reads. So the folded report arms a timer: the loop waits for the
    // channel, but no longer than the window, and asks the disk again when the window closes and nothing arrived to
    // take the question over. (Measured on the storm fixture: the first reconcile saw 61 of 120 rewritten files,
    // because the rest were written while it was running.)
    let mut pending_at: Option<std::time::Instant> = None;
    // **Whether the next ask needs the project's file list read again**, which is the expensive half of a reconcile:
    // a directory that appeared, vanished or moved may have brought whole files with it. Lost events alone cannot add
    // a file to a database project, so the common report pays for the stamps only.
    let mut listing_pending = false;
    loop {
        // **The armed window, fired by the timeout rather than by an event.** Nothing else in this task has a timer, so
        // the wait below is where the window is closed.
        if let Some(at) = pending_at
            && std::time::Instant::now() >= at
        {
            pending_at = None;
            last_reconcile = Some(std::time::Instant::now());
            log::warn!(
                "{folded} lost-event report(s) folded since the last look; asking the disk what moved (no reload)"
            );
            folded = 0;
            context
                .enqueue(UpdateEvent::Reconcile {
                    listing: listing_pending,
                })
                .await;
            listing_pending = false;
        }

        let first = if let Some(at) = pending_at {
            let wait = at.saturating_duration_since(std::time::Instant::now());
            match tokio::time::timeout(wait, receiver.recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                // The window closed with nothing arriving: the reconcile above is this iteration's work.
                Err(_) => continue,
            }
        } else {
            match receiver.recv().await {
                Some(event) => event,
                None => break,
            }
        };
        tokio::time::sleep(BATCH_DELAY).await;
        let mut events = vec![first];
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }

        let mut changes = Vec::new();
        let mut rescan = overflow.swap(false, Ordering::AcqRel);
        if rescan {
            folded += 1;
        }
        let mut listing = false;
        {
            let filter = filter
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for event in events {
                match event {
                    Ok(event) => {
                        collect_event(event, &filter, &mut changes, &mut rescan, &mut listing)
                    }
                    Err(error) => {
                        log::error!("native file watcher error: {error}");
                        rescan = true;
                    }
                }
            }
        }
        listing_pending |= listing;

        if !changes.is_empty() {
            context
                .enqueue(UpdateEvent::WatchedFilesChanged(
                    DidChangeWatchedFilesParams { changes },
                ))
                .await;
        }
        if rescan {
            folded += 1;
            let now = std::time::Instant::now();
            let due =
                last_reconcile.is_none_or(|last| now.duration_since(last) >= RECONCILE_INTERVAL);
            if due {
                last_reconcile = Some(now);
                pending_at = None;
                // **`warn` once per window, not once per batch**: 332 of these arrived in fifty seconds on a kernel
                // tree, and a log that repeats itself is a log nobody reads. The number says how much was folded.
                log::warn!(
                    "{folded} lost-event report(s) since the last look; asking the disk what moved (no reload)"
                );
                folded = 0;
                context
                    .enqueue(UpdateEvent::Reconcile {
                        listing: listing_pending,
                    })
                    .await;
                listing_pending = false;
            } else {
                log::debug!("a reconcile just ran; the next window will answer this report");
                pending_at = last_reconcile.map(|last| last + RECONCILE_INTERVAL);
            }
        }
    }
}

fn collect_event(
    event: Event,
    filter: &WatchFilter,
    changes: &mut Vec<FileEvent>,
    rescan: &mut bool,
    listing: &mut bool,
) {
    if event.need_rescan() {
        log::warn!("the native file watcher reported lost events");
        *rescan = true;
    }
    if matches!(
        event.kind,
        EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime))
    ) {
        return;
    }
    if event.paths.is_empty() {
        *rescan = true;
        return;
    }

    let folder_kind = matches!(
        event.kind,
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
    );
    let renames = matches!(event.kind, EventKind::Modify(ModifyKind::Name(_)));
    let ambiguous_kind = matches!(
        event.kind,
        EventKind::Create(CreateKind::Any) | EventKind::Remove(RemoveKind::Any)
    ) || renames;
    for (index, path) in event.paths.iter().enumerate() {
        if filter.is_ignored(path) {
            continue;
        }
        let kind = match event.kind {
            EventKind::Create(_) => FileChangeType::CREATED,
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                FileChangeType::DELETED
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => FileChangeType::CREATED,
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if index == 0 => {
                FileChangeType::DELETED
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => FileChangeType::CREATED,
            _ => FileChangeType::CHANGED,
        };
        if folder_kind {
            // A directory appeared or went away: files may have arrived or vanished wholesale, and only a listing
            // can say which. That is the reconcile's job, not a change event's.
            *rescan = true;
            *listing = true;
            continue;
        }
        // **Whether this is a directory is asked only when the event's kind does not say** — a `stat` per event is
        // exactly the cost that made a kernel tree's churn expensive (`inotify` reports `Create(File)` and
        // `Create(Folder)` apart, so the ambiguous kinds are the rare ones). A directory that moved means the same
        // thing a created one does: the listing is out of date.
        if ambiguous_kind
            && (path.is_dir() || (kind == FileChangeType::DELETED && path.extension().is_none()))
        {
            *rescan = true;
            *listing = true;
            continue;
        }
        if let Some(uri) = path_to_uri(path) {
            changes.push(FileEvent::new(uri, kind));
        } else {
            log::warn!("cannot turn watched path {} into a URI", path.display());
            *rescan = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::Flag;

    #[test]
    fn file_renames_keep_both_halves_and_excluded_paths_are_ignored() {
        let root = std::env::temp_dir().join("cpp-ls-native-watch-test");
        let filter = WatchFilter::new(&root).ignore(root.join("ignored"));
        let old = root.join("old.h");
        let new = root.join("new.h");
        let mut changes = Vec::new();
        let mut rescan = false;
        let mut listing = false;
        collect_event(
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                .add_path(old.clone())
                .add_path(new.clone()),
            &filter,
            &mut changes,
            &mut rescan,
            &mut listing,
        );
        collect_event(
            Event::new(EventKind::Create(CreateKind::File))
                .add_path(root.join("ignored").join("ignored.h")),
            &filter,
            &mut changes,
            &mut rescan,
            &mut listing,
        );
        assert_eq!(
            changes,
            vec![
                FileEvent::new(path_to_uri(&old).unwrap(), FileChangeType::DELETED),
                FileEvent::new(path_to_uri(&new).unwrap(), FileChangeType::CREATED),
            ]
        );
        // **A file's rename is changes, not a listing**: both halves are named, so no walk is needed. A *directory*
        // rename is the case that cannot name its children, and that one asks for the listing.
        assert!(
            !rescan,
            "both halves are named, so the change events carry the whole answer"
        );
        assert!(!listing, "and the project list is still right");
    }

    #[test]
    fn lost_events_and_directory_changes_trigger_a_rescan() {
        let root = std::env::temp_dir().join("cpp-ls-native-watch-test");
        let filter = WatchFilter::new(&root);
        // **Lost events and directory changes are both reconciles, but only one needs the listing.** A lost-event
        // report is about files that moved — their names are already in the project — while a directory that appeared
        // or went away may have brought whole files with it, which only a listing can find.
        for (event, wanted_listing) in [
            (Event::new(EventKind::Any).set_flag(Flag::Rescan), false),
            (
                Event::new(EventKind::Create(CreateKind::Folder)).add_path(root.join("new-dir")),
                true,
            ),
            (
                Event::new(EventKind::Remove(RemoveKind::Any)).add_path(root.join("old-dir")),
                true,
            ),
        ] {
            let mut changes = Vec::new();
            let mut rescan = false;
            let mut listing = false;
            collect_event(event, &filter, &mut changes, &mut rescan, &mut listing);
            assert!(rescan);
            assert_eq!(listing, wanted_listing);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reloading_the_same_root_keeps_the_watcher_and_updates_its_filter() {
        use lsp_server::Connection;
        use lsp_types::ClientCapabilities;
        use std::sync::atomic::AtomicUsize;

        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "cpp-ls-watch-reuse-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let (connection, _client) = Connection::memory();
        let server = crate::context::ServerContext::new(
            Arc::new(connection),
            ClientCapabilities::default(),
            crate::context::ClientId::Other,
            None,
        );
        let context = server.snapshot();
        let watcher = NativeFileWatcher::default();
        watcher
            .start(
                context.clone(),
                root.clone(),
                WatchFilter::new(&root),
                Vec::new(),
            )
            .await
            .unwrap();
        let first = watcher.state.lock().await.as_ref().unwrap().task.id();
        let excluded = root.join("excluded");
        watcher
            .start(
                context.clone(),
                root.clone(),
                WatchFilter::new(&root).ignore(excluded.clone()),
                Vec::new(),
            )
            .await
            .unwrap();
        {
            let state = watcher.state.lock().await;
            let state = state.as_ref().unwrap();
            assert_eq!(state.task.id(), first);
            assert!(state.filter.read().unwrap().is_ignored(&excluded));
            state.task.abort();
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if watcher
                    .state
                    .lock()
                    .await
                    .as_ref()
                    .unwrap()
                    .task
                    .is_finished()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        watcher
            .start(context, root.clone(), WatchFilter::new(&root), Vec::new())
            .await
            .unwrap();
        assert_ne!(
            watcher.state.lock().await.as_ref().unwrap().task.id(),
            first
        );
        watcher.stop().await;
        server.close().await;
        std::fs::remove_dir(&root).unwrap();
    }
}
