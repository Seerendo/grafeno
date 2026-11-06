//! Grafeno as a desktop application.
//!
//! The window shows Grafeno Studio (bundled by Tauri, so its settings and
//! scripts persist). In local mode the app opens a database file and runs
//! the engine inside its own process: the Studio's API calls reach the
//! HTTP router directly through [`request`], with no port open on the
//! machine. In remote mode the same bridge forwards them to a Grafeno
//! server, so no server needs to allow the app as a CORS origin.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
use std::sync::Mutex;

use axum::Router;
use axum::body::Body;
#[cfg(test)]
use grafeno::Grafeno;
use http_body_util::BodyExt;
use serde::Serialize;
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use tower::ServiceExt;

mod local_database;
#[cfg(test)]
use local_database::router_for;
use local_database::{
    Session, SessionInfo, backup_target, file_name, info, local_path, open_session, request_target,
};

/// Recent databases kept in the menu and on the start screen.
const RECENT: usize = 10;

#[derive(Default)]
struct Desktop {
    session: Mutex<Option<Session>>,
    recent: Mutex<Vec<PathBuf>>,
    agent: Mutex<Option<ureq::Agent>>,
}

#[derive(Serialize)]
struct RecentItem {
    path: String,
    name: String,
    exists: bool,
}

#[derive(Serialize)]
struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

fn recent_file(app: &AppHandle) -> Option<PathBuf> {
    Some(app.path().app_config_dir().ok()?.join("recent.json"))
}

fn load_recent(app: &AppHandle) -> Vec<PathBuf> {
    recent_file(app)
        .and_then(|file| std::fs::read(file).ok())
        .and_then(|bytes| serde_json::from_slice::<Vec<PathBuf>>(&bytes).ok())
        .unwrap_or_default()
}

fn save_recent(app: &AppHandle, recent: &[PathBuf]) {
    if let Some(file) = recent_file(app) {
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(recent) {
            let _ = std::fs::write(file, bytes);
        }
    }
}

fn set_title(app: &AppHandle, title: &str) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_title(title);
    }
}

/// Cambia de base solo si la nueva apertura tiene éxito.
fn open_path(app: &AppHandle, desktop: &Desktop, path: PathBuf) -> Result<SessionInfo, String> {
    let (reply, path) = {
        let mut current = desktop.session.lock().expect("not poisoned");
        let reply = open_session(&mut current, path)?;
        let path = current.as_ref().expect("sesión abierta").path.clone();
        (reply, path)
    };
    let mut recent = desktop.recent.lock().expect("not poisoned");
    recent.retain(|other| other != &path);
    recent.insert(0, path.clone());
    recent.truncate(RECENT);
    save_recent(app, &recent);
    drop(recent);
    set_title(app, &format!("{} — Grafeno", file_name(&path)));
    let _ = rebuild_menu(app);
    Ok(reply)
}

// ------------------------------------------------------------ commands

/// The database open in the app, if any.
#[tauri::command]
fn session(desktop: State<'_, Desktop>) -> Option<SessionInfo> {
    desktop
        .session
        .lock()
        .expect("not poisoned")
        .as_ref()
        .map(info)
}

#[tauri::command]
fn open_database(
    app: AppHandle,
    desktop: State<'_, Desktop>,
    path: String,
) -> Result<SessionInfo, String> {
    open_path(&app, &desktop, PathBuf::from(path))
}

#[tauri::command]
fn close_database(app: AppHandle, desktop: State<'_, Desktop>) {
    desktop.session.lock().expect("not poisoned").take();
    set_title(&app, "Grafeno");
}

#[tauri::command]
fn recent(desktop: State<'_, Desktop>) -> Vec<RecentItem> {
    desktop
        .recent
        .lock()
        .expect("not poisoned")
        .iter()
        .map(|path| RecentItem {
            path: path.display().to_string(),
            name: file_name(path),
            exists: path.exists(),
        })
        .collect()
}

#[tauri::command]
fn forget_recent(app: AppHandle, desktop: State<'_, Desktop>, path: String) {
    let mut recent = desktop.recent.lock().expect("not poisoned");
    recent.retain(|other| other != Path::new(&path));
    save_recent(&app, &recent);
    drop(recent);
    let _ = rebuild_menu(&app);
}

/// Asks for a database file: an existing one, or where to create a new one.
#[tauri::command]
async fn pick_database(app: AppHandle, create: bool) -> Option<String> {
    let dialog = app
        .dialog()
        .file()
        .set_title(if create {
            "New Grafeno database"
        } else {
            "Open a Grafeno database"
        })
        .add_filter("Grafeno database", &["redb", "grafeno"]);
    let picked = if create {
        dialog.set_file_name("memory.redb").blocking_save_file()
    } else {
        dialog.blocking_pick_file()
    };
    picked
        .and_then(|file| file.into_path().ok())
        .map(|path| path.display().to_string())
}

/// Asks where to save a file (exports).
#[tauri::command]
async fn pick_save(app: AppHandle, name: String, extension: String) -> Option<String> {
    app.dialog()
        .file()
        .set_file_name(&name)
        .add_filter(extension.to_uppercase(), &[extension.as_str()])
        .blocking_save_file()
        .and_then(|file| file.into_path().ok())
        .map(|path| path.display().to_string())
}

/// Asks for a file to read (imports, scripts).
#[tauri::command]
async fn pick_open(app: AppHandle, extensions: Vec<String>) -> Option<String> {
    let extensions: Vec<&str> = extensions.iter().map(String::as_str).collect();
    app.dialog()
        .file()
        .add_filter("Files", &extensions)
        .blocking_pick_file()
        .and_then(|file| file.into_path().ok())
        .map(|path| path.display().to_string())
}

/// La copia corresponde a la sesión elegida antes del diálogo. Nunca sobrescribe.
#[tauri::command]
async fn backup_database(
    desktop: State<'_, Desktop>,
    path: String,
    session_url: String,
) -> Result<u64, String> {
    let db = backup_target(
        desktop.session.lock().expect("not poisoned").as_ref(),
        &session_url,
    )?;
    tokio::task::spawn_blocking(move || {
        db.begin_read()
            .and_then(|read| read.backup(Path::new(&path)))
            .map(|stats| stats.bytes)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Recupera en un archivo nuevo sin cambiar la sesión ni modificar el respaldo.
#[tauri::command]
async fn restore_database(source: String, path: String) -> Result<u64, String> {
    tokio::task::spawn_blocking(move || {
        grafeno::Grafeno::restore_backup(Path::new(&source), Path::new(&path))
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn write_text(path: String, contents: String) -> Result<(), String> {
    std::fs::write(&path, contents).map_err(|error| format!("cannot write {path}: {error}"))
}

#[tauri::command]
async fn read_text(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|error| format!("cannot read {path}: {error}"))
}

#[tauri::command]
fn open_external(app: AppHandle, url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("only web links open outside the app".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|error| error.to_string())
}

/// A request to the open database's router, in this process.
async fn local_request(
    router: Router,
    method: &str,
    path: &str,
    headers: &BTreeMap<String, String>,
    body: Option<String>,
) -> Result<Reply, String> {
    let mut builder = axum::http::Request::builder()
        .method(method)
        .uri(if path.is_empty() { "/" } else { path });
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let request = builder
        .body(Body::from(body.unwrap_or_default()))
        .map_err(|error| error.to_string())?;
    let response = router
        .oneshot(request)
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned())))
        .collect();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|error| error.to_string())?
        .to_bytes();
    Ok(Reply {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// One HTTP request from the Studio: to the open database in-process, or
/// to a remote server.
#[tauri::command]
async fn request(
    desktop: State<'_, Desktop>,
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
) -> Result<Reply, String> {
    if let Some(path) = local_path(&url) {
        let (router, token, path) = {
            let current = desktop.session.lock().expect("not poisoned");
            request_target(current.as_ref(), path)?
        };
        let mut headers = headers;
        headers.insert("authorization".into(), format!("Bearer {token}"));
        return local_request(router, &method, &path, &headers, body).await;
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("not a web address: {url}"));
    }
    let agent = desktop
        .agent
        .lock()
        .expect("not poisoned")
        .get_or_insert_with(|| {
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .new_agent()
        })
        .clone();
    tokio::task::spawn_blocking(move || remote_request(&agent, &method, &url, &headers, body))
        .await
        .map_err(|error| error.to_string())?
}

/// A request to a Grafeno server, sent by the app (blocking).
fn remote_request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    headers: &BTreeMap<String, String>,
    body: Option<String>,
) -> Result<Reply, String> {
    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let request = builder
        .body(body.unwrap_or_default())
        .map_err(|error| error.to_string())?;
    let mut response = agent
        .run(request)
        .map_err(|error| format!("cannot reach {url}: {error}"))?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_owned())))
        .collect();
    let body = response
        .body_mut()
        .with_config()
        .limit(1 << 30)
        .read_to_string()
        .map_err(|error| format!("cannot read the response: {error}"))?;
    Ok(Reply {
        status,
        headers,
        body,
    })
}

// ------------------------------------------------------------ menu

/// Menu items whose action happens in the Studio: it gets their id as a
/// `menu` event.
const VIEWS: [(&str, &str, &str); 7] = [
    ("view:overview", "Overview", "CmdOrCtrl+1"),
    ("view:explorer", "Explorer", "CmdOrCtrl+2"),
    ("view:graph", "Graph", "CmdOrCtrl+3"),
    ("view:scripts", "Scripts", "CmdOrCtrl+4"),
    ("view:search", "Search", "CmdOrCtrl+5"),
    ("view:memory", "Memory", "CmdOrCtrl+6"),
    ("view:admin", "Administration", "CmdOrCtrl+7"),
];

fn rebuild_menu(app: &AppHandle) -> tauri::Result<()> {
    let desktop = app.state::<Desktop>();
    let recent = desktop.recent.lock().expect("not poisoned").clone();
    let mut open_recent = SubmenuBuilder::new(app, "Open recent");
    if recent.is_empty() {
        open_recent = open_recent.item(
            &MenuItemBuilder::with_id("none", "No recent databases")
                .enabled(false)
                .build(app)?,
        );
    }
    for (index, path) in recent.iter().enumerate() {
        open_recent = open_recent.item(
            &MenuItemBuilder::with_id(format!("recent:{index}"), path.display().to_string())
                .build(app)?,
        );
    }
    let file = SubmenuBuilder::new(app, "File")
        .item(
            &MenuItemBuilder::with_id("new", "New database…")
                .accelerator("CmdOrCtrl+N")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("open", "Open database…")
                .accelerator("CmdOrCtrl+O")
                .build(app)?,
        )
        .item(&open_recent.build()?)
        .item(&MenuItemBuilder::with_id("connect", "Connect to a server…").build(app)?)
        .separator()
        .item(&MenuItemBuilder::with_id("import", "Import…").build(app)?)
        .item(
            &MenuItemBuilder::with_id("export", "Export everything…")
                .accelerator("CmdOrCtrl+E")
                .build(app)?,
        )
        .separator()
        .item(
            &MenuItemBuilder::with_id("close", "Close database")
                .accelerator("CmdOrCtrl+W")
                .build(app)?,
        )
        .item(&PredefinedMenuItem::quit(app, Some("Quit"))?)
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let mut view = SubmenuBuilder::new(app, "View");
    for (id, label, accelerator) in VIEWS {
        view = view.item(
            &MenuItemBuilder::with_id(id, label)
                .accelerator(accelerator)
                .build(app)?,
        );
    }
    let view = view
        .separator()
        // Ctrl+K is the Studio's own shortcut: no accelerator, or it fires twice.
        .item(&MenuItemBuilder::with_id("palette", "Command palette (Ctrl+K)").build(app)?)
        .item(&MenuItemBuilder::with_id("theme:dark", "Dark theme").build(app)?)
        .item(&MenuItemBuilder::with_id("theme:light", "Light theme").build(app)?)
        .item(&MenuItemBuilder::with_id("theme:system", "System theme").build(app)?)
        .separator()
        .item(
            &MenuItemBuilder::with_id("reload", "Reload")
                .accelerator("CmdOrCtrl+R")
                .build(app)?,
        )
        .item(&PredefinedMenuItem::fullscreen(app, None)?)
        .build()?;
    let help = SubmenuBuilder::new(app, "Help")
        .item(&MenuItemBuilder::with_id("help:docs", "Query language (GQ)").build(app)?)
        .item(&MenuItemBuilder::with_id("help:repo", "Grafeno on GitHub").build(app)?)
        .build()?;
    let menu = MenuBuilder::new(app)
        .items(&[&file, &edit, &view, &help])
        .build()?;
    app.set_menu(menu)?;
    Ok(())
}

fn on_menu(app: &AppHandle, id: &str) {
    let desktop = app.state::<Desktop>();
    match id {
        "reload" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.eval("location.reload()");
            }
        }
        "help:docs" => {
            let _ = app.opener().open_url(
                "https://github.com/Seerendo/grafeno/blob/main/docs/query-language.md",
                None::<&str>,
            );
        }
        "help:repo" => {
            let _ = app
                .opener()
                .open_url("https://github.com/Seerendo/grafeno", None::<&str>);
        }
        recent if recent.starts_with("recent:") => {
            let index: usize = recent["recent:".len()..].parse().unwrap_or(usize::MAX);
            let path = desktop
                .recent
                .lock()
                .expect("not poisoned")
                .get(index)
                .cloned();
            if let Some(path) = path {
                let _ = app.emit("open-path", path.display().to_string());
            }
        }
        other => {
            let _ = app.emit("menu", other.to_owned());
        }
    }
}

/// A database file given on the command line (or by a second launch).
fn path_argument(args: &[String]) -> Option<String> {
    args.iter()
        .skip(1)
        .find(|arg| !arg.starts_with('-') && (arg.ends_with(".redb") || arg.ends_with(".grafeno")))
        .cloned()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // One app: a second launch shows this window, and opens its file.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            if let Some(path) = path_argument(&args) {
                let _ = app.emit("open-path", path);
            }
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Desktop::default())
        .setup(|app| {
            let handle = app.handle().clone();
            let desktop = handle.state::<Desktop>();
            *desktop.recent.lock().expect("not poisoned") = load_recent(&handle);
            rebuild_menu(&handle)?;
            handle.on_menu_event(|app, event| on_menu(app, event.id().as_ref()));
            let args: Vec<String> = std::env::args().collect();
            if let Some(path) = path_argument(&args) {
                // The Studio asks for the session when it starts.
                if let Err(error) = open_path(&handle, &desktop, PathBuf::from(path)) {
                    eprintln!("{error}");
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            session,
            open_database,
            close_database,
            recent,
            forget_recent,
            pick_database,
            pick_save,
            pick_open,
            write_text,
            read_text,
            open_external,
            backup_database,
            restore_database,
            request,
        ])
        .run(tauri::generate_context!())
        .expect("the Grafeno window could not start");
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn call(router: &Router, method: &str, path: &str, body: Option<&str>) -> Reply {
        let headers = BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]);
        local_request(
            router.clone(),
            method,
            path,
            &headers,
            body.map(str::to_owned),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_bridge_reads_and_writes_in_process() {
        let router = router_for(Arc::new(Grafeno::in_memory().unwrap()), None);
        let health = call(&router, "GET", "/health", None).await;
        assert_eq!(health.status, 200);
        let created = call(&router, "POST", "/v1/namespaces", Some(r#"{"name":"lab"}"#)).await;
        assert_eq!(created.status, 201, "{}", created.body);
        let wrote = call(
            &router,
            "POST",
            "/v1/lab/query",
            Some(r#"{"query":"CREATE (:Person {name: 'Ana'})-[:knows]->(:Person {name: 'Bob'})"}"#),
        )
        .await;
        assert_eq!(wrote.status, 200, "{}", wrote.body);
        let read = call(
            &router,
            "POST",
            "/v1/lab/query",
            Some(r#"{"query":"MATCH (p:Person) RETURN count(p)"}"#),
        )
        .await;
        assert!(read.body.contains("[[2]]"), "{}", read.body);
        assert!(
            read.headers
                .get("content-type")
                .is_some_and(|t| t.contains("json"))
        );
        let deleted = call(&router, "DELETE", "/v1/namespaces/lab", None).await;
        assert_eq!(deleted.status, 200);
        let missing = call(&router, "GET", "/v1/namespaces/lab", None).await;
        assert_eq!(missing.status, 404);
        // The Studio is not served from the app's router.
        assert_eq!(call(&router, "GET", "/studio/", None).await.status, 404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_bridge_reaches_a_server() {
        let router = router_for(Arc::new(Grafeno::in_memory().unwrap()), None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .new_agent();
        let url = format!("http://{address}");
        let reply = tokio::task::spawn_blocking(move || {
            let headers =
                BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]);
            let created = remote_request(
                &agent,
                "POST",
                &format!("{url}/v1/namespaces"),
                &headers,
                Some(r#"{"name":"far"}"#.into()),
            )
            .unwrap();
            let missing = remote_request(
                &agent,
                "GET",
                &format!("{url}/v1/namespaces/nope"),
                &headers,
                None,
            )
            .unwrap();
            (created, missing)
        })
        .await
        .unwrap();
        assert_eq!(reply.0.status, 201, "{}", reply.0.body);
        assert_eq!(
            reply.1.status, 404,
            "errors come back as answers, not failures"
        );
    }

    #[test]
    fn a_database_path_on_the_command_line() {
        let args = |list: &[&str]| list.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            path_argument(&args(&["app", "--flag", "C:/a/b.redb"])).as_deref(),
            Some("C:/a/b.redb")
        );
        assert_eq!(
            path_argument(&args(&["app", "notes.grafeno"])).as_deref(),
            Some("notes.grafeno")
        );
        assert_eq!(path_argument(&args(&["app", "readme.txt"])), None);
    }
}
