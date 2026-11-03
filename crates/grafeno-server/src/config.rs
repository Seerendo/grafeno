//! `grafeno.toml`: the settings of an installed server, shared by
//! `grafeno serve --config`, the system service and `grafeno shell --config`.
//!
//! ```toml
//! db = "/var/lib/grafeno/grafeno.redb"
//! addr = "127.0.0.1:7810"
//! token = "…"            # every /v1 request needs `Authorization: Bearer <token>`
//! studio = true          # serve Grafeno Studio at /studio/
//! cors_origins = []      # origins allowed to call the API from a browser
//! ```

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_ADDR: &str = "127.0.0.1:7810";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<SocketAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cors_origins: Vec<String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        toml::from_str(&text).map_err(|error| format!("invalid {}: {error}", path.display()))
    }

    /// The URL a client on this machine uses to reach the server.
    pub fn local_url(&self) -> String {
        let addr = self
            .addr
            .unwrap_or_else(|| DEFAULT_ADDR.parse().expect("valid default address"));
        let host = if addr.ip().is_unspecified() {
            "127.0.0.1".to_owned()
        } else if addr.is_ipv6() {
            format!("[{}]", addr.ip())
        } else {
            addr.ip().to_string()
        };
        format!("http://{host}:{}", addr.port())
    }
}

/// Where installers put the configuration on this system.
pub fn default_path() -> PathBuf {
    if cfg!(windows) {
        let data = std::env::var_os("ProgramData").unwrap_or_else(|| "C:\\ProgramData".into());
        PathBuf::from(data).join("Grafeno").join("grafeno.toml")
    } else {
        PathBuf::from("/etc/grafeno/grafeno.toml")
    }
}

/// A random token of 32 URL-safe characters (192 bits).
pub fn new_token() -> Result<String, String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| format!("no random source: {error}"))?;
    Ok(bytes
        .iter()
        .map(|b| ALPHABET[(*b & 63) as usize] as char)
        .collect())
}

/// Writes a new configuration with a fresh token. Refuses to replace an
/// existing file unless `force`.
pub fn init(path: &Path, db: &Path, addr: SocketAddr, force: bool) -> Result<Config, String> {
    if path.exists() && !force {
        return Err(format!(
            "{} already exists (use --force to replace it)",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let config = Config {
        db: Some(db.to_path_buf()),
        addr: Some(addr),
        token: Some(new_token()?),
        studio: Some(true),
        cors_origins: Vec::new(),
    };
    let text = format!(
        "# Grafeno server settings. Restart the service after changing them.\n\
         # Every /v1 request needs `Authorization: Bearer <token>`; Grafeno Studio asks for it.\n\
         # `addr = \"0.0.0.0:7810\"` listens on every network interface.\n\n{}",
        toml::to_string(&config).map_err(|error| error.to_string())?
    );
    std::fs::write(path, text)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    restrict(path);
    Ok(config)
}

/// Makes the file (it holds the token) readable by its owner only.
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_writes_a_loadable_config_with_a_token() {
        let dir = std::env::temp_dir().join(format!("grafeno-config-{}", grafeno::NodeId::new()));
        let path = dir.join("grafeno.toml");
        let config = init(
            &path,
            &dir.join("data").join("g.redb"),
            "0.0.0.0:7900".parse().unwrap(),
            false,
        )
        .unwrap();
        assert_eq!(config.token.as_ref().unwrap().len(), 32);
        assert_eq!(Config::load(&path).unwrap(), config);
        assert_eq!(config.local_url(), "http://127.0.0.1:7900");
        assert!(dir.join("data").is_dir());
        assert!(
            init(
                &path,
                &dir.join("g.redb"),
                "127.0.0.1:1".parse().unwrap(),
                false
            )
            .is_err()
        );
        assert_ne!(new_token().unwrap(), new_token().unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_unknown_settings() {
        assert!(toml::from_str::<Config>("port = 1").is_err());
        let config: Config =
            toml::from_str("addr = \"[::1]:7810\"\ncors_origins = [\"*\"]").unwrap();
        assert_eq!(config.local_url(), "http://[::1]:7810");
    }
}
