//! On Windows, gives the executable its icon and description.

fn main() {
    #[cfg(windows)]
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

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
