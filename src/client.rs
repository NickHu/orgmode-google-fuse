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
        self.calendarhub
            .calendar_list()
            .list()
            .doit()
            .await
            .map(|(_res, calendar_list)| calendar_list)
    }

    #[allow(unused)]
    pub async fn get_calendar(&self, calendar_id: &str) -> Result<Calendar> {
        self.calendarhub
            .calendars()
            .get(calendar_id)
            .doit()
            .await
            .map(|(_res, calendar)| calendar)
    }

    pub async fn list_events(&self, calendar_id: &str) -> Result<Events> {
        self.calendarhub
            .events()
            .list(calendar_id)
            .time_min(
                // a year ago
                chrono::Utc::now()
                    .checked_sub_signed(chrono::Duration::days(365))
                    .unwrap(),
            )
            .doit()
            .await
            .map(|(_res, events)| events)
    }

    pub async fn list_events_with_sync_token(
        &self,
        calendar_id: &str,
        sync_token: &SyncToken,
    ) -> Result<Events> {
        self.calendarhub
            .events()
            .list(calendar_id)
            .sync_token(sync_token)
            .doit()
            .await
            .map(|(_res, events)| events)
    }

    #[allow(unused)]
    pub async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<Event> {
        self.calendarhub
            .events()
            .get(calendar_id, event_id)
            .doit()
            .await
            .map(|(_res, event)| event)
    }

    pub async fn list_tasklists(&self) -> Result<TaskLists> {
        self.taskshub
            .tasklists()
            .list()
            .doit()
            .await
            .map(|(_res, tasklists)| tasklists)
    }

    pub async fn get_tasklist(&self, tasklist_id: &str) -> Result<TaskList> {
        self.taskshub
            .tasklists()
            .get(tasklist_id)
            .doit()
            .await
            .map(|(_res, tasklist)| tasklist)
    }

    pub async fn list_tasks(&self, tasklist_id: &str) -> Result<Tasks> {
        self.taskshub
            .tasks()
            .list(tasklist_id)
            .doit()
            .await
            .map(|(_res, tasks)| tasks)
    }

    #[allow(unused)]
    pub async fn get_task(&self, tasklist_id: &str, task_id: &str) -> Result<Task> {
        self.taskshub
            .tasks()
            .get(tasklist_id, task_id)
            .doit()
            .await
            .map(|(_res, task)| task)
    }

    pub async fn insert_task(&self, tasklist_id: &str, task: Task) -> Result<Task> {
        self.taskshub
            .tasks()
            .insert(task, tasklist_id)
            .doit()
            .await
            .map(|(_res, task)| task)
    }

    pub async fn patch_task(&self, tasklist_id: &str, task_id: &str, task: Task) -> Result<Task> {
        self.taskshub
            .tasks()
            .patch(task, tasklist_id, task_id)
            .doit()
            .await
            .map(|(_res, task)| task)
    }

    pub async fn delete_task(&self, tasklist_id: &str, task_id: &str) -> Result<()> {
        self.taskshub
            .tasks()
            .delete(tasklist_id, task_id)
            .doit()
            .await
            .map(|_res| ())
    }
}

pub(crate) enum WriteCommand {
    InsertTask {
        tasklist_id: String,
        task: Task,
    },
    PatchTask {
        tasklist_id: String,
        task_id: String,
        task: Task,
    },
    DeleteTask {
        tasklist_id: String,
        task_id: String,
    },
    SyncTasklist {
        tasklist_id: String,
    },
}
