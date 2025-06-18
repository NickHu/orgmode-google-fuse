use google_calendar3::{
    api::{Calendar, CalendarListEntry, Event},
    CalendarHub,
};
use google_tasks1::{
    api::{Task, TaskList},
    hyper_rustls::{self, HttpsConnector},
    hyper_util::{self, client::legacy::connect::HttpConnector},
    yup_oauth2, Result, TasksHub,
};

use crate::oauth::APPLICATION_SECRET;

type SyncToken = String;

pub(crate) struct GoogleClient {
    calendarhub: CalendarHub<HttpsConnector<HttpConnector>>,
    taskshub: TasksHub<HttpsConnector<HttpConnector>>,
}

impl GoogleClient {
    pub async fn new() -> Self {
        let dirs = directories::ProjectDirs::from("", "", "orgmode-google-fuse")
            .expect("Failed to get project directories");
        let authdir = dirs
            .state_dir()
            .expect("Failed to get state directory path");
        std::fs::create_dir_all(authdir).expect("Failed to create state directory");
        let auth = yup_oauth2::InstalledFlowAuthenticator::builder(
            APPLICATION_SECRET.clone(),
            yup_oauth2::InstalledFlowReturnMethod::HTTPRedirect,
        )
        .persist_tokens_to_disk(authdir.join("google_oauth2_token.json"))
        .build()
        .await
        .unwrap();

        auth.token(&[
            "https://www.googleapis.com/auth/calendar.readonly",
            "https://www.googleapis.com/auth/calendar.events.readonly",
            "https://www.googleapis.com/auth/tasks.readonly",
        ])
        .await
        .expect("Failed to get OAuth token");

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
        let calendarhub = CalendarHub::new(client.clone(), auth.clone());
        let taskshub = TasksHub::new(client, auth);
        Self {
            calendarhub,
            taskshub,
        }
    }

    pub async fn list_calendars(&self) -> Result<Vec<CalendarListEntry>> {
        self.calendarhub
            .calendar_list()
            .list()
            .doit()
            .await
            .map(|(_res, calendar_list_entry)| calendar_list_entry.items.unwrap_or_default())
    }

    pub async fn get_calendar(&self, calendar_id: &str) -> Result<Calendar> {
        self.calendarhub
            .calendars()
            .get(calendar_id)
            .doit()
            .await
            .map(|(_res, calendar)| calendar)
    }

    pub async fn list_events(&self, calendar_id: &str) -> Result<(Option<SyncToken>, Vec<Event>)> {
        self.calendarhub
            .events()
            .list(calendar_id)
            .doit()
            .await
            .map(|(_res, events)| (events.next_sync_token, events.items.unwrap_or_default()))
    }

    pub async fn list_events_with_sync_token(
        &self,
        calendar_id: &str,
        sync_token: &SyncToken,
    ) -> Result<(Option<SyncToken>, Vec<Event>)> {
        self.calendarhub
            .events()
            .list(calendar_id)
            .sync_token(sync_token)
            .doit()
            .await
            .map(|(_res, events)| (events.next_sync_token, events.items.unwrap_or_default()))
    }

    pub async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<Event> {
        self.calendarhub
            .events()
            .get(calendar_id, event_id)
            .doit()
            .await
            .map(|(_res, event)| event)
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
