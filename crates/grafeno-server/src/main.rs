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
    /// List the namespaces with their node and edge counts.
    Namespaces {
        #[command(flatten)]
        db: FileArgs,
    },
    /// Print statistics as JSON (of every namespace, or of the given ones).
    Stats {
        #[command(flatten)]
        db: FileArgs,
        #[arg(long = "namespace", short = 'n')]
        namespaces: Vec<String>,
    },
    /// Check that indexes and counters agree with the records. Fails if not.
    Check {
        #[command(flatten)]
        db: FileArgs,
        #[arg(long = "namespace", short = 'n')]
        namespaces: Vec<String>,
    },
    /// Rebuild the indexes and counters of namespaces from their records.
    Rebuild {
        #[command(flatten)]
        db: FileArgs,
        #[arg(long = "namespace", short = 'n', required_unless_present = "all")]
        namespaces: Vec<String>,
        /// Rebuild every namespace.
        #[arg(long, conflicts_with = "namespaces")]
        all: bool,
    },
    /// Write namespaces (all by default) as a JSON Lines export.
    Export {
        #[command(flatten)]
        db: FileArgs,
        #[arg(long = "namespace", short = 'n')]
        namespaces: Vec<String>,
        /// Output file; standard output when absent.
        #[arg(long, short = 'o')]
        output: Option<PathBuf>,
    },
    /// Import a JSON Lines export in one transaction.
    Import {
        #[command(flatten)]
        db: FileArgs,
        /// Export file, or `-` for standard input.
        input: PathBuf,
    },
}

/// The database file of an administration command. Stop any server using
/// it first: only one process can open it.
#[derive(Args)]
struct FileArgs {
    #[arg(long, env = "GRAFENO_DB", default_value = "grafeno.redb")]
    db: PathBuf,
}

impl FileArgs {
    fn open(&self) -> grafeno::Result<Grafeno> {
        Grafeno::open(&self.db)
    }
}

/// The given namespaces, or every namespace when none is given.
fn namespaces_or_all(db: &Grafeno, namespaces: Vec<String>) -> grafeno::Result<Vec<String>> {
    if !namespaces.is_empty() {
        return Ok(namespaces);
    }
    Ok(db
        .begin_read()?
        .namespaces()?
        .into_iter()
        .map(|info| info.name)
        .collect())
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
        Command::Namespaces { db } => {
            let namespaces = db.open()?.begin_read()?.namespaces()?;
            let width = namespaces
                .iter()
                .map(|info| info.name.len())
                .max()
                .unwrap_or(0)
                .max(9);
            println!("{:width$}  {:>10}  {:>10}", "namespace", "nodes", "edges");
            for info in namespaces {
                println!(
                    "{:width$}  {:>10}  {:>10}",
                    info.name, info.nodes, info.edges
                );
            }
        }
        Command::Stats { db, namespaces } => {
            let db = db.open()?;
            let read = db.begin_read()?;
            let stats = namespaces_or_all(&db, namespaces)?
                .iter()
                .map(|ns| read.stats(ns))
                .collect::<grafeno::Result<Vec<_>>>()?;
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }
        Command::Check { db, namespaces } => {
            let db = db.open()?;
            let read = db.begin_read()?;
            let mut failed = 0;
            for ns in namespaces_or_all(&db, namespaces)? {
                let report = read.check(&ns)?;
                let summary = format!(
                    "{} nodes, {} edges, {} texts, {} vectors, {} dangling links",
                    report.nodes, report.edges, report.texts, report.vectors, report.dangling_links
                );
                if report.is_ok() {
                    println!("{ns}: ok ({summary})");
                } else {
                    failed += 1;
                    println!("{ns}: {} problems ({summary})", report.problems.len());
                    for problem in &report.problems {
                        println!("  - {problem}");
                    }
                }
            }
            if failed > 0 {
                return Err(format!(
                    "{failed} namespace(s) have problems; `grafeno rebuild` can repair indexes"
                )
                .into());
            }
        }
        Command::Rebuild {
            db,
            namespaces,
            all: _,
        } => {
            let db = db.open()?;
            for ns in namespaces_or_all(&db, namespaces)? {
                let report = db.write(|tx| tx.rebuild_indexes(&ns))?;
                println!("{}", serde_json::to_string(&report)?);
            }
        }
        Command::Export {
            db,
            namespaces,
            output,
        } => {
            let db = db.open()?;
            let read = db.begin_read()?;
            let stats = match output {
                Some(path) => {
                    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
                    read.export(&namespaces, &mut file)?
                }
                None => read.export(&namespaces, &mut std::io::stdout().lock())?,
            };
            eprintln!(
                "exported {} namespaces, {} nodes, {} edges",
                stats.namespaces, stats.nodes, stats.edges
            );
        }
        Command::Import { db, input } => {
            let db = db.open()?;
            let stats = if input.as_os_str() == "-" {
                db.import(std::io::stdin().lock())?
            } else {
                db.import(std::io::BufReader::new(std::fs::File::open(input)?))?
            };
            eprintln!(
                "imported {} namespaces, {} nodes, {} edges",
                stats.namespaces, stats.nodes, stats.edges
            );
        }
    }
    Ok(())
}
