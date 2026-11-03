//! Serves Grafeno Studio, the web interface, from files embedded in the
//! binary at build time (see `build.rs`).

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};

#[cfg(studio)]
static FILES: include_dir::Dir<'static> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../studio/dist");

/// Whether this binary carries the Studio.
pub const INCLUDED: bool = cfg!(studio);

const MISSING: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Grafeno Studio</title>\
<style>body{font:15px/1.6 system-ui,sans-serif;max-width:640px;margin:48px auto;padding:0 16px}\
code{background:#8882;padding:2px 6px;border-radius:4px}</style></head><body>\
<h1>Grafeno Studio is not in this build</h1>\
<p>This <code>grafeno</code> binary was built without the web interface. Build it, then rebuild the binary:</p>\
<p><code>cd studio &amp;&amp; npm ci &amp;&amp; npm run build</code><br><code>cargo build --release -p grafeno-server</code></p>\
<p>The HTTP API works as usual under <code>/v1</code>.</p></body></html>";

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" | "map" => "application/json",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(studio)]
fn file(path: &str) -> Option<&'static [u8]> {
    FILES.get_file(path).map(include_dir::File::contents)
}

#[cfg(not(studio))]
fn file(_path: &str) -> Option<&'static [u8]> {
    None
}

pub async fn redirect() -> Redirect {
    Redirect::permanent("/studio/")
}

pub async fn index() -> Response {
    asset(Path(String::new())).await
}

pub async fn asset(Path(path): Path<String>) -> Response {
    if !INCLUDED {
        return Html(MISSING).into_response();
    }
    let path = if path.is_empty() {
        "index.html".to_owned()
    } else {
        path
    };
    let (path, contents) = match file(&path) {
        Some(contents) => (path, contents),
        // Unknown paths get the app, which routes on the client.
        None if !path.starts_with("assets/") => (
            "index.html".to_owned(),
            file("index.html").unwrap_or_default(),
        ),
        None => return StatusCode::NOT_FOUND.into_response(),
    };
    // Built assets have content hashes in their names: cache them forever.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, content_type(&path)),
            (header::CACHE_CONTROL, cache),
        ],
        contents,
    )
        .into_response()
}
