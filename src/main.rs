use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use clap::Parser;
use fuse::OrgFS;
use fuser::MountOption;
use futures::{stream, StreamExt};

use crate::{client::WriteCommand, org::tasklist::OrgTaskList};

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
    let mut sync_tokens = Arc::new(tokio::sync::Mutex::new(Vec::default()));
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
        async move {
            let mut interval = tokio::time::interval(POLL_INTERVAL);
            loop {
                interval.tick().await;
                tracing::info!("Polling for calendar updates…");
                let new_sync_tokens = Arc::new(tokio::sync::Mutex::new(Vec::default()));
                {
                    stream::iter(sync_tokens.lock().await.iter())
                        .filter_map(|(id, sync_token)| async {
                            calendars
                                .iter()
                                .find(|x| x.meta().calendar().id.as_ref() == Some(id))
                                .map(|x| (x, id.clone(), sync_token.clone()))
                        })
                        .for_each(|(org_calendar, id, sync_token)| {
                            let client = client.clone();
                            let new_sync_tokens = new_sync_tokens.clone();
                            async move {
                                let events = match sync_token {
                                    Some(sync_token) => {
                                        tracing::debug!(
                                            "Syncing calendar {} with token {}",
                                            id,
                                            sync_token
                                        );
                                        match client
                                            .list_events_with_sync_token(id.as_ref(), &sync_token)
                                            .await
                                        {
                                            Ok(events) => events,
                                            Err(e) => {
                                                tracing::error!(
                                                    "Failed to list events for calendar {}: {}",
                                                    id,
                                                    e
                                                );
                                                return;
                                            }
                                        }
                                    }
                                    _ => {
                                        tracing::debug!("Syncing calendar {} without token", id);
                                        match client.list_events(id.as_ref()).await {
                                            Ok(events) => events,
                                            Err(e) => {
                                                tracing::error!(
                                                    "Failed to list events for calendar {}: {}",
                                                    id,
                                                    e
                                                );
                                                return;
                                            }
                                        }
                                    }
                                };
                                let new_sync_token = events.next_sync_token.as_ref().cloned();
                                org_calendar.sync(events);
                                new_sync_tokens.lock().await.push((id, new_sync_token));
                            }
                        })
                        .await;
                }
                sync_tokens = new_sync_tokens;
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
                    WriteCommand::InsertTask { tasklist_id, task } => {
                        client
                            .insert_task(&tasklist_id, task)
                            .await
                            .expect("Failed to insert task");
                    }
                    WriteCommand::PatchTask {
                        tasklist_id,
                        task_id,
                        task,
                    } => {
                        client
                            .patch_task(&tasklist_id, &task_id, task)
                            .await
                            .expect("Failed to patch task");
                    }
                    WriteCommand::DeleteTask {
                        tasklist_id,
                        task_id,
                    } => {
                        client
                            .delete_task(&tasklist_id, &task_id)
                            .await
                            .expect("Failed to delete task");
                        // the server won't tell us about the deletion, so manually remove it here
                        let tasklist = tasklists
                            .iter()
                            .find(|tl| tl.meta().tasklist().id.as_ref() == Some(&tasklist_id))
                            .expect("Tasklist not found");
                        tasklist.delete_id(&task_id);
                    }
                    WriteCommand::SyncTasklist { tasklist_id } => {
                        let tasklist = tasklists
                            .iter()
                            .find(|tl| tl.meta().tasklist().id.as_ref() == Some(&tasklist_id))
                            .expect("Tasklist not found");
                        // give fsync, file close some time to settle
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        update_tasklist(&client, tasklist)
                            .await
                            .expect("Failed to sync tasks");
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
    let meta = org_tasklist.meta();
    let tl_id = meta.tasklist().id.as_ref().expect("tasklist with no id");
    tracing::info!("Updating tasklist {}…", tl_id);
    let tasks = client.list_tasks(tl_id).await?;
    let updated = client
        .get_tasklist(tl_id)
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
