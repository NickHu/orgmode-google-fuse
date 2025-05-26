use google_tasks1::{
    api::{Task, TaskList},
    hyper_rustls::{self, HttpsConnector},
    hyper_util::{self, client::legacy::connect::HttpConnector},
    yup_oauth2, Result, TasksHub,
};

use crate::oauth::APPLICATION_SECRET;

pub(crate) struct GoogleClient {
    taskshub: TasksHub<HttpsConnector<HttpConnector>>,
}

impl GoogleClient {
    pub async fn new() -> Self {
        let auth = yup_oauth2::InstalledFlowAuthenticator::builder(
            APPLICATION_SECRET.clone(),
            yup_oauth2::InstalledFlowReturnMethod::HTTPRedirect,
        )
        .build()
        .await
        .unwrap();

        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(
                    hyper_rustls::HttpsConnectorBuilder::new()
                        .with_native_roots()
                        .unwrap()
                        .https_or_http()
                        .enable_http1()
                        .build(),
                );
        let taskshub = TasksHub::new(client, auth);
        Self { taskshub }
    }

    pub async fn list_tasklists(&self) -> Result<Vec<TaskList>> {
        self.taskshub
            .tasklists()
            .list()
            .doit()
            .await
            .map(|(_res, tasklists)| tasklists.items.unwrap_or_default())
    }

    pub async fn get_tasklist(&self, tasklist_id: &str) -> Result<TaskList> {
        self.taskshub
            .tasklists()
            .get(tasklist_id)
            .doit()
            .await
            .map(|(_res, tasklist)| tasklist)
    }

    pub async fn list_tasks(&self, tasklist_id: &str) -> Result<Vec<Task>> {
        self.taskshub
            .tasks()
            .list(tasklist_id)
            .doit()
            .await
            .map(|(_res, tasks)| tasks.items.unwrap_or_default())
    }

    pub async fn get_task(&self, tasklist_id: &str, task_id: &str) -> Result<Task> {
        self.taskshub
            .tasks()
            .get(tasklist_id, task_id)
            .doit()
            .await
            .map(|(_res, task)| task)
    }
}
