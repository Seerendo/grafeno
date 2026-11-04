//! Running the HTTP server, from the command line or as a system service.
//!
//! On Windows, `grafeno service install` registers a service (started by the
//! Service Control Manager with `grafeno service run --config …`). On Linux,
//! a systemd unit runs `grafeno serve --config /etc/grafeno/grafeno.toml`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use grafeno::Grafeno;

use crate::config::{Config, DEFAULT_ADDR};
use crate::http::{self, AppState, Options};

/// Everything `grafeno serve` needs, once the config file, the environment
/// and the command line have been combined (the command line wins).
#[derive(Clone, Debug)]
pub struct ServerSettings {
    /// `None`: an in-memory database.
    pub db: Option<PathBuf>,
    pub addr: SocketAddr,
    pub token: Option<String>,
    pub options: Options,
    /// Limits for each GQ query.
    pub limits: grafeno_query::Limits,
}

/// Values given on the command line (or in the environment), if any.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub db: Option<PathBuf>,
    pub memory: bool,
    pub addr: Option<SocketAddr>,
    pub token: Option<String>,
    pub no_studio: bool,
    pub cors_origins: Vec<String>,
}

impl ServerSettings {
    pub fn resolve(config: Option<Config>, overrides: Overrides) -> Self {
        let config = config.unwrap_or_default();
        let db = if overrides.memory {
            None
        } else {
            Some(
                overrides
                    .db
                    .or(config.db)
                    .unwrap_or_else(|| PathBuf::from("grafeno.redb")),
            )
        };
        let mut cors_origins = config.cors_origins;
        cors_origins.extend(overrides.cors_origins);
        Self {
            db,
            addr: overrides
                .addr
                .or(config.addr)
                .unwrap_or_else(|| DEFAULT_ADDR.parse().expect("valid default address")),
            token: overrides
                .token
                .or(config.token)
                .filter(|token| !token.is_empty()),
            options: Options {
                studio: !overrides.no_studio && config.studio.unwrap_or(true),
                cors_origins,
            },
            limits: config.query.limits(),
        }
    }

    fn open(&self) -> grafeno::Result<Grafeno> {
        match &self.db {
            Some(path) => Grafeno::open(path),
            None => Grafeno::in_memory(),
        }
    }
}

/// Opens the database and serves until `shutdown` completes.
pub fn run(
    settings: ServerSettings,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState {
        db: Arc::new(settings.open()?),
        token: settings.token.as_deref().map(Arc::from),
        limits: settings.limits.clone(),
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(http::serve_until(
            state,
            settings.addr,
            settings.options,
            shutdown,
        ))?;
    Ok(())
}

/// Serves until Ctrl+C (or SIGTERM on Unix).
pub fn run_until_signal(settings: ServerSettings) -> Result<(), Box<dyn std::error::Error>> {
    run(settings, http::shutdown_signal())
}

#[cfg(windows)]
pub mod windows {
    //! The Windows service, named `Grafeno`.

    use std::ffi::OsString;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    use super::{Overrides, ServerSettings};
    use crate::config::Config;

    pub const NAME: &str = "Grafeno";
    const DISPLAY_NAME: &str = "Grafeno graph engine";

    static CONFIG: OnceLock<PathBuf> = OnceLock::new();

    define_windows_service!(ffi_service_main, service_main);

    /// Called by the Service Control Manager (`grafeno service run`). It
    /// fails when started from a console.
    pub fn dispatch(config: PathBuf) -> Result<(), String> {
        let _ = CONFIG.set(config);
        service_dispatcher::start(NAME, ffi_service_main).map_err(|error| {
            format!("{error} (this command is run by Windows; to serve from a console, use `grafeno serve`)")
        })
    }

    /// `grafeno.log` next to the configuration: services have no console.
    fn log(config: &Path, message: &str) {
        let path = config.with_file_name("grafeno.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{} {message}", grafeno::Timestamp::now().iso());
        }
    }

    fn service_main(_arguments: Vec<OsString>) {
        let config = CONFIG
            .get()
            .cloned()
            .unwrap_or_else(crate::config::default_path);
        if let Err(error) = run_service(&config) {
            log(&config, &format!("error: {error}"));
        }
    }

    fn run_service(config_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let stop = Mutex::new(Some(stop));
        let status = service_control_handler::register(NAME, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                if let Some(stop) = stop.lock().ok().and_then(|mut stop| stop.take()) {
                    let _ = stop.send(());
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;
        let report = |state: ServiceState, exit: u32| {
            status.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: if state == ServiceState::Running {
                    ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
                } else {
                    ServiceControlAccept::empty()
                },
                exit_code: ServiceExitCode::Win32(exit),
                checkpoint: 0,
                wait_hint: Duration::from_secs(10),
                process_id: None,
            })
        };

        let settings = Config::load(config_path)
            .map(|config| ServerSettings::resolve(Some(config), Overrides::default()));
        let result = match settings {
            Ok(settings) => {
                report(ServiceState::Running, 0)?;
                log(config_path, &format!("serving on http://{}", settings.addr));
                super::run(settings, async {
                    let _ = stopped.await;
                })
            }
            Err(error) => Err(error.into()),
        };
        if let Err(error) = &result {
            log(config_path, &format!("stopped with an error: {error}"));
        } else {
            log(config_path, "stopped");
        }
        report(ServiceState::Stopped, u32::from(result.is_err()))?;
        Ok(())
    }

    /// Registers the service (started automatically with Windows), and
    /// starts it unless `start` is false. Needs an administrator.
    pub fn install(config: &Path, start: bool) -> Result<(), Box<dyn std::error::Error>> {
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )?;
        let info = ServiceInfo {
            name: NAME.into(),
            display_name: DISPLAY_NAME.into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: std::env::current_exe()?,
            launch_arguments: vec![
                "service".into(),
                "run".into(),
                "--config".into(),
                config.into(),
            ],
            dependencies: vec![],
            account_name: None,
            account_password: None,
        };
        let service =
            manager.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)?;
        service.set_description(
            "Graph database for agent memory and knowledge. Studio: http://127.0.0.1:7810/studio/",
        )?;
        if start {
            service.start::<&str>(&[])?;
        }
        Ok(())
    }

    /// Stops (waiting up to 15 seconds) and removes the service.
    pub fn uninstall() -> Result<(), Box<dyn std::error::Error>> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(
            NAME,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        )?;
        if service.query_status()?.current_state != ServiceState::Stopped {
            let _ = service.stop();
            for _ in 0..30 {
                if service.query_status()?.current_state == ServiceState::Stopped {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        service.delete()?;
        Ok(())
    }
}
