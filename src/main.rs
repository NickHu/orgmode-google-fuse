use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use clap::Parser;
use fuse::OrgFS;
use fuser::MountOption;
use futures::{stream, StreamExt};
use tokio::sync::Notify;

use crate::{
    org::{calendar::OrgCalendar, tasklist::OrgTaskList, MetaPendingContainer},
    write::{process_write, WriteCommand},
};

mod client;
mod fuse;
mod oauth;
mod org;
mod write;

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
            tx_wcmd.clone(),
            tx_fh,
            pending_fh.clone(),
        ),
        &args.mount,
        &[MountOption::FSName("orgmode-google-fuse".to_string())],
    )?;

    // spawn background task to poll for calendars updates
    let trigger_calendar_update = Arc::new(Notify::new());
    tokio::spawn({
        let calendars = calendars.clone();
        let tx_wcmd = tx_wcmd.clone();
        let trigger_calendar_update = trigger_calendar_update.clone();
        async move {
            let mut interval = tokio::time::interval(POLL_INTERVAL);
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = trigger_calendar_update.notified() => { interval.reset() }
                }
                tracing::info!("Polling for calendar updates…");
                for calendar in calendars.iter() {
                    let calendar_id = calendar
                        .with_meta(|m| m.calendar().id.clone())
                        .expect("calendar with no id");
                    tx_wcmd
                        .send(WriteCommand::SyncCalendar { calendar_id })
                        .unwrap();
                }
            }
        }
    });

    // spawn background task to poll for tasks updates
    let trigger_tasklist_update = Arc::new(Notify::new());
    tokio::spawn({
        let tasklists = tasklists.clone();
        let tx_wcmd = tx_wcmd.clone();
        let trigger_tasklist_update = trigger_tasklist_update.clone();
        async move {
            let mut interval = tokio::time::interval(POLL_INTERVAL);
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = trigger_tasklist_update.notified() => { interval.reset() }
                }
                tracing::info!("Polling for task updates…");
                for tasklist in tasklists.iter() {
                    let tasklist_id = tasklist
                        .with_meta(|m| m.tasklist().id.clone())
                        .expect("tasklist with no id");
                    tx_wcmd
                        .send(WriteCommand::SyncTasklist { tasklist_id })
                        .unwrap();
                }
            }
        }
    });

    loop {
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
        let hup = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .expect("failed to install SIGHUP handler")
                .recv()
                .await;
        };
        let waitpids = Arc::new(Mutex::new(Vec::default()));
        tokio::select! {
            _ = int => {
                tracing::info!("Received SIGINT, unmounting…");
                break;
            }
            _ = term => {
                tracing::info!("Received SIGTERM, unmounting…");
                break;
            }
            _ = hup => {
                tracing::info!("Received SIGHUP, triggering sync…");
                trigger_calendar_update.notify_waiters();
                trigger_tasklist_update.notify_waiters();
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
                    process_write(&client, &calendars, &mut sync_tokens.lock().await, &tasklists, wcmd).await;
                }
            } => {
                tracing::info!("Processed write commands");
            }
        }
    }

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
