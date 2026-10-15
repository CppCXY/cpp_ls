use std::sync::Arc;

use lsp_types::{
    NumberOrString, ProgressParams, ProgressParamsValue, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd, WorkDoneProgressReport,
};

use super::ClientProxy;

pub struct StatusBar {
    client: Arc<ClientProxy>,
    supports_work_done_progress: bool,
}

/// Progress a client can be shown.
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
        }
    }

    pub async fn create_progress_task(&self, task: ProgressTask) {
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
