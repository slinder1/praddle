use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    owner: String,
    repository: String,
    git_dir: PathBuf,
    #[arg(long)]
    socket: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    praddle_test_server::serve(args.owner, args.repository, args.git_dir, &args.socket).await
}
