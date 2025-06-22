use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use clap::Parser;
use fuse::OrgFS;
use fuser::MountOption;
use futures::{stream, StreamExt};

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

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Pid>();
    let pending_fh = Arc::new(Mutex::new(HashMap::new()));
    let _handle = fuser::spawn_mount2(
        OrgFS::new(calendars.clone(), tasklists.clone(), tx, pending_fh.clone()),
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
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        loop {
            interval.tick().await;
            tracing::info!("Polling for task updates…");
            match client.list_tasklists().await {
                Ok(tls) => {
                    stream::iter(tls.items.unwrap_or_default().into_iter())
                        .filter_map(|tl| async {
                            let tasks = client.list_tasks(tl.id.as_ref().unwrap()).await.ok()?;
                            Some((tl, tasks))
                        })
                        .for_each(|(tl, tasks)| {
                            let tasklists = tasklists.clone();
                            async move {
                                if let Some(org_tasklist) =
                                    tasklists.iter().find(|x| x.meta().tasklist().id == tl.id)
                                {
                                    org_tasklist.sync(tasks);
                                }
                            }
                        })
                        .await;
                }
                Err(e) => {
                    tracing::error!("Failed to poll tasklists: {}", e);
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
    tokio::select! {
        _ = int => {
            tracing::info!("Received SIGINT, unmounting…");
        }
        _ = term => {
            tracing::info!("Received SIGTERM, unmounting…");
        }
        _ = async {
            while let Some(pid) = rx.recv().await {
                tracing::debug!("Live PID: {}", pid);
                let pending_fh = pending_fh.clone();
                tokio::spawn(async move {
                    waitpid_any::WaitHandle::open(pid as i32).expect("Failed to open waitpid").wait().unwrap();
                    tracing::debug!("Dropping PID: {}", pid);
                    pending_fh.lock().unwrap().retain(|(_ino, p), _| pid != *p)
                });
            }
        } => {}
    }

    Ok(())
}
