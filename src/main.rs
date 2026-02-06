use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use clap::Parser;
use fuse::OrgFS;
use fuser::MountOption;
use futures::{stream, StreamExt};

use crate::{
    client::{
        CalendarEventInsert, CalendarEventModify, CalendarEventWrite, TaskInsert, TaskModify,
        TaskWrite, WriteCommand,
    },
    org::{calendar::OrgCalendar, tasklist::OrgTaskList, MetaPendingContainer},
};

mod client;
mod fuse;
mod oauth;
mod org;

pub(crate) type Pid = u32;

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(120); // 2 minutes

#[derive(Parser, Debug)]
#[clap(author = "Nick Hu", version, about)]
/// Application configuration
struct Args {
    /// mount point
    #[arg()]
    mount: String,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    std::fs::create_dir_all(&args.mount).expect("Failed to create mount directory");

    let client = Arc::new(client::GoogleClient::new().await);

    let cl = client.list_calendars().await.unwrap();
    let sync_tokens = Arc::new(tokio::sync::Mutex::new(Vec::default()));
    let calendars = Arc::new(
        stream::iter(cl.items.unwrap_or_default().into_iter())
            .filter_map(|cal| async {
                let events = client.list_events(cal.id.as_ref().unwrap()).await.ok()?;
                let sync_token = events.next_sync_token.as_ref().cloned();
                sync_tokens
                    .lock()
                    .await
                    .push((cal.id.clone().unwrap(), sync_token));
                Some((cal, events).into())
            })
            .collect::<Vec<_>>()
            .await,
    );

    let tls = client.list_tasklists().await.unwrap();
    let tasklists = Arc::new(
        stream::iter(tls.items.unwrap_or_default().into_iter())
            .filter_map(|tl| async {
                let tasks = client.list_tasks(tl.id.as_ref().unwrap()).await.ok()?;
                Some((tl, tasks).into())
            })
            .collect::<Vec<_>>()
            .await,
    );

    let (tx_wcmd, mut rx_wcmd) = tokio::sync::mpsc::unbounded_channel::<WriteCommand>();
    let (tx_fh, mut rx_fh) = tokio::sync::mpsc::unbounded_channel::<Pid>();
    let pending_fh = Arc::new(Mutex::new(HashMap::new()));
    let _handle = fuser::spawn_mount2(
        OrgFS::new(
            calendars.clone(),
            tasklists.clone(),
            tx_wcmd,
            tx_fh,
            pending_fh.clone(),
        ),
        &args.mount,
        &[MountOption::FSName("orgmode-google-fuse".to_string())],
    )?;

    // spawn background task to poll for calendars updates
    tokio::spawn({
        let client = client.clone();
        let calendars = calendars.clone();
        let mut sync_tokens = sync_tokens.clone();
        async move {
            let mut interval = tokio::time::interval(POLL_INTERVAL);
            loop {
                interval.tick().await;
                if let Err(e) = update_calendars(&client, &calendars, &mut sync_tokens).await {
                    tracing::error!("Failed to update calendars: {:?}", e);
                }
            }
        }
    });

    // spawn background task to poll for tasks updates
    tokio::spawn({
        let client = client.clone();
        let tasklists = tasklists.clone();
        async move {
            let mut interval = tokio::time::interval(POLL_INTERVAL);
            loop {
                interval.tick().await;
                if let Err(e) = update_tasklists(&client, &tasklists).await {
                    tracing::error!("Failed to update tasklists: {:?}", e);
                }
            }
        }
    });

    // handle SIGINT and SIGTERM to unmount gracefully
    let int = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install SIGINT handler");
    };
    let term = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    let waitpids = Arc::new(Mutex::new(Vec::default()));
    tokio::select! {
        _ = int => {
            tracing::info!("Received SIGINT, unmounting…");
        }
        _ = term => {
            tracing::info!("Received SIGTERM, unmounting…");
        }
        _ = async {
            while let Some(pid) = rx_fh.recv().await {
                tracing::debug!("Live PID: {}", pid);
                let pending_fh = pending_fh.clone();
                let waitpids = waitpids.clone();
                if !waitpids.lock().unwrap().contains(&pid) {
                    // we don't know if the file handle was `release`d, so track active waitpids and don't spawn multiple
                    tracing::debug!("Spawning waitpid for PID: {}", pid);
                    tokio::spawn(async move {
                        waitpids.lock().unwrap().push(pid);
                        tracing::trace!("waiting: {:?}", waitpids.lock().unwrap());
                        if let Ok(mut wh) = waitpid_any::WaitHandle::open(pid as i32) {
                            wh.wait().unwrap();
                        }
                        tracing::debug!("Dropping PID: {}", pid);
                        pending_fh.lock().unwrap().retain(|(_ino, p), _| pid != *p);
                        waitpids.lock().unwrap().retain(|p| pid != *p);
                        tracing::trace!("waiting: {:?}", waitpids.lock().unwrap());
                    });
                }
            }
        } => {}
        _ = async {
            while let Some(wcmd) = rx_wcmd.recv().await {
                match wcmd {
                    WriteCommand::Task { tasklist_id, cmd } => {
                        let tasklist = tasklists
                            .iter()
                            .find(|tl| tl.with_meta(|m| m.tasklist().id.as_ref() == Some(&tasklist_id)))
                            .expect("Tasklist not found");
                        process_tasklist_write(&client, tasklist, cmd).await;
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
                                process_tasklist_write(
                                    &client,
                                    tasklist,
                                    TaskWrite::Insert(insert.clone()),
                                )
                                .await;
                            }
                            for (task_id, modification) in &pending.1 {
                                process_tasklist_write(
                                    &client,
                                    tasklist,
                                    TaskWrite::Modify {
                                        task_id: task_id.clone(),
                                        modification: modification.clone(),
                                    },
                                )
                                .await;
                            }
                        }

                        // give fsync, file close some time to settle
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        update_tasklist(&client, tasklist)
                            .await
                            .expect("Failed to sync tasks");
                    }
                    WriteCommand::CalendarEvent { calendar_id, cmd } => {
                        let calendar = calendars
                            .iter()
                            .find(|cal| cal.with_meta(|m| m.calendar().id.as_ref() == Some(&calendar_id)))
                            .expect("Calendar not found");
                        process_calendar_write(&client, calendar, cmd).await;
                    }
                    WriteCommand::SyncCalendar { calendar_id } => {
                        let calendar = calendars
                            .iter()
                            .find(|cal| cal.with_meta(|m| m.calendar().id.as_ref() == Some(&calendar_id)))
                            .expect("Calendar not found");
                        let mut guard = sync_tokens.lock().await;
                        let sync_token = guard
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
                                    &client,
                                    calendar,
                                    CalendarEventWrite::Insert(insert.clone()),
                                )
                                .await;
                            }
                            for (event_id, modification) in &pending.1 {
                                process_calendar_write(
                                    &client,
                                    calendar,
                                    CalendarEventWrite::Modify {
                                        event_id: event_id.clone(),
                                        modification: modification.clone(),
                                    },
                                )
                                .await;
                            }
                        }

                        // give fsync, file close some time to settle
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        let next_sync_token = update_calendar(&client, calendar, sync_token.as_deref())
                            .await
                            .expect("Failed to sync calendar");
                        if let (Some(sync_token), Some(next_sync_token)) = (sync_token, next_sync_token) {
                            *sync_token = next_sync_token;
                        }
                    }
                }
            }
        } => {
            tracing::info!("Processed write commands");
        }
    }

    Ok(())
}

async fn update_tasklists(
    client: &client::GoogleClient,
    tasklists: &[OrgTaskList],
) -> google_tasks1::Result<()> {
    tracing::info!("Polling for task updates…");
    for org_tasklist in tasklists {
        update_tasklist(client, org_tasklist).await?;
    }
    // TODO: newly created tasklists since application start are ignored
    // TODO: we don't learn about deletes made upstream
    Ok(())
}

async fn update_tasklist(
    client: &client::GoogleClient,
    org_tasklist: &OrgTaskList,
) -> google_tasks1::Result<()> {
    let tl_id = org_tasklist
        .with_meta(|m| m.tasklist().id.clone())
        .expect("tasklist with no id");
    tracing::info!("Updating tasklist {}…", tl_id);
    let tasks = client.list_tasks(&tl_id).await?;
    let updated = client
        .get_tasklist(&tl_id)
        .await?
        .updated
        .as_ref()
        .and_then(|str| {
            chrono::DateTime::parse_from_rfc3339(str)
                .ok()
                .map(|dt| dt.into())
        })
        .unwrap_or(std::time::UNIX_EPOCH);
    org_tasklist.sync(tasks, updated);
    Ok(())
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
            if let Ok(new) = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                client.insert_task(&tasklist_id, *task.clone()),
            )
            .await
            .unwrap_or_else(|e| Err(google_tasks1::Error::Io(e.into())))
            {
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

#[allow(clippy::type_complexity)]
async fn update_calendars(
    client: &client::GoogleClient,
    calendars: &[OrgCalendar],
    sync_tokens: &mut Arc<tokio::sync::Mutex<Vec<(String, Option<client::SyncToken>)>>>,
) -> google_calendar3::Result<()> {
    let new_sync_tokens = Arc::new(tokio::sync::Mutex::new(Vec::default()));
    {
        tracing::info!("Polling for calendar updates…");
        for org_calendar in calendars {
            let cal_id = org_calendar
                .with_meta(|m| m.calendar().id.clone())
                .expect("calendar with no id");
            let guard = sync_tokens.lock().await;
            let sync_token = guard
                .iter()
                .find(|(id, _)| id == &cal_id)
                .and_then(|(_, token)| token.as_ref());
            let new_sync_token = update_calendar(client, org_calendar, sync_token).await?;
            new_sync_tokens
                .lock()
                .await
                .push((cal_id.clone(), new_sync_token));
        }
    }
    *sync_tokens = new_sync_tokens;
    Ok(())
}

async fn update_calendar(
    client: &client::GoogleClient,
    org_calendar: &OrgCalendar,
    sync_token: Option<&client::SyncToken>,
) -> google_calendar3::Result<Option<client::SyncToken>> {
    let cal_id = org_calendar
        .with_meta(|m| m.calendar().id.clone())
        .expect("calendar with no id");
    let events = match sync_token {
        Some(sync_token) => {
            tracing::info!("Syncing calendar {} with token {}", cal_id, sync_token);
            client
                .list_events_with_sync_token(cal_id.as_ref(), sync_token)
                .await?
        }
        _ => {
            tracing::info!("Syncing calendar {} without token", cal_id);
            client.list_events(cal_id.as_ref()).await?
        }
    };
    let next_sync_token = events.next_sync_token.as_ref().cloned();
    let updated = events
        .updated
        .map(|dt| dt.into())
        .unwrap_or(std::time::UNIX_EPOCH);
    org_calendar.sync(events, updated);
    Ok(next_sync_token)
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
