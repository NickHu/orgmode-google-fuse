use google_calendar3::{
    api::{Calendar, CalendarList, Event, EventDateTime, Events},
    CalendarHub,
};
use google_tasks1::{
    api::{Task, TaskList, TaskLists, Tasks},
    hyper_rustls::{self, HttpsConnector},
    hyper_util::{self, client::legacy::connect::HttpConnector},
    Result, TasksHub,
};

use crate::oauth::APPLICATION_SECRET;

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

    pub async fn insert_event(&self, calendar_id: &str, event: Event) -> Result<Event> {
        self.calendarhub
            .events()
            .insert(event, calendar_id)
            .doit()
            .await
            .map(|(_res, event)| event)
    }

    pub async fn patch_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        event: Event,
    ) -> Result<Event> {
        self.calendarhub
            .events()
            .patch(event, calendar_id, event_id)
            .doit()
            .await
            .map(|(_res, event)| event)
    }

    pub async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<()> {
        self.calendarhub
            .events()
            .delete(calendar_id, event_id)
            .doit()
            .await
            .map(|_res| ())
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
            .max_results(100)
            .show_deleted(false)
            .show_hidden(false)
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

#[derive(Debug, Clone)]
pub(crate) enum WriteCommand {
    Task {
        tasklist_id: String,
        cmd: TaskWrite,
    },
    SyncTasklist {
        tasklist_id: String,
    },
    CalendarEvent {
        calendar_id: String,
        cmd: CalendarEventWrite,
    },
    SyncCalendar {
        calendar_id: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum TaskWrite {
    Insert(TaskInsert),
    Modify {
        task_id: String,
        modification: TaskModify,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum TaskInsert {
    Insert { task: Box<Task> },
}

impl PartialEq for TaskInsert {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (TaskInsert::Insert { task: task1 }, TaskInsert::Insert { task: task2 }) => {
                task1.completed == task2.completed
                    && task1.due == task2.due
                    && task1.notes == task2.notes
                    && task1.status == task2.status
                    && task1.title == task2.title
            }
        }
    }
}

impl Eq for TaskInsert {}

impl std::hash::Hash for TaskInsert {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            TaskInsert::Insert { task } => {
                task.completed.hash(state);
                task.due.hash(state);
                task.notes.hash(state);
                task.status.hash(state);
                task.title.hash(state);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum TaskModify {
    Patch { task: Box<Task> },
    Delete,
}

#[derive(Debug, Clone)]
pub(crate) enum CalendarEventWrite {
    Insert(CalendarEventInsert),
    Modify {
        event_id: String,
        modification: CalendarEventModify,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum CalendarEventInsert {
    Insert { event: Box<Event> },
}

impl PartialEq for CalendarEventInsert {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                CalendarEventInsert::Insert { event: event1 },
                CalendarEventInsert::Insert { event: event2 },
            ) => {
                fn eq_eventdatetime(x: &Option<EventDateTime>, y: &Option<EventDateTime>) -> bool {
                    match (x, y) {
                        (Some(x), Some(y)) => {
                            x.date == y.date
                                && x.date_time == y.date_time
                                && x.time_zone == y.time_zone
                        }
                        (None, None) => true,
                        _ => false,
                    }
                }
                event1.description == event2.description
                    && eq_eventdatetime(&event1.end, &event2.end)
                    && eq_eventdatetime(&event1.start, &event2.start)
                    && event1.summary == event2.summary
                    && event1.color_id == event2.color_id
                    && event1.location == event2.location
                    && event1.status == event2.status
                    && event1.status == event2.transparency
            }
        }
    }
}

impl Eq for CalendarEventInsert {}

impl std::hash::Hash for CalendarEventInsert {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            CalendarEventInsert::Insert { event } => {
                event.description.hash(state);
                {
                    let mut hash_eventdatetime = |x: &Option<EventDateTime>| {
                        if let Some(x) = x {
                            x.date.hash(state);
                            x.date_time.hash(state);
                            x.time_zone.hash(state);
                        } else {
                            None::<()>.hash(state);
                        }
                    };
                    hash_eventdatetime(&event.end);
                    hash_eventdatetime(&event.start);
                }
                event.summary.hash(state);
                event.color_id.hash(state);
                event.location.hash(state);
                event.status.hash(state);
                event.transparency.hash(state);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CalendarEventModify {
    Patch { event: Box<Event> },
    Delete,
}
