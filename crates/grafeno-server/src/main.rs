//! The `grafeno` command: serves a Grafeno database over HTTP or MCP.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use grafeno::Grafeno;

use grafeno_server::{http, mcp};

#[derive(Parser)]
#[command(
    name = "grafeno",
    version,
    about = "Graph engine for agent memory and knowledge"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the JSON HTTP API.
    Serve {
        #[command(flatten)]
        db: DbArgs,
        /// Address to listen on.
        #[arg(long, env = "GRAFENO_ADDR", default_value = "127.0.0.1:7810")]
        addr: SocketAddr,
        /// If set, every `/v1` request needs `Authorization: Bearer <token>`.
        #[arg(long, env = "GRAFENO_TOKEN", hide_env_values = true)]
        token: Option<String>,
    },
    /// Serve MCP (Model Context Protocol) over stdin and stdout.
    Mcp {
        #[command(flatten)]
        db: DbArgs,
        /// Namespace used by tools that do not name one.
        #[arg(long, env = "GRAFENO_NAMESPACE", default_value = "default")]
        namespace: String,
    },
}

#[derive(Args)]
struct DbArgs {
    /// Database file, created if missing. Only one process can open it at a time.
    #[arg(long, env = "GRAFENO_DB", default_value = "grafeno.redb")]
    db: PathBuf,
    /// Keep everything in memory instead of a file (lost on exit).
    #[arg(long, conflicts_with = "db")]
    memory: bool,
}

impl DbArgs {
    fn open(&self) -> grafeno::Result<Grafeno> {
        if self.memory {
            Grafeno::in_memory()
        } else {
            Grafeno::open(&self.db)
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("grafeno: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Serve { db, addr, token } => {
            let state = http::AppState {
                db: Arc::new(db.open()?),
                token: token.filter(|token| !token.is_empty()).map(Arc::from),
            };
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(http::serve(state, addr))?;
        }
        Command::Mcp { db, namespace } => mcp::run(db.open()?, namespace)?,
    }
    Ok(())
}
