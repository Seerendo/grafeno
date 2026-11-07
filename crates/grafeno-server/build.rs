//! On Windows, gives the executable its icon and description, and the
//! message table the Event Log needs to show the service's events (one
//! message, id 1, whose text is the event's own string).

fn main() {
    #[cfg(windows)]
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon = root.join("packaging/windows/grafeno.ico");
        println!("cargo::rerun-if-changed={}", icon.display());
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
        let messages = out.join("messages.bin");
        std::fs::write(&messages, message_table()).expect("write the message table");
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon(&icon.to_string_lossy())
            .set("FileDescription", "Grafeno graph engine")
            .set("ProductName", "Grafeno")
            .append_rc_content(&format!(
                "1 MESSAGETABLE \"{}\"",
                messages.display().to_string().replace('\\', "/")
            ));
        // A missing resource compiler only costs the icon and the messages.
        if let Err(error) = resource.compile() {
            println!("cargo::warning=no icon in the executable: {error}");
        }
    }
}

/// A MESSAGE_RESOURCE_DATA with one block holding message 1, "%1": the
/// Event Log shows each event's string as it is.
#[cfg(windows)]
fn message_table() -> Vec<u8> {
    let text: Vec<u16> = "%1\r\n\0".encode_utf16().collect();
    let entry_len = (4 + text.len() * 2).div_ceil(4) * 4;
    let mut out = Vec::new();
    out.extend_from_slice(&1u32.to_le_bytes()); // one block
    out.extend_from_slice(&1u32.to_le_bytes()); // its lowest id
    out.extend_from_slice(&1u32.to_le_bytes()); // its highest id
    out.extend_from_slice(&16u32.to_le_bytes()); // where its entries start
    out.extend_from_slice(&(entry_len as u16).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // UTF-16
    for unit in text {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out.resize(16 + entry_len, 0);
    out
}
