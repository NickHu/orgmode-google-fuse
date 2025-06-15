use clap::Parser;
use fuse::OrgFS;
use fuser::MountOption;
use futures::{stream, StreamExt};

mod client;
mod fuse;
mod oauth;
mod tasklist;

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
    let tls = client.list_tasklists().await.unwrap();
    let tasklists = stream::iter(tls.into_iter())
        .then(|tl| async {
            let tasks = client.list_tasks(tl.id.as_ref().unwrap()).await.unwrap();
            (tl, tasks).into()
        })
        .collect()
        .await;

    let () = fuser::mount2(
        OrgFS::new(tasklists),
        &args.mount,
        &[MountOption::FSName("orgmode-google-fuse".to_string())],
    )?;
    Ok(())
}
