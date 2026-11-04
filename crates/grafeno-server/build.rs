//! Embeds Grafeno Studio (the built web interface in `studio/dist`) when it
//! exists. Without it the binary still builds, and `/studio` explains how to
//! build the interface. On Windows it also gives the executable its icon.

use std::path::Path;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(studio)");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dist = root.join("studio/dist");
    println!("cargo::rerun-if-changed={}", dist.display());
    let wanted = std::env::var_os("CARGO_FEATURE_STUDIO").is_some();
    if let Some(index) = wanted
        .then(|| std::fs::read(dist.join("index.html")).ok())
        .flatten()
    {
        println!("cargo::rustc-cfg=studio");
        // `include_dir!` does not tell Cargo about the files it embeds. The
        // index names the content-hashed assets, so a new build changes it;
        // passing its hash to rustc makes Cargo recompile the crate.
        let hash = index.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
        println!("cargo::rustc-env=GRAFENO_STUDIO_BUILD={hash:016x}");
    }

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = root.join("packaging/windows/grafeno.ico");
        println!("cargo::rerun-if-changed={}", icon.display());
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon(&icon.to_string_lossy())
            .set("FileDescription", "Grafeno graph engine")
            .set("ProductName", "Grafeno");
        // A missing resource compiler only costs the icon.
        if let Err(error) = resource.compile() {
            println!("cargo::warning=no icon in the executable: {error}");
        }
    }
}
