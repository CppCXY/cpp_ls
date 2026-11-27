use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use lsp_types::{
    NumberOrString, ProgressParams, ProgressParamsValue, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd, WorkDoneProgressReport,
};

use super::ClientProxy;

pub struct StatusBar {
    client: Arc<ClientProxy>,
    supports_work_done_progress: bool,
    /// **Which load the indicator on screen belongs to** — see [`StatusBar::begin_load`].
    ///
    /// A reload starts a new load before the previous one has stopped, and both used to drive the same progress
    /// token independently: whichever finished first ended the other's bar. The number is what tells a finishing
    /// load whether the bar it is holding is still its own.
    the_load_on_screen: Arc<AtomicU64>,
    /// The next load's number.
    next_load: Arc<AtomicU64>,
}

/// **The load indicator one pump owns** — created by [`StatusBar::begin_load`], ended by [`LoadIndicator::retire`]
/// or by dropping it.
///
/// Dropping is what makes a stuck bar impossible: the pump has several exits (the workspace was reloaded, the
/// session was superseded, a panic) and every one of them drops this value.
pub struct LoadIndicator {
    client: Arc<ClientProxy>,
    supports_work_done_progress: bool,
    generation: u64,
    the_load_on_screen: Arc<AtomicU64>,
    retired: AtomicBool,
}

impl LoadIndicator {
    /// **End the bar now**, while the pump keeps running (its remaining work is not what the reader waits for).
    ///
    /// Only the load whose number is still on screen may end it: a reload that started meanwhile took the bar over.
    pub fn retire(&self) {
        if self.retired.swap(true, Ordering::AcqRel) {
            return;
        }
        if !self.supports_work_done_progress {
            return;
        }
        if self.the_load_on_screen.load(Ordering::Acquire) != self.generation {
            return;
        }
        self.client.send_notification(
            "$/progress",
            ProgressParams {
                token: NumberOrString::Number(ProgressTask::LoadWorkspace.as_i32()),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                    message: Some("the workspace is indexed".to_string()),
                })),
            },
        )
    }
}

impl Drop for LoadIndicator {
    fn drop(&mut self) {
        self.retire();
    }
}

///
/// **One variant, and there was a second.** `DiagnoseWorkspace` reported a pass that published every indexed file's
/// diagnostics; with diagnostics pulled (`textDocument/diagnostic`) there is no such pass — a client asks about the
/// files it is showing, and the server does not walk the project to answer them. A variant nothing constructs is an
/// invitation to construct it, so it is gone with the pass.
#[derive(Debug, Clone, Copy)]
pub enum ProgressTask {
    LoadWorkspace = 0,
}

impl ProgressTask {
    pub fn as_i32(&self) -> i32 {
        *self as i32
    }

    pub fn get_task_name(&self) -> &'static str {
        match self {
            ProgressTask::LoadWorkspace => "Load workspace",
        }
    }
}

impl StatusBar {
    pub fn new(client: Arc<ClientProxy>, supports_work_done_progress: bool) -> Self {
        Self {
            client,
            supports_work_done_progress,
            the_load_on_screen: Arc::new(AtomicU64::new(0)),
            next_load: Arc::new(AtomicU64::new(0)),
        }
    }

    /// **Start the load indicator, and hand back the only thing that may end it.**
    ///
    /// The bar used to be driven by `create_progress_task` at the start and `finish_progress_task` at the end, with
    /// every exit path between them responsible for calling the second — and two of them did not: a session
    /// discarded because a reload superseded it, and a pump that stopped because the workspace version moved. Both
    /// left the reader's bar frozen at its last report ("re-reading 1550 file(s), cooking 79 file(s)") with the
    /// server idle. A guard cannot forget.
    ///
    /// A **newer load** takes the bar over: the older guard's `retire`/`drop` then does nothing, so a finishing
    /// load can never end the bar of the one that replaced it.
    pub fn begin_load(&self) -> LoadIndicator {
        let generation = self.next_load.fetch_add(1, Ordering::AcqRel) + 1;
        self.the_load_on_screen.store(generation, Ordering::Release);
        self.create_progress_task(ProgressTask::LoadWorkspace);
        LoadIndicator {
            client: Arc::clone(&self.client),
            supports_work_done_progress: self.supports_work_done_progress,
            generation,
            the_load_on_screen: Arc::clone(&self.the_load_on_screen),
            retired: AtomicBool::new(false),
        }
    }

    pub fn create_progress_task(&self, task: ProgressTask) {
        if !self.supports_work_done_progress {
            return;
        }

        self.client.send_request_no_response(
            "window/workDoneProgress/create",
            WorkDoneProgressCreateParams {
                token: NumberOrString::Number(task.as_i32()),
            },
        );

        self.client.send_notification(
            "$/progress",
            ProgressParams {
                token: NumberOrString::Number(task as i32),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                    WorkDoneProgressBegin {
                        title: task.get_task_name().to_string(),
                        cancellable: Some(false),
                        message: Some(task.get_task_name().to_string()),
                        percentage: None,
                    },
                )),
            },
        )
    }

    pub fn update_progress_task(
        &self,
        task: ProgressTask,
        percentage: Option<u32>,
        message: Option<String>,
    ) {
        if !self.supports_work_done_progress {
            return;
        }

        self.client.send_notification(
            "$/progress",
            ProgressParams {
                token: NumberOrString::Number(task.as_i32()),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                    WorkDoneProgressReport {
                        percentage,
                        cancellable: Some(false),
                        message,
                    },
                )),
            },
        )
    }

    pub fn finish_progress_task(&self, task: ProgressTask, message: Option<String>) {
        if !self.supports_work_done_progress {
            return;
        }

        self.client.send_notification(
            "$/progress",
            ProgressParams {
                token: NumberOrString::Number(task.as_i32()),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                    message,
                })),
            },
        )
    }
}
