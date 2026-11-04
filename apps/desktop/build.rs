fn main() {
    // The window shows the Studio's build: make it a dependency of the app.
    println!("cargo::rerun-if-changed=../../studio/dist/index.html");
    tauri_build::build();
}
