//! The `grafeno` command: serves a Grafeno database over HTTP or MCP, runs
//! it as a service, and manages it from a console.

use clap::{Args, Parser, Subcommand};
use grafeno::Grafeno;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use grafeno_server::config::{self, Config};
use grafeno_server::mcp;
use grafeno_server::service::{self, Overrides, ServerSettings};
use grafeno_server::shell::{Backend, Shell, read_script, stdin_script};

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
    /// Serve the JSON HTTP API and Grafeno Studio (at /studio/).
    Serve {
        /// Settings file (`grafeno.toml`); flags and environment variables
        /// take precedence over it.
        #[arg(long, env = "GRAFENO_CONFIG")]
        config: Option<PathBuf>,
        /// Database file, created if missing. Default: grafeno.redb.
        #[arg(long, env = "GRAFENO_DB")]
        db: Option<PathBuf>,
        /// Keep everything in memory instead of a file (lost on exit).
        #[arg(long, conflicts_with = "db")]
        memory: bool,
        /// Address to listen on. Default: 127.0.0.1:7810.
        #[arg(long, env = "GRAFENO_ADDR")]
        addr: Option<SocketAddr>,
        /// If set, every `/v1` request needs `Authorization: Bearer <token>`.
        #[arg(long, env = "GRAFENO_TOKEN", hide_env_values = true)]
        token: Option<String>,
        /// Do not serve Grafeno Studio.
        #[arg(long)]
        no_studio: bool,
        /// Allow browsers on this origin to call the API (repeatable; `*`
        /// for any), such as a Studio hosted elsewhere.
        #[arg(long = "cors-origin")]
        cors_origins: Vec<String>,
    },
    /// Create or read the settings file of an installed server.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Run Grafeno as a Windows service.
    #[command(subcommand)]
    Service(ServiceCommand),
    /// Serve MCP (Model Context Protocol) over stdin and stdout.
    Mcp {
        #[command(flatten)]
        db: DbArgs,
        /// Namespace used by tools that do not name one.
        #[arg(long, env = "GRAFENO_NAMESPACE", default_value = "default")]
        namespace: String,
    },
    /// An interactive console for GQ queries and administration, like psql.
    Shell {
        /// Connect to the server of this settings file (its address and token).
        #[arg(long, conflicts_with_all = ["url", "db", "memory"])]
        config: Option<PathBuf>,
        /// Database file to open directly (stop any server using it first).
        #[arg(long, conflicts_with_all = ["url", "memory"])]
        db: Option<PathBuf>,
        /// Use a throwaway in-memory database.
        #[arg(long, conflicts_with = "url")]
        memory: bool,
        /// URL of a running `grafeno serve`, such as http://127.0.0.1:7810.
        #[arg(long, env = "GRAFENO_URL")]
        url: Option<String>,
        /// Bearer token of the server.
        #[arg(long, env = "GRAFENO_TOKEN", hide_env_values = true)]
        token: Option<String>,
        #[arg(
            long,
            short = 'n',
            env = "GRAFENO_NAMESPACE",
            default_value = "default"
        )]
        namespace: String,
        /// Run these statements (and meta-commands) and exit.
        #[arg(long, short = 'c', conflicts_with = "file")]
        command: Option<String>,
        /// Run the statements in this file (`-` for standard input) and exit.
        #[arg(long, short = 'f')]
        file: Option<String>,
        /// Print results as JSON instead of tables.
        #[arg(long)]
        json: bool,
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
        /// Load in chunks of 20,000 records with a single flush at the end:
        /// faster for large exports, but an error leaves the chunks before
        /// it in place.
        #[arg(long)]
        bulk: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Write a settings file with a new random token.
    Init {
        /// Where to write it. Default: the system location (`grafeno config path`).
        #[arg(long)]
        path: Option<PathBuf>,
        /// Database file of the server. Default: grafeno.redb in a data folder next to the settings.
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value = grafeno_server::config::DEFAULT_ADDR)]
        addr: SocketAddr,
        /// Replace an existing file (and its token).
        #[arg(long)]
        force: bool,
    },
    /// Print where installers keep the settings on this system.
    Path,
    /// Print the token of a settings file (for Grafeno Studio or clients).
    Token {
        #[arg(long)]
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Register the `Grafeno` service and start it (as administrator).
    Install {
        /// Settings file the service reads. Default: the system location.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Register it without starting it.
        #[arg(long)]
        no_start: bool,
    },
    /// Stop and remove the service (as administrator).
    Uninstall,
    /// Entry point used by Windows to run the service.
    #[command(hide = true)]
    Run {
        #[arg(long)]
        config: Option<PathBuf>,
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
        Command::Serve {
            config,
            db,
            memory,
            addr,
            token,
            no_studio,
            cors_origins,
        } => {
            let config = config.map(|path| Config::load(&path)).transpose()?;
            let overrides = Overrides {
                db,
                memory,
                addr,
                token,
                no_studio,
                cors_origins,
            };
            service::run_until_signal(ServerSettings::resolve(config, overrides))?;
        }
        Command::Config(command) => match command {
            ConfigCommand::Init {
                path,
                db,
                addr,
                force,
            } => {
                let path = path.unwrap_or_else(config::default_path);
                let db = db.unwrap_or_else(|| {
                    path.parent()
                        .unwrap_or_else(|| std::path::Path::new("."))
                        .join("data")
                        .join("grafeno.redb")
                });
                config::init(&path, &db, addr, force)?;
                println!("Wrote {}.", path.display());
            }
            ConfigCommand::Path => println!("{}", config::default_path().display()),
            ConfigCommand::Token { path } => {
                let path = path.unwrap_or_else(config::default_path);
                match Config::load(&path)?.token {
                    Some(token) => println!("{token}"),
                    None => return Err(format!("{} has no token", path.display()).into()),
                }
            }
        },
        Command::Service(command) => service_command(command)?,
        Command::Mcp { db, namespace } => mcp::run(db.open()?, namespace)?,
        Command::Shell {
            config,
            db,
            memory,
            url,
            token,
            namespace,
            command,
            file,
            json,
        } => {
            let (url, token) = match config {
                Some(path) => {
                    let config = Config::load(&path)?;
                    (Some(config.local_url()), token.or(config.token))
                }
                None => (url, token),
            };
            let backend = match url {
                Some(url) => Backend::remote(&url, token.filter(|t| !t.is_empty())),
                None if memory => Backend::Local(Grafeno::in_memory()?),
                None => Backend::Local(Grafeno::open(
                    db.unwrap_or_else(|| PathBuf::from("grafeno.redb")),
                )?),
            };
            let mut shell = Shell::new(backend, namespace, json);
            if let Some(command) = command {
                shell.run_script(&command);
            } else if let Some(file) = file {
                shell.run_script(&read_script(&file)?);
            } else if let Some(script) = stdin_script() {
                shell.run_script(&script);
            } else {
                shell.interactive()?;
            }
            if shell.failed {
                return Err("some statements failed".into());
            }
        }
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
        Command::Import { db, input, bulk } => {
            let db = db.open()?;
            let options = if bulk {
                grafeno::ImportOptions::bulk()
            } else {
                grafeno::ImportOptions::default()
            };
            let stats = if input.as_os_str() == "-" {
                db.import_with(std::io::stdin().lock(), &options)?
            } else {
                db.import_with(
                    std::io::BufReader::new(std::fs::File::open(input)?),
                    &options,
                )?
            };
            eprintln!(
                "imported {} namespaces, {} nodes, {} edges",
                stats.namespaces, stats.nodes, stats.edges
            );
        }
    }
    Ok(())
}

#[cfg(windows)]
fn service_command(command: ServiceCommand) -> Result<(), Box<dyn std::error::Error>> {
    use grafeno_server::service::windows;
    match command {
        ServiceCommand::Install { config, no_start } => {
            let config = config.unwrap_or_else(config::default_path);
            if !config.exists() {
                return Err(format!(
                    "{} does not exist; create it with `grafeno config init`",
                    config.display()
                )
                .into());
            }
            windows::install(&config, !no_start)?;
            println!(
                "Installed the {} service{}.",
                windows::NAME,
                if no_start { "" } else { " and started it" }
            );
        }
        ServiceCommand::Uninstall => {
            windows::uninstall()?;
            println!("Removed the {} service.", windows::NAME);
        }
        ServiceCommand::Run { config } => {
            windows::dispatch(config.unwrap_or_else(config::default_path))?;
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn service_command(command: ServiceCommand) -> Result<(), Box<dyn std::error::Error>> {
    let _ = command;
    Err(
        "`grafeno service` is for Windows. On Linux the packages install a systemd unit: \
         `systemctl enable --now grafeno` (or use install.sh --service)."
            .into(),
    )
}
