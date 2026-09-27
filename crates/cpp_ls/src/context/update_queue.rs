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
//! So the queue is **applied before a request's handler runs**, and only up to the sequence number that was current
//! when the request was dispatched ([`UpdateInbox::queued`], read on the message loop's own task, where the order
//! is the client's). A change that arrives *after* the request is not applied for it — that is what the client's
//! `$/cancelRequest` is about, and applying it would answer a question about text the cursor was never placed in.
//!
//! # Who applies, and what "one writer" means here
//!
//! Two callers: the worker task (when the server is otherwise idle) and any request that finds the queue ahead of
//! it. They do not interleave, because the queue's lock is held **for as long as the events take to apply** — so
//! whoever holds it is the one writer, and the order the client sent things in is the order they happen in, no
//! matter who is doing the applying.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lsp_types::{
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams,
};
use tokio::sync::{Mutex, Notify};

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
pub enum UpdateEvent {
    Opened(DidOpenTextDocumentParams),
    Changed(DidChangeTextDocumentParams),
    Saved(DidSaveTextDocumentParams),
    Closed(DidCloseTextDocumentParams),
    WatchedFilesChanged(DidChangeWatchedFilesParams),
}

impl UpdateEvent {
    /// Is `other` a **newer statement about the same thing**, so that this event has nothing left to say?
    ///
    /// One shape qualifies, and it is the one a client produces while typing, pasting or formatting: two
    /// `didChange` for one document, one after the other. FULL sync is what makes it safe — every change carries
    /// the **whole** text, so the later one replaces the earlier rather than transforming it, and what is dropped
    /// is a text the client itself never showed (it went from the first to the second without stopping).
    ///
    /// Everything else is left alone, and each exclusion has its own reason:
    ///
    /// * `didOpen`, `didClose` and `didSave` are not only text — they move the file between "the buffer" and "the
    ///   disk", and a save is a statement that the two now agree;
    /// * two changes to **different** documents are two documents: last-wins across them would drop a file's whole
    ///   edit;
    /// * a change that is not **adjacent** to one for the same document is separated by something that changed the
    ///   world in between — `A` edited, `B` opened, `A` edited again: merging those two would apply the second
    ///   change to `A` before `B` was opened, which is an order the client did not send.
    ///
    /// (Incremental sync would break this rule outright, and that is one more reason this server advertises FULL:
    /// a range-based change cannot be dropped without losing the edits it makes.)
    fn absorbs(&self, other: &UpdateEvent) -> bool {
        match (self, other) {
            (UpdateEvent::Changed(earlier), UpdateEvent::Changed(later)) => {
                earlier.text_document.uri == later.text_document.uri
            }
            _ => false,
        }
    }
}

/// One event and where it sits in the client's stream.
struct QueuedUpdate {
    /// The number this event was given when it was queued, starting at `1`. See [`UpdateInbox::queued`].
    seq: u64,
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
        UpdateInbox {
            pending: Mutex::new(VecDeque::new()),
            applying: Mutex::new(()),
            queued: AtomicU64::new(0),
            work: Notify::new(),
        }
    }

    /// Queue one event, and wake whoever applies them.
    ///
    /// **A run of changes to one document collapses into one piece of work** — see [`UpdateEvent::absorbs`] for the
    /// exact rule and why nothing else may collapse. The count still goes up for every message, because the count
    /// is what a request measures its own past with, and the merged event keeps the **earliest** number of the run:
    /// a request that was dispatched after the first change of the run is entitled to see the run, and it now sees
    /// the run's last text — which is the only text of it that still exists.
    pub async fn push(&self, event: UpdateEvent) {
        // The number is taken **before** the lock, so that a caller reading `queued` on another task never sees a
        // count whose event is not in the queue yet: the count is only ever read by the task that sends (see the
        // module documentation), and this ordering is what makes that safe rather than lucky.
        let seq = self.queued.fetch_add(1, Ordering::SeqCst) + 1;

        let mut pending = self.pending.lock().await;
        match pending.back_mut() {
            Some(last) if last.event.absorbs(&event) => last.event = event,
            _ => pending.push_back(QueuedUpdate { seq, event }),
        }
        drop(pending);

        self.work.notify_one();
    }

    /// **How many events the client has sent so far** — the number a request records when it is dispatched.
    ///
    /// A request applies the queue **up to this number and no further**, which is the ordering guarantee stated as
    /// arithmetic: everything the client sent before the request, and nothing it sent after.
    pub fn queued(&self) -> u64 {
        self.queued.load(Ordering::SeqCst)
    }

    /// Wait until there is something to apply, or `timeout` passes.
    pub async fn wait_for_work(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.work.notified()).await.is_ok()
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
        let _applying = self.applying.lock().await;
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

            apply(queued.event).await;
            applied += 1;
        }

        applied
    }
}

/// **Apply the client's pending notifications up to `upto`** — the whole of what a request waits for.
pub async fn catch_up_upto(snapshot: &ServerContextSnapshot, upto: u64) -> usize {
    snapshot
        .inbox()
        .apply_upto(upto, |event| async move { process(snapshot, event).await })
        .await
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
            if catch_up_upto(&snapshot, u64::MAX).await == 0 {
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
                .push(changed("file:///a.cpp", version, &format!("text {version}")))
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
            applied.iter().filter_map(|text| text.as_deref()).collect::<Vec<_>>(),
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
        assert_eq!(applied, [true], "the `Closed` is not this request's business");

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

        assert_eq!(applied, 2, "each event is applied exactly once, by one of the two");
        assert_eq!(
            most.load(Ordering::SeqCst),
            1,
            "and never by both at once"
        );
    }
}
