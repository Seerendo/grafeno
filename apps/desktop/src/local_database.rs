//! Sesiones locales sin dependencia de la ventana: permite probar aperturas,
//! cambios de archivo y cierres sin iniciar una sesión gráfica.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use grafeno::Grafeno;
use grafeno_server::http::{AppState, Options, router_with};
use serde::Serialize;

pub(super) const LOCAL_URL: &str = "http://grafeno.local";

pub(super) struct Session {
    pub path: PathBuf,
    pub db: Arc<Grafeno>,
    pub router: Router,
    // El token propietario evita que las claves de la base bloqueen a la app.
    pub token: String,
}

#[derive(Debug, Serialize)]
pub(super) struct SessionInfo {
    pub url: &'static str,
    pub path: String,
    pub name: String,
}

pub(super) fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

pub(super) fn info(session: &Session) -> SessionInfo {
    SessionInfo {
        url: LOCAL_URL,
        path: session.path.display().to_string(),
        name: file_name(&session.path),
    }
}

pub(super) fn router_for(db: Arc<Grafeno>, token: Option<&str>) -> Router {
    router_with(
        AppState::new(db, token),
        &Options {
            studio: false,
            cors_origins: Vec::new(),
        },
    )
}

/// El llamador mantiene el bloqueo de la sesión durante toda la operación.
/// Una apertura fallida conserva el archivo, el router y el token anteriores.
pub(super) fn open_session(
    current: &mut Option<Session>,
    path: PathBuf,
) -> Result<SessionInfo, String> {
    let path = std::path::absolute(&path)
        .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
    let path = path.canonicalize().unwrap_or(path);
    if let Some(session) = current.as_ref()
        && session.path == path
    {
        return Ok(info(session));
    }

    // Preparar el reemplazo antes de soltar la base actual. Crear también
    // está permitido: el diálogo de archivo decide entre abrir y crear.
    let db = Grafeno::open(&path).map_err(|error| {
        let text = error.to_string();
        let normalized = text.to_ascii_lowercase();
        if normalized.contains("lock") || normalized.contains("already open") {
            format!(
                "{} is open in another program (a Grafeno service or another window)",
                path.display()
            )
        } else {
            format!("cannot open {}: {text}", path.display())
        }
    })?;
    // Canonicalizar después de crear hace que futuras aperturas mediante
    // rutas equivalentes o enlaces simbólicos reconozcan la misma sesión.
    let path = path.canonicalize().unwrap_or(path);
    let token = grafeno_server::auth::new_secret();
    let db = Arc::new(db);
    let router = router_for(db.clone(), Some(&token));
    let next = Session {
        path,
        db,
        router,
        token,
    };
    let reply = info(&next);
    *current = Some(next);
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeno::{NewNode, NodeId};
    use std::io::{BufRead, Write};
    use std::process::{Child, Command, Stdio};

    struct Files(PathBuf);

    impl Files {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("grafeno-local-{}", NodeId::new()));
            std::fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn insert(session: &Session, name: &str) -> NodeId {
        session
            .db
            .write(|tx| tx.create_node("lab", NewNode::new("Person").prop("name", name)))
            .unwrap()
    }

    #[test]
    fn create_close_and_reopen_preserves_committed_data() {
        let files = Files::new();
        let path = files.path("mi base de investigación.redb");
        let mut current = None;
        let opened = open_session(&mut current, path.clone()).unwrap();
        assert_eq!(opened.name, "mi base de investigación.redb");
        assert_eq!(opened.url, LOCAL_URL);
        let id = insert(current.as_ref().unwrap(), "Ana");
        drop(current.take());

        // El cierre debe liberar el archivo para otros consumidores.
        let external = Grafeno::open(&path).unwrap();
        assert_eq!(
            external
                .begin_read()
                .unwrap()
                .node("lab", id)
                .unwrap()
                .unwrap()
                .props["name"],
            "Ana"
        );
        drop(external);
        open_session(&mut current, path).unwrap();
        assert!(
            current
                .as_ref()
                .unwrap()
                .db
                .begin_read()
                .unwrap()
                .node("lab", id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn switching_files_keeps_their_data_separate_and_releases_the_previous_file() {
        let files = Files::new();
        let first = files.path("primera.redb");
        let second = files.path("segunda.redb");
        let mut current = None;
        open_session(&mut current, first.clone()).unwrap();
        let ana = insert(current.as_ref().unwrap(), "Ana");
        open_session(&mut current, second.clone()).unwrap();
        let bob = insert(current.as_ref().unwrap(), "Bob");
        assert!(
            current
                .as_ref()
                .unwrap()
                .db
                .begin_read()
                .unwrap()
                .node("lab", ana)
                .unwrap()
                .is_none()
        );
        let previous = Grafeno::open(&first).unwrap();
        assert!(
            previous
                .begin_read()
                .unwrap()
                .node("lab", ana)
                .unwrap()
                .is_some()
        );
        drop(previous);
        open_session(&mut current, first).unwrap();
        assert!(
            current
                .as_ref()
                .unwrap()
                .db
                .begin_read()
                .unwrap()
                .node("lab", bob)
                .unwrap()
                .is_none()
        );
        assert!(Grafeno::open(second).is_ok());
    }

    #[test]
    fn reopening_the_same_file_preserves_the_session_and_token() {
        let files = Files::new();
        let path = files.path("actual.redb");
        let mut current = None;
        open_session(&mut current, path.clone()).unwrap();
        let original = current.as_ref().unwrap().db.clone();
        let token = current.as_ref().unwrap().token.clone();
        open_session(&mut current, path).unwrap();
        open_session(&mut current, files.0.join(".").join("actual.redb")).unwrap();
        assert!(Arc::ptr_eq(&original, &current.as_ref().unwrap().db));
        assert_eq!(token, current.as_ref().unwrap().token);
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_to_the_current_file_reuses_the_session() {
        let files = Files::new();
        let path = files.path("actual.redb");
        let link = files.path("enlace.redb");
        let mut current = None;
        open_session(&mut current, path.clone()).unwrap();
        let token = current.as_ref().unwrap().token.clone();
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let reply = open_session(&mut current, link).unwrap();
        assert_eq!(
            reply.path,
            path.canonicalize().unwrap().display().to_string()
        );
        assert_eq!(token, current.as_ref().unwrap().token);
    }

    #[test]
    fn a_failed_first_open_leaves_no_session() {
        let files = Files::new();
        let mut current = None;
        let error = open_session(&mut current, files.path("ausente/base.redb")).unwrap_err();
        assert!(error.contains("cannot open"), "{error}");
        assert!(current.is_none());
    }

    #[test]
    fn failed_opens_preserve_the_current_database_and_the_rejected_file() {
        let files = Files::new();
        let invalid = files.path("invalida.redb");
        let bytes = b"Este archivo no es una base redb";
        std::fs::write(&invalid, bytes).unwrap();
        let mut current = None;
        open_session(&mut current, files.path("actual.redb")).unwrap();
        let original = current.as_ref().unwrap().db.clone();
        let token = current.as_ref().unwrap().token.clone();
        let id = insert(current.as_ref().unwrap(), "Ana");
        for rejected in [
            invalid.clone(),
            files.0.clone(),
            files.path("no existe/base.redb"),
        ] {
            let error = open_session(&mut current, rejected.clone()).unwrap_err();
            assert!(error.contains("cannot open"), "{error}");
            assert!(error.contains(&rejected.display().to_string()), "{error}");
            assert!(Arc::ptr_eq(&original, &current.as_ref().unwrap().db));
            assert_eq!(token, current.as_ref().unwrap().token);
            assert!(
                current
                    .as_ref()
                    .unwrap()
                    .db
                    .begin_read()
                    .unwrap()
                    .node("lab", id)
                    .unwrap()
                    .is_some()
            );
        }
        insert(current.as_ref().unwrap(), "Sigue disponible");
        assert_eq!(std::fs::read(invalid).unwrap(), bytes);
    }

    #[test]
    fn a_truncated_database_does_not_replace_the_current_session() {
        let files = Files::new();
        let damaged = files.path("truncada.redb");
        let db = Grafeno::open(&damaged).unwrap();
        db.write(|tx| tx.create_node("lab", NewNode::new("Person")))
            .unwrap();
        drop(db);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&damaged)
            .unwrap()
            .set_len(64)
            .unwrap();
        let mut current = None;
        open_session(&mut current, files.path("actual.redb")).unwrap();
        let token = current.as_ref().unwrap().token.clone();
        let error = open_session(&mut current, damaged).unwrap_err();
        assert!(error.contains("cannot open"), "{error}");
        assert_eq!(token, current.as_ref().unwrap().token);
        insert(current.as_ref().unwrap(), "Sigue disponible");
    }

    struct LockHolder(Child);

    impl Drop for LockHolder {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    #[ignore = "proceso auxiliar de la prueba de bloqueo"]
    fn hold_database_lock() {
        let path = std::env::var_os("GRAFENO_TEST_LOCK_PATH").unwrap();
        let _db = Grafeno::open(PathBuf::from(path)).unwrap();
        println!("GRAFENO_DATABASE_LOCKED");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    }

    #[test]
    fn a_file_locked_by_another_process_does_not_replace_the_current_session() {
        let files = Files::new();
        let locked = files.path("ocupada.redb");
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "local_database::tests::hold_database_lock",
                "--ignored",
                "--nocapture",
            ])
            .env("GRAFENO_TEST_LOCK_PATH", &locked)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut holder = LockHolder(child);
        let mut output = std::io::BufReader::new(holder.0.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert!(
                output.read_line(&mut line).unwrap() > 0,
                "el proceso no confirmó el bloqueo"
            );
            if line.contains("GRAFENO_DATABASE_LOCKED") {
                break;
            }
        }
        let mut current = None;
        open_session(&mut current, files.path("actual.redb")).unwrap();
        let token = current.as_ref().unwrap().token.clone();
        let error = open_session(&mut current, locked.clone()).unwrap_err();
        assert!(error.contains("open in another program"), "{error}");
        assert!(error.contains(&locked.display().to_string()), "{error}");
        assert_eq!(token, current.as_ref().unwrap().token);
        insert(current.as_ref().unwrap(), "Sigue disponible");
        drop(holder);
        open_session(&mut current, locked).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_preserves_the_current_session() {
        use std::os::unix::fs::PermissionsExt;
        let files = Files::new();
        let read_only = files.path("solo lectura.redb");
        drop(Grafeno::open(&read_only).unwrap());
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o444)).unwrap();
        // Los procesos privilegiados eluden este permiso; CI usa un usuario normal.
        if std::fs::OpenOptions::new()
            .write(true)
            .open(&read_only)
            .is_ok()
        {
            eprintln!(
                "prueba de permisos omitida: el usuario puede escribir archivos de solo lectura"
            );
            return;
        }
        let mut current = None;
        open_session(&mut current, files.path("actual.redb")).unwrap();
        let token = current.as_ref().unwrap().token.clone();
        let error = open_session(&mut current, read_only).unwrap_err();
        assert!(error.contains("cannot open"), "{error}");
        assert_eq!(token, current.as_ref().unwrap().token);
        insert(current.as_ref().unwrap(), "Sigue disponible");
    }
}
