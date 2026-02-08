use std::{sync::atomic::Ordering, time::SystemTime};

use google_calendar3::api::{Event, EventDateTime};
use google_tasks1::api::Task;

use crate::{
    client,
    org::{calendar::OrgCalendar, tasklist::OrgTaskList, MetaPendingContainer},
    update_calendar, update_tasklist,
};

// trick vim into reloading
const TOUCH_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Clone)]
pub(crate) enum WriteCommand {
    CalendarEvent {
        calendar_id: String,
        cmd: CalendarEventWrite,
    },
    SyncCalendar {
        calendar_id: String,
    },
    TouchCalendar {
        calendar_id: String,
    },
    Task {
        tasklist_id: String,
        cmd: TaskWrite,
    },
    SyncTasklist {
        tasklist_id: String,
    },
    TouchTasklist {
        tasklist_id: String,
    },
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

async fn process_calendar_write(
    client: &client::GoogleClient,
    calendar: &OrgCalendar,
    cmd: CalendarEventWrite,
) {
    let calendar_id = calendar.with_meta(|m| m.calendar().id.clone()).unwrap();
    match cmd {
        CalendarEventWrite::Insert(CalendarEventInsert::Insert { event }) => {
            if let Ok(new) = client.insert_event(&calendar_id, *event.clone()).await {
                let id = new
                    .id
                    .clone()
                    .expect("Server returned inserted event with no id");
                tracing::debug!("Inserted event with id: {}", id);
                calendar.add_id(&id, new);
            } else {
                calendar.push_pending_insert(CalendarEventInsert::Insert { event });
            }
        }
        CalendarEventWrite::Modify {
            event_id,
            modification: CalendarEventModify::Patch { event },
        } => {
            if let Ok(new) = client
                .patch_event(&calendar_id, &event_id, *event.clone())
                .await
            {
                tracing::debug!("Updated event with id: {}", event_id);
                calendar.update_id(&event_id, new);
            } else {
                calendar.push_pending_modify(event_id, CalendarEventModify::Patch { event });
            }
        }
        CalendarEventWrite::Modify {
            event_id,
            modification: CalendarEventModify::Delete,
        } => {
            if let Ok(()) = client.delete_event(&calendar_id, &event_id).await {
                calendar.delete_id(&event_id);
            } else {
                calendar.push_pending_modify(event_id, CalendarEventModify::Delete);
            }
        }
    }
}

async fn process_tasklist_write(
    client: &client::GoogleClient,
    tasklist: &OrgTaskList,
    cmd: TaskWrite,
) {
    let tasklist_id = tasklist.with_meta(|m| m.tasklist().id.clone()).unwrap();
    match cmd {
        TaskWrite::Insert(TaskInsert::Insert { task }) => {
            tracing::trace!("Inserting");
            if let Ok(new) = client.insert_task(&tasklist_id, *task.clone()).await {
                tracing::trace!("Success");
                let id = new
                    .id
                    .clone()
                    .expect("Server returned inserted task with no id");
                tracing::debug!("Inserted task with id: {}", id);
                tasklist.add_id(&id, new);
            } else {
                tracing::error!("Failed to insert task; saving");
                tasklist.push_pending_insert(TaskInsert::Insert { task });
            }
            tracing::trace!("Finish insert");
        }
        TaskWrite::Modify {
            task_id,
            modification: TaskModify::Patch { task },
        } => {
            if let Ok(new) = client
                .patch_task(&tasklist_id, &task_id, *task.clone())
                .await
            {
                tracing::debug!("Updated task with id: {}", task_id);
                tasklist.update_id(&task_id, new);
            } else {
                tracing::error!("Failed to update task with id: {}; saving", task_id);
                tasklist.push_pending_modify(task_id, TaskModify::Patch { task });
            }
        }
        TaskWrite::Modify {
            task_id,
            modification: TaskModify::Delete,
        } => {
            if let Ok(()) = client.delete_task(&tasklist_id, &task_id).await {
                tasklist.delete_id(&task_id);
            } else {
                tracing::error!("Failed to delete task with id: {}; saving", task_id);
                tasklist.push_pending_modify(task_id, TaskModify::Delete);
            }
        }
    }
}

pub(super) async fn process_write(
    client: &client::GoogleClient,
    calendars: &[OrgCalendar],
    sync_tokens: &mut [(String, Option<String>)],
    tasklists: &[OrgTaskList],
    cmd: WriteCommand,
) {
    match cmd {
        WriteCommand::CalendarEvent { calendar_id, cmd } => {
            let calendar = calendars
                .iter()
                .find(|cal| cal.with_meta(|m| m.calendar().id.as_ref() == Some(&calendar_id)))
                .expect("Calendar not found");
            process_calendar_write(client, calendar, cmd).await;
        }
        WriteCommand::SyncCalendar { calendar_id } => {
            let calendar = calendars
                .iter()
                .find(|cal| cal.with_meta(|m| m.calendar().id.as_ref() == Some(&calendar_id)))
                .expect("Calendar not found");
            let sync_token = sync_tokens
                .iter_mut()
                .find(|(id, _)| id == &calendar_id)
                .and_then(|(_, token)| token.as_mut());

            // try to flush our pending writes
            if calendar.with_pending(|p| !(p.0.is_empty() && p.1.is_empty())) {
                tracing::debug!("Flushing pending writes for calendar {}", calendar_id);
                let old_meta = calendar.clear_pending();
                let pending = old_meta.pending();
                for insert in &pending.0 {
                    process_calendar_write(
                        client,
                        calendar,
                        CalendarEventWrite::Insert(insert.clone()),
                    )
                    .await;
                }
                for (event_id, modification) in &pending.1 {
                    process_calendar_write(
                        client,
                        calendar,
                        CalendarEventWrite::Modify {
                            event_id: event_id.clone(),
                            modification: modification.clone(),
                        },
                    )
                    .await;
                }
            }

            let next_sync_token = update_calendar(client, calendar, sync_token.as_deref())
                .await
                .unwrap_or_else(|e| {
                    tracing::error!("Failed to sync calendar {}: {}", calendar_id, e);
                    None
                });
            if let (Some(sync_token), Some(next_sync_token)) = (sync_token, next_sync_token) {
                *sync_token = next_sync_token;
            }
        }
        WriteCommand::TouchCalendar { calendar_id } => {
            let calendar = calendars
                .iter()
                .find(|tl| tl.with_meta(|m| m.calendar().id.as_ref() == Some(&calendar_id)))
                .expect("Calendar not found");
            calendar.with_meta(|m| {
                m.updated()
                    .store(SystemTime::now() + TOUCH_DELAY, Ordering::Release)
            });
        }
        WriteCommand::Task { tasklist_id, cmd } => {
            let tasklist = tasklists
                .iter()
                .find(|tl| tl.with_meta(|m| m.tasklist().id.as_ref() == Some(&tasklist_id)))
                .expect("Tasklist not found");
            process_tasklist_write(client, tasklist, cmd).await;
        }
        WriteCommand::SyncTasklist { tasklist_id } => {
            let tasklist = tasklists
                .iter()
                .find(|tl| tl.with_meta(|m| m.tasklist().id.as_ref() == Some(&tasklist_id)))
                .expect("Tasklist not found");

            // try to flush our pending writes
            if tasklist.with_pending(|p| !(p.0.is_empty() && p.1.is_empty())) {
                tracing::debug!("Flushing pending writes for tasklist {}", tasklist_id);
                let old_meta = tasklist.clear_pending();
                let pending = old_meta.pending();
                for insert in &pending.0 {
                    process_tasklist_write(client, tasklist, TaskWrite::Insert(insert.clone()))
                        .await;
                }
                for (task_id, modification) in &pending.1 {
                    process_tasklist_write(
                        client,
                        tasklist,
                        TaskWrite::Modify {
                            task_id: task_id.clone(),
                            modification: modification.clone(),
                        },
                    )
                    .await;
                }
            }

            if let Err(e) = update_tasklist(client, tasklist).await {
                tracing::error!("Failed to sync tasklist {}: {}", tasklist_id, e);
            }
        }
        WriteCommand::TouchTasklist { tasklist_id } => {
            let tasklist = tasklists
                .iter()
                .find(|tl| tl.with_meta(|m| m.tasklist().id.as_ref() == Some(&tasklist_id)))
                .expect("Tasklist not found");
            tasklist.with_meta(|m| {
                m.updated()
                    .store(SystemTime::now() + TOUCH_DELAY, Ordering::Release)
            });
        }
    }
}
