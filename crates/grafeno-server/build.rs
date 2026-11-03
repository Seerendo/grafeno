//! Embeds Grafeno Studio (the built web interface in `studio/dist`) when it
//! exists. Without it the binary still builds, and `/studio` explains how to
//! build the interface.

use std::path::Path;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(studio)");
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../studio/dist");
    println!("cargo::rerun-if-changed={}", dist.display());
    if dist.join("index.html").exists() {
        println!("cargo::rustc-cfg=studio");
    }
}
