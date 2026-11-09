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

const CAPACITY: usize = 256;
const BATCH_DELAY: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct NativeFileWatcher {
    state: Mutex<Option<WatcherState>>,
}

struct WatcherState {
    _watcher: RecommendedWatcher,
    task: tokio::task::JoinHandle<()>,
    root: PathBuf,
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
    ) -> notify::Result<()> {
        let mut current = self.state.lock().await;
        if let Some(state) = current.as_ref()
            && state.root == root
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

        let task = tokio::spawn(watch_events(context, receiver, overflow, filter.clone()));
        let previous = current.replace(WatcherState {
            _watcher: watcher,
            task,
            root: root.clone(),
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
    while let Some(first) = receiver.recv().await {
        tokio::time::sleep(BATCH_DELAY).await;
        let mut events = vec![first];
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }

        let mut changes = Vec::new();
        let mut rescan = overflow.swap(false, Ordering::AcqRel);
        if rescan {
            log::warn!("native file watcher queue overflowed; the workspace will be rescanned");
        }
        {
            let filter = filter
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for event in events {
                match event {
                    Ok(event) => collect_event(event, &filter, &mut changes, &mut rescan),
                    Err(error) => {
                        log::error!("native file watcher error: {error}");
                        rescan = true;
                    }
                }
            }
        }

        if !changes.is_empty() {
            context
                .enqueue(UpdateEvent::WatchedFilesChanged(
                    DidChangeWatchedFilesParams { changes },
                ))
                .await;
        }
        if rescan {
            context
                .workspace_manager()
                .lock()
                .await
                .add_rescan_task(context.clone())
                .await;
        }
    }
}

fn collect_event(
    event: Event,
    filter: &WatchFilter,
    changes: &mut Vec<FileEvent>,
    rescan: &mut bool,
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
        let ambiguous_removed_directory = kind == FileChangeType::DELETED
            && path.extension().is_none()
            && !matches!(event.kind, EventKind::Remove(RemoveKind::File));
        let directory_mutation = path.is_dir()
            && matches!(
                event.kind,
                EventKind::Create(_)
                    | EventKind::Remove(_)
                    | EventKind::Modify(ModifyKind::Name(_))
            );
        if folder_kind || directory_mutation || ambiguous_removed_directory {
            *rescan = true;
            continue;
        }
        if path.is_dir() {
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
        collect_event(
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                .add_path(old.clone())
                .add_path(new.clone()),
            &filter,
            &mut changes,
            &mut rescan,
        );
        collect_event(
            Event::new(EventKind::Create(CreateKind::File))
                .add_path(root.join("ignored").join("ignored.h")),
            &filter,
            &mut changes,
            &mut rescan,
        );
        assert_eq!(
            changes,
            vec![
                FileEvent::new(path_to_uri(&old).unwrap(), FileChangeType::DELETED),
                FileEvent::new(path_to_uri(&new).unwrap(), FileChangeType::CREATED),
            ]
        );
        assert!(!rescan);
    }

    #[test]
    fn lost_events_and_directory_changes_trigger_a_rescan() {
        let root = std::env::temp_dir().join("cpp-ls-native-watch-test");
        let filter = WatchFilter::new(&root);
        for event in [
            Event::new(EventKind::Any).set_flag(Flag::Rescan),
            Event::new(EventKind::Create(CreateKind::Folder)).add_path(root.join("new-dir")),
            Event::new(EventKind::Remove(RemoveKind::Any)).add_path(root.join("old-dir")),
        ] {
            let mut changes = Vec::new();
            let mut rescan = false;
            collect_event(event, &filter, &mut changes, &mut rescan);
            assert!(rescan);
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
        );
        let context = server.snapshot();
        let watcher = NativeFileWatcher::default();
        watcher
            .start(context.clone(), root.clone(), WatchFilter::new(&root))
            .await
            .unwrap();
        let first = watcher.state.lock().await.as_ref().unwrap().task.id();
        let excluded = root.join("excluded");
        watcher
            .start(
                context.clone(),
                root.clone(),
                WatchFilter::new(&root).ignore(excluded.clone()),
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
            .start(context, root.clone(), WatchFilter::new(&root))
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
