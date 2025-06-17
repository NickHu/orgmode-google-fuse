use clap::Parser;
use fuse::OrgFS;
use fuser::MountOption;
use futures::{stream, StreamExt};

mod client;
mod fuse;
mod oauth;
mod org;

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
    let client = client::GoogleClient::new().await;

    let cs = client.list_calendars().await.unwrap();
    let calendars = stream::iter(cs.into_iter())
        .filter_map(|cal| async {
            let events = client.list_events(cal.id.as_ref().unwrap()).await.ok()?;
            Some((cal, events).into())
        })
        .collect()
        .await;

    let tls = client.list_tasklists().await.unwrap();
    let tasklists = stream::iter(tls.into_iter())
        .filter_map(|tl| async {
            let tasks = client.list_tasks(tl.id.as_ref().unwrap()).await.ok()?;
            Some((tl, tasks).into())
        })
        .collect()
        .await;

    let _handle = fuser::spawn_mount2(
        OrgFS::new(calendars, tasklists),
        &args.mount,
        &[MountOption::FSName("orgmode-google-fuse".to_string())],
    )?;

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
    }

    Ok(())
}
