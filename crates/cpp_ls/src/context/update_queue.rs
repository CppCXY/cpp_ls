//! # The update queue — one writer, in the order the client sent things
//!
//! Every notification that changes the analysis goes through here:
//!
//! ```text
//! didOpen ───┐
//! didChange ─┼─▶ one queue ─▶ sync the buffer ─▶ session.did_open/did_change ─▶ schedule diagnostics
//! didClose ──┘
//! ```
//!
//! # Why one writer rather than one task per notification
//!
//! Because the order is the meaning. `didChange` then `didClose` is a file that was edited and closed; the same two
//! messages the other way round are a file that was closed (so the disk answers for it) and then edited. A runtime
//! that ran them concurrently could produce either, and the analysis holds one session with one `&mut` — which is
//! exactly the single-writer shape this queue gives it.
//!
//! # Why the handlers only enqueue — and why a request does not
//!
//! So that the loop reading from the client never waits for a parse. That much was right; what was missing is the
//! other half of the protocol's own model, and it failed in a way worth recording:
//!
//! ```text
//! client: didChange (text T2)          → queued
//! client: completion at a position in T2
//! server: the request is served first  → the analysis still holds T1
//!                                      → the position is past the end of T1's line, no node is there
//!                                      → the answer is `null`, or a completion of the wrong kind
//! ```
//!
//! Measured on this server, before the fix: a burst of 24 changes followed immediately by one completion answered
//! with the **file's global names** (`w`, `Widget`, `f`) instead of `Widget`'s members, because the cursor had been
//! translated against a 24-edits-old text. The client did nothing wrong: a request may assume the server has
//! processed everything it sent before it.
//!
//! A request waits until the single worker has applied the sequence current at dispatch. Requests also form
//! coalescing barriers; if a later change starts while a request is running, that request is cancelled with
//! `ContentModified` rather than being answered against a newer live session.
//!
//! # Who applies, and what "one writer" means here
//!
//! Only the worker applies notifications. Requests observe its completed sequence and never take ownership of an
//! in-progress update, so cancelling a waiter cannot drop the update.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lsp_types::{
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams,
};
use tokio::sync::{Mutex, Notify, watch};

use crate::handlers;

use super::snapshot::{ServerContextInner, ServerContextSnapshot};

/// How long the worker sleeps before looking at the queue again when nothing has woken it.
///
/// Insurance rather than the mechanism, the same way [`crate::context::AnalysisState::wait_for_work`] is: a wake-up
/// is stored, so an event queued while the worker was between the drain and the wait is not lost — but a stall
/// would leave the file's text unapplied for the rest of the session, and one look a second is a cheap way never to
/// find out.
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// One thing the client said, waiting its turn.
///
/// The names drop the protocol's `did` prefix: this is a queue of *what happened* to a document, and the protocol's
/// own spelling is already on the parameter type inside each variant.
#[derive(Debug)]
pub enum UpdateEvent {
    Opened(DidOpenTextDocumentParams),
    Changed(DidChangeTextDocumentParams),
    Saved(DidSaveTextDocumentParams),
    Closed(DidCloseTextDocumentParams),
    WatchedFilesChanged(DidChangeWatchedFilesParams),
}

impl UpdateEvent {
    fn kind(&self) -> &'static str {
        match self {
            Self::Opened(_) => "opened",
            Self::Changed(_) => "changed",
            Self::Saved(_) => "saved",
            Self::Closed(_) => "closed",
            Self::WatchedFilesChanged(_) => "watched files changed",
        }
    }

    fn document_uri(&self) -> Option<&lsp_types::Uri> {
        match self {
            Self::Opened(params) => Some(&params.text_document.uri),
            Self::Changed(params) => Some(&params.text_document.uri),
            Self::Saved(params) => Some(&params.text_document.uri),
            Self::Closed(params) => Some(&params.text_document.uri),
            Self::WatchedFilesChanged(_) => None,
        }
    }

    fn changes_analysis(&self) -> bool {
        match self {
            Self::Saved(params) => params.text.is_some(),
            Self::WatchedFilesChanged(params) => !params.changes.is_empty(),
            _ => true,
        }
    }

    /// Adjacent changes to one document can be combined in order, unless a request separates them.
    fn absorbs(&self, other: &UpdateEvent) -> bool {
        match (self, other) {
            (UpdateEvent::Changed(earlier), UpdateEvent::Changed(later)) => {
                earlier.text_document.uri == later.text_document.uri
                    && earlier.text_document.version < later.text_document.version
            }
            _ => false,
        }
    }

    fn absorb(&mut self, other: UpdateEvent) {
        match (self, other) {
            (UpdateEvent::Changed(earlier), UpdateEvent::Changed(later)) => {
                if let Some(full) = later
                    .content_changes
                    .iter()
                    .rposition(|change| change.range.is_none())
                {
                    earlier.text_document.version = later.text_document.version;
                    earlier.content_changes =
                        later.content_changes.into_iter().skip(full).collect();
                } else {
                    earlier.text_document.version = later.text_document.version;
                    earlier.content_changes.extend(later.content_changes);
                }
            }
            _ => unreachable!("only compatible document changes are merged"),
        }
    }
}

/// One event and where it sits in the client's stream.
struct QueuedUpdate {
    /// The number this event was given when it was queued, starting at `1`. See [`UpdateInbox::queued`].
    seq: u64,
    last_seq: u64,
    event: UpdateEvent,
}

/// **The client's pending notifications**, and the rule that one processor applies them at a time.
pub struct UpdateInbox {
    /// Oldest first, each with its sequence number. Behind the lock rather than behind a channel, because the
    /// bound a request applies up to has to be decided by *looking* at the front of the queue: a channel hands out
    /// whatever is next, and an event that belongs to a later request cannot be put back.
    ///
    /// **Held only for a push or a pop** — never across the applying of an event. That is measured rather than
    /// tidy: applying a `didChange` takes the session's write lock, which the index pump holds for a whole slice, so
    /// a burst used to wait ~90 ms *per message* on the previous message's apply and the message loop behind it —
    /// 24 changes cost the client **2.4 s**, and a completion asked for at the end of them waited for all of it.
    pending: Mutex<VecDeque<QueuedUpdate>>,
    /// **The single-writer rule**: held by whoever is applying events, for as long as that takes.
    applying: Mutex<()>,
    /// How many events have ever been queued — see [`UpdateInbox::queued`].
    queued: AtomicU64,
    /// The latest request boundary; changes may not be combined across it.
    request_boundary: AtomicU64,
    /// The last event whose application has completed.
    applied: watch::Sender<u64>,
    /// The last analysis-changing event whose application has begun.
    started_change: watch::Sender<u64>,
    /// "There is something to apply."
    work: Notify,
}

impl Default for UpdateInbox {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateInbox {
    pub fn new() -> Self {
        let (applied, _) = watch::channel(0);
        let (started_change, _) = watch::channel(0);
        UpdateInbox {
            pending: Mutex::new(VecDeque::new()),
            applying: Mutex::new(()),
            queued: AtomicU64::new(0),
            request_boundary: AtomicU64::new(0),
            applied,
            started_change,
            work: Notify::new(),
        }
    }

    /// Queue one event, and wake whoever applies them.
    ///
    /// **A run of changes to one document collapses into one piece of work** — see [`UpdateEvent::absorbs`] for the
    /// exact rule and why nothing else may collapse. The count still goes up for every message, because the count
    /// is what a request measures its own past with. A request boundary prevents merging a later change into the
    /// text that request is entitled to see.
    pub async fn push(&self, event: UpdateEvent) {
        let mut pending = self.pending.lock().await;
        let seq = self.queued.fetch_add(1, Ordering::SeqCst) + 1;
        let boundary = self.request_boundary.load(Ordering::SeqCst);
        match pending.back_mut() {
            Some(last) if last.seq > boundary && last.event.absorbs(&event) => {
                last.event.absorb(event);
                last.last_seq = seq;
            }
            _ => pending.push_back(QueuedUpdate {
                seq,
                last_seq: seq,
                event,
            }),
        }
        drop(pending);

        self.work.notify_one();
    }

    /// **How many events the client has sent so far** — the number a request records when it is dispatched.
    ///
    /// A request waits until the worker has applied at least this sequence; later changes cancel an in-flight request.
    pub fn queued(&self) -> u64 {
        self.queued.load(Ordering::SeqCst)
    }

    pub fn applied(&self) -> u64 {
        *self.applied.borrow()
    }

    /// Run a short synchronous action without letting a new event cross the applied-sequence check.
    pub(crate) async fn when_caught_up<T>(&self, action: impl FnOnce() -> T) -> Option<T> {
        let _pending = self.pending.lock().await;
        if self.queued() == self.applied() {
            Some(action())
        } else {
            None
        }
    }

    pub fn record_request(&self) -> u64 {
        let upto = self.queued();
        self.request_boundary.fetch_max(upto, Ordering::SeqCst);
        upto
    }

    pub async fn wait_until_applied(&self, upto: u64) {
        let mut applied = self.applied.subscribe();
        loop {
            if *applied.borrow_and_update() >= upto {
                return;
            }
            applied
                .changed()
                .await
                .expect("the update inbox remains alive while a request waits");
        }
    }

    pub fn started_changes(&self) -> watch::Receiver<u64> {
        self.started_change.subscribe()
    }

    /// Wait until there is something to apply, or `timeout` passes.
    pub async fn wait_for_work(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.work.notified())
            .await
            .is_ok()
    }

    /// Apply every queued event up to `upto`, in order, calling `apply` for each one.
    ///
    /// **One applier at a time**, which is what keeps the client's order: the queue is handed out oldest-first under
    /// `pending`, and `applying` is held for the whole run — so a request that finds the worker applying an event
    /// waits here, then applies what is left itself, and `didOpen` can never overtake the `didChange` that followed
    /// it. A *push* is not blocked by that, which is the point: the storage lock is taken per event and released
    /// before the event is applied, so a client that sends a burst is not waiting for the burst.
    ///
    /// Returns how many were applied, which is what the idle worker looks at to decide whether to sleep.
    pub async fn apply_upto<F, Fut>(&self, upto: u64, mut apply: F) -> usize
    where
        F: FnMut(UpdateEvent) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let started = std::time::Instant::now();
        let _applying = self.applying.lock().await;
        let waited = started.elapsed().as_millis();
        if waited > 50 {
            log::debug!("apply_upto: waited {waited} ms for the applier (upto {upto})");
        }
        let mut applied = 0;

        loop {
            let next = {
                let mut pending = self.pending.lock().await;
                match pending.front() {
                    Some(queued) if queued.seq <= upto => pending.pop_front(),
                    _ => None,
                }
            };

            let Some(queued) = next else {
                break;
            };

            if queued.event.changes_analysis() {
                self.started_change.send_replace(queued.last_seq);
            }
            log::debug!(
                "apply_upto: applying event {} ({}, document {:?})",
                queued.seq,
                queued.event.kind(),
                queued.event.document_uri()
            );
            let started = std::time::Instant::now();
            apply(queued.event).await;
            self.applied.send_replace(queued.last_seq);
            let took = started.elapsed().as_millis();
            if took > 50 {
                log::debug!("apply_upto: event {} took {took} ms", queued.seq);
            }
            applied += 1;
        }

        applied
    }
}

/// **Apply the client's pending notifications up to `upto`** — the whole of what a request waits for.
///
/// The timing is the diagnostic that was missing when a request looked stuck: a client waits here before its handler's
/// first line, so "the request never answered" and "the events took a second" were indistinguishable in the log. Only a
/// slow run is logged.
pub async fn catch_up_upto(snapshot: &ServerContextSnapshot, upto: u64) {
    let started = std::time::Instant::now();
    snapshot.inbox().wait_until_applied(upto).await;
    let took = started.elapsed().as_millis();
    if took > 50 {
        log::debug!("catch_up_upto: waited {took} ms for events through {upto}");
    }
}

/// Apply one event: the whole of what a notification does to the analysis.
async fn process(snapshot: &ServerContextSnapshot, event: UpdateEvent) {
    match event {
        UpdateEvent::Opened(params) => {
            handlers::process_did_open_text_document(snapshot.clone(), params).await;
        }
        UpdateEvent::Changed(params) => {
            handlers::process_did_change_text_document(snapshot.clone(), params).await;
        }
        UpdateEvent::Saved(params) => {
            handlers::process_did_save_text_document(snapshot.clone(), params).await;
        }
        UpdateEvent::Closed(params) => {
            handlers::process_did_close_document(snapshot.clone(), params).await;
        }
        UpdateEvent::WatchedFilesChanged(params) => {
            handlers::process_did_change_watched_files(snapshot.clone(), params).await;
        }
    }
}

/// The worker: apply whatever is queued, and wait when there is nothing.
///
/// It exists for the requests that never come: an edit with no completion after it still has to reach the analysis
/// (diagnostics are published from it, and the index pump is woken by it).
pub fn spawn_update_queue(inner: Arc<ServerContextInner>) {
    tokio::spawn(async move {
        let snapshot = ServerContextSnapshot::new(inner);

        loop {
            let worker_snapshot = &snapshot;
            let applied = snapshot
                .inbox()
                .apply_upto(u64::MAX, |event| async move {
                    process(worker_snapshot, event).await
                })
                .await;
            if applied == 0 {
                snapshot.inbox().wait_for_work(IDLE_WAIT).await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{UpdateEvent, UpdateInbox};
    use lsp_types::{
        DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
        TextDocumentIdentifier,
    };

    fn opened(uri: &str) -> UpdateEvent {
        UpdateEvent::Opened(DidOpenTextDocumentParams {
            text_document: lsp_types::TextDocumentItem {
                uri: uri.parse().expect("a uri"),
                language_id: "cpp".to_string(),
                version: 1,
                text: String::new(),
            },
        })
    }

    fn closed(uri: &str) -> UpdateEvent {
        UpdateEvent::Closed(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: uri.parse().expect("a uri"),
            },
        })
    }

    /// A `didChange` carrying the whole text, the way FULL sync sends one.
    fn changed(uri: &str, version: i32, text: &str) -> UpdateEvent {
        UpdateEvent::Changed(DidChangeTextDocumentParams {
            text_document: lsp_types::VersionedTextDocumentIdentifier {
                uri: uri.parse().expect("a uri"),
                version,
            },
            content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: text.to_string(),
            }],
        })
    }

    /// The text a queued `Changed` carries, for an assertion about which one survived.
    fn text_of(event: &UpdateEvent) -> Option<&str> {
        match event {
            UpdateEvent::Changed(params) => Some(&params.content_changes.last()?.text),
            _ => None,
        }
    }

    #[test]
    fn a_save_without_text_does_not_invalidate_running_requests() {
        let event = UpdateEvent::Saved(lsp_types::DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier {
                uri: "file:///a.cpp".parse().unwrap(),
            },
            text: None,
        });
        assert!(!event.changes_analysis());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publishing_a_caught_up_result_cannot_race_a_new_enqueue() {
        let inbox = std::sync::Arc::new(UpdateInbox::new());
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let publishing_inbox = inbox.clone();
        let publishing = tokio::spawn(async move {
            publishing_inbox
                .when_caught_up(move || {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                })
                .await
        });
        entered_rx.await.unwrap();
        let enqueueing_inbox = inbox.clone();
        let mut enqueueing =
            tokio::spawn(async move { enqueueing_inbox.push(opened("file:///a.cpp")).await });
        let blocked = tokio::time::timeout(std::time::Duration::from_millis(50), &mut enqueueing)
            .await
            .is_err();
        release_tx.send(()).unwrap();
        publishing.await.unwrap().unwrap();
        enqueueing.await.unwrap();
        assert!(blocked);
    }

    /// **A run of changes to one document is one piece of work, and it is the last text of the run.**
    ///
    /// This is what makes the ordering guarantee affordable: a client that is typing sends a change per keystroke,
    /// and the server used to apply every one of them — 24 parses for a user who typed 24 characters before the
    /// completion that needed the *last* of them. What the request must see is unchanged (it still sees the run's
    /// last text, which is the only text of the run that exists).
    #[tokio::test]
    async fn a_run_of_changes_to_one_document_is_one_piece_of_work() {
        let inbox = UpdateInbox::new();

        for version in 1..=24 {
            inbox
                .push(changed(
                    "file:///a.cpp",
                    version,
                    &format!("text {version}"),
                ))
                .await;
        }

        let mut applied = Vec::new();
        let count = inbox
            .apply_upto(u64::MAX, |event| {
                applied.push(text_of(&event).unwrap_or_default().to_string());
                async {}
            })
            .await;

        assert_eq!(count, 1, "one document's run of changes is applied once");
        assert_eq!(applied, ["text 24"], "and the text applied is the last one");

        // The count is still per **message**: that is what a request measures its own past with, so a request
        // dispatched after the 24th change is entitled to it, and one dispatched before them is not.
        assert_eq!(inbox.queued(), 24);
    }

    /// The three shapes that must **not** collapse, each for its own reason.
    #[tokio::test]
    async fn changes_that_are_not_the_same_document_in_a_row_are_left_alone() {
        let inbox = UpdateInbox::new();

        inbox.push(changed("file:///a.cpp", 1, "a1")).await;
        // A different document: last-wins across these would drop a whole file's edit.
        inbox.push(changed("file:///b.cpp", 1, "b1")).await;
        // The same document again, but not adjacent: merging would apply `a2` before `b.cpp` was even opened.
        inbox.push(changed("file:///a.cpp", 2, "a2")).await;
        // A close and a reopen are not text: they move the file between the buffer and the disk.
        inbox.push(closed("file:///c.cpp")).await;
        inbox.push(opened("file:///c.cpp")).await;

        let mut applied = Vec::new();
        let count = inbox
            .apply_upto(u64::MAX, |event| {
                applied.push(text_of(&event).map(str::to_string));
                async {}
            })
            .await;

        assert_eq!(count, 5, "five messages, five applications: {applied:?}");
        assert_eq!(
            applied
                .iter()
                .filter_map(|text| text.as_deref())
                .collect::<Vec<_>>(),
            ["a1", "b1", "a2"],
            "in the order the client sent them"
        );
    }

    /// The bound is what makes the guarantee exact: a request applies what the client sent **before** it, and a
    /// change that arrived after is left for the next one — which is the difference between answering about the
    /// text the cursor was placed in and answering about text the user has not seen yet.
    #[tokio::test]
    async fn applying_stops_at_the_bound_it_was_given() {
        let inbox = UpdateInbox::new();

        inbox.push(opened("file:///a.cpp")).await;
        // The request is dispatched here: two events are the client's past, and the third is its future.
        let dispatched_at = inbox.queued();
        inbox.push(closed("file:///a.cpp")).await;

        let mut applied = Vec::new();
        let count = inbox
            .apply_upto(dispatched_at, |event| {
                applied.push(matches!(event, UpdateEvent::Opened(_)));
                async {}
            })
            .await;

        assert_eq!(count, 1);
        assert_eq!(
            applied,
            [true],
            "the `Closed` is not this request's business"
        );

        // …and the next caller — the idle worker, or the next request — gets it.
        let count = inbox
            .apply_upto(u64::MAX, |event| {
                applied.push(matches!(event, UpdateEvent::Opened(_)));
                async {}
            })
            .await;
        assert_eq!(count, 1);
        assert_eq!(applied, [true, false], "in the order the client sent them");
    }

    #[tokio::test]
    async fn a_request_boundary_prevents_merging_a_later_change() {
        let inbox = UpdateInbox::new();
        inbox.push(changed("file:///a.cpp", 1, "before")).await;
        let bound = inbox.record_request();
        inbox.push(changed("file:///a.cpp", 2, "after")).await;

        let mut applied = Vec::new();
        inbox
            .apply_upto(bound, |event| {
                applied.push(text_of(&event).unwrap().to_string());
                async {}
            })
            .await;
        assert_eq!(applied, ["before"]);

        inbox
            .apply_upto(u64::MAX, |event| {
                applied.push(text_of(&event).unwrap().to_string());
                async {}
            })
            .await;
        assert_eq!(applied, ["before", "after"]);
    }

    #[tokio::test]
    async fn incremental_changes_are_combined_without_losing_their_order() {
        let inbox = UpdateInbox::new();
        let ranged = |version, text: &str| {
            let mut event = changed("file:///a.cpp", version, text);
            if let UpdateEvent::Changed(params) = &mut event {
                params.content_changes[0].range = Some(lsp_types::Range::new(
                    lsp_types::Position::new(0, 0),
                    lsp_types::Position::new(0, 0),
                ));
            }
            event
        };
        inbox.push(ranged(2, "first")).await;
        inbox.push(ranged(3, "second")).await;

        let mut changes = Vec::new();
        inbox
            .apply_upto(u64::MAX, |event| {
                if let UpdateEvent::Changed(params) = event {
                    assert_eq!(params.text_document.version, 3);
                    changes.extend(params.content_changes.into_iter().map(|change| change.text));
                }
                async {}
            })
            .await;
        assert_eq!(changes, ["first", "second"]);
    }

    #[tokio::test]
    async fn a_full_replacement_discards_preceding_incremental_edits() {
        let inbox = UpdateInbox::new();
        inbox.push(changed("file:///a.cpp", 1, "old")).await;
        let mut later = match changed("file:///a.cpp", 2, "replacement") {
            UpdateEvent::Changed(params) => params,
            _ => unreachable!(),
        };
        later.content_changes.insert(
            0,
            lsp_types::TextDocumentContentChangeEvent {
                range: Some(lsp_types::Range::new(
                    lsp_types::Position::new(10, 0),
                    lsp_types::Position::new(10, 0),
                )),
                range_length: None,
                text: "obsolete".to_string(),
            },
        );
        inbox.push(UpdateEvent::Changed(later)).await;
        let mut changes = Vec::new();
        inbox
            .apply_upto(u64::MAX, |event| {
                if let UpdateEvent::Changed(params) = event {
                    changes.extend(params.content_changes.into_iter().map(|change| change.text));
                }
                async {}
            })
            .await;
        assert_eq!(changes, ["replacement"]);
    }

    #[tokio::test]
    async fn cancelling_a_waiter_does_not_discard_an_update_in_progress() {
        use std::sync::Arc;
        use std::time::Duration;

        let inbox = Arc::new(UpdateInbox::new());
        inbox.push(changed("file:///a.cpp", 1, "text")).await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let worker_inbox = inbox.clone();
        let worker = tokio::spawn(async move {
            let mut entered = Some(entered_tx);
            let mut release = Some(release_rx);
            worker_inbox
                .apply_upto(u64::MAX, |_| {
                    let entered = entered.take().unwrap();
                    let release = release.take().unwrap();
                    async move {
                        entered.send(()).unwrap();
                        release.await.unwrap();
                    }
                })
                .await
        });

        entered_rx.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), inbox.wait_until_applied(1))
                .await
                .is_err()
        );
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), inbox.wait_until_applied(1))
            .await
            .expect("the update completes after its waiter is dropped");
        assert_eq!(worker.await.unwrap(), 1);
    }

    /// Two processors do not interleave: while one is applying an event, another waits for the queue — so
    /// `didOpen` and the `didChange` that follows it cannot run at the same time.
    #[tokio::test]
    async fn only_one_processor_is_inside_the_queue() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let inbox = Arc::new(UpdateInbox::new());
        inbox.push(opened("file:///a.cpp")).await;
        inbox.push(closed("file:///a.cpp")).await;

        let inside = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));

        let mut running = Vec::new();
        for _ in 0..2 {
            let inbox = inbox.clone();
            let inside = inside.clone();
            let most = most.clone();
            running.push(tokio::spawn(async move {
                inbox
                    .apply_upto(u64::MAX, |_| {
                        let inside = inside.clone();
                        let most = most.clone();
                        async move {
                            let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                            most.fetch_max(now, Ordering::SeqCst);
                            // A yield, so that the other task has every chance to get in if the lock allowed it.
                            tokio::task::yield_now().await;
                            inside.fetch_sub(1, Ordering::SeqCst);
                        }
                    })
                    .await
            }));
        }

        let mut applied = 0;
        for task in running {
            applied += task.await.expect("the task ran");
        }

        assert_eq!(
            applied, 2,
            "each event is applied exactly once, by one of the two"
        );
        assert_eq!(most.load(Ordering::SeqCst), 1, "and never by both at once");
    }
}
