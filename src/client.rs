use google_calendar3::{
    api::{Calendar, CalendarList, Event, Events},
    CalendarHub,
};
use google_tasks1::{
    api::{Task, TaskList, TaskLists, Tasks},
    hyper_rustls::{self, HttpsConnector},
    hyper_util::{self, client::legacy::connect::HttpConnector},
    Result, TasksHub,
};
use tokio::time::timeout;

use crate::oauth::APPLICATION_SECRET;

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

pub(super) type SyncToken = String;

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
            .unwrap_or(std::path::Path::new("~/.local/state/orgmode-google-fuse"));
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
            "https://www.googleapis.com/auth/calendar",
            "https://www.googleapis.com/auth/calendar.events",
            "https://www.googleapis.com/auth/tasks",
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
                        .enable_http2()
                        .build(),
                );
        let calendarhub = CalendarHub::new(client.clone(), auth.clone());
        let taskshub = TasksHub::new(client, auth);
        Self {
            calendarhub,
            taskshub,
        }
    }

    pub async fn list_calendars(&self) -> Result<CalendarList> {
        timeout(TIMEOUT, self.calendarhub.calendar_list().list().doit())
            .await
            .unwrap_or_else(|e| Err(google_calendar3::Error::Io(e.into())))
            .map(|(_res, calendar_list)| calendar_list)
    }

    #[allow(unused)]
    pub async fn get_calendar(&self, calendar_id: &str) -> Result<Calendar> {
        timeout(
            TIMEOUT,
            self.calendarhub.calendars().get(calendar_id).doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_calendar3::Error::Io(e.into())))
        .map(|(_res, calendar)| calendar)
    }

    pub async fn list_events(&self, calendar_id: &str) -> Result<Events> {
        timeout(
            TIMEOUT,
            self.calendarhub
                .events()
                .list(calendar_id)
                .time_min(
                    // a year ago
                    chrono::Utc::now()
                        .checked_sub_signed(chrono::Duration::days(365))
                        .unwrap(),
                )
                .doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_calendar3::Error::Io(e.into())))
        .map(|(_res, events)| events)
    }

    pub async fn list_events_with_sync_token(
        &self,
        calendar_id: &str,
        sync_token: &SyncToken,
    ) -> Result<Events> {
        timeout(
            TIMEOUT,
            self.calendarhub
                .events()
                .list(calendar_id)
                .sync_token(sync_token)
                .doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_calendar3::Error::Io(e.into())))
        .map(|(_res, events)| events)
    }

    #[allow(unused)]
    pub async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<Event> {
        timeout(
            TIMEOUT,
            self.calendarhub.events().get(calendar_id, event_id).doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_calendar3::Error::Io(e.into())))
        .map(|(_res, event)| event)
    }

    pub async fn insert_event(&self, calendar_id: &str, event: Event) -> Result<Event> {
        timeout(
            TIMEOUT,
            self.calendarhub.events().insert(event, calendar_id).doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_calendar3::Error::Io(e.into())))
        .map(|(_res, event)| event)
    }

    pub async fn patch_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: Event,
    ) -> Result<Event> {
        timeout(
            TIMEOUT,
            self.calendarhub
                .events()
                .patch(event, calendar_id, event_id)
                .doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_calendar3::Error::Io(e.into())))
        .map(|(_res, event)| event)
    }

    pub async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<()> {
        timeout(
            TIMEOUT,
            self.calendarhub
                .events()
                .delete(calendar_id, event_id)
                .doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_calendar3::Error::Io(e.into())))
        .map(|_res| ())
    }

    pub async fn list_tasklists(&self) -> Result<TaskLists> {
        timeout(TIMEOUT, self.taskshub.tasklists().list().doit())
            .await
            .unwrap_or_else(|e| Err(google_tasks1::Error::Io(e.into())))
            .map(|(_res, tasklists)| tasklists)
    }

    pub async fn get_tasklist(&self, tasklist_id: &str) -> Result<TaskList> {
        timeout(TIMEOUT, self.taskshub.tasklists().get(tasklist_id).doit())
            .await
            .unwrap_or_else(|e| Err(google_tasks1::Error::Io(e.into())))
            .map(|(_res, tasklist)| tasklist)
    }

    pub async fn list_tasks(&self, tasklist_id: &str) -> Result<Tasks> {
        timeout(
            TIMEOUT,
            self.taskshub
                .tasks()
                .list(tasklist_id)
                .max_results(100)
                .show_deleted(false)
                .show_hidden(false)
                .doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_tasks1::Error::Io(e.into())))
        .map(|(_res, tasks)| tasks)
    }

    #[allow(unused)]
    pub async fn get_task(&self, tasklist_id: &str, task_id: &str) -> Result<Task> {
        timeout(
            TIMEOUT,
            self.taskshub.tasks().get(tasklist_id, task_id).doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_tasks1::Error::Io(e.into())))
        .map(|(_res, task)| task)
    }

    pub async fn insert_task(&self, tasklist_id: &str, task: Task) -> Result<Task> {
        timeout(
            TIMEOUT,
            self.taskshub.tasks().insert(task, tasklist_id).doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_tasks1::Error::Io(e.into())))
        .map(|(_res, task)| task)
    }

    pub async fn patch_task(&self, tasklist_id: &str, task_id: &str, task: Task) -> Result<Task> {
        timeout(
            TIMEOUT,
            self.taskshub
                .tasks()
                .patch(task, tasklist_id, task_id)
                .doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_tasks1::Error::Io(e.into())))
        .map(|(_res, task)| task)
    }

    pub async fn delete_task(&self, tasklist_id: &str, task_id: &str) -> Result<()> {
        timeout(
            TIMEOUT,
            self.taskshub.tasks().delete(tasklist_id, task_id).doit(),
        )
        .await
        .unwrap_or_else(|e| Err(google_tasks1::Error::Io(e.into())))
        .map(|_res| ())
    }

    pub(crate) async fn move_task(
        &self,
        tasklist_id: &str,
        task_id: &str,
        new_predecessor: Option<&str>,
    ) -> Result<Task> {
        timeout(
            TIMEOUT,
            if let Some(new_predecessor) = new_predecessor {
                self.taskshub
                    .tasks()
                    .move_(tasklist_id, task_id)
                    .previous(new_predecessor)
                    .doit()
            } else {
                self.taskshub.tasks().move_(tasklist_id, task_id).doit()
            },
        )
        .await
        .unwrap_or_else(|e| Err(google_tasks1::Error::Io(e.into())))
        .map(|(_res, task)| task)
    }
}
