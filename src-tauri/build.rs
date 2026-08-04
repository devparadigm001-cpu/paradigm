fn main() {
    tauri_build::build();

    // Integration-test binaries link the Tauri runtime and therefore import
    // Common Controls v6 symbols, but cargo gives test targets no application
    // manifest -- so they fail to load with STATUS_ENTRYPOINT_NOT_FOUND before
    // running anything. tauri_build handles this for the app binary only.
    #[cfg(windows)]
    {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("tests")
            .join("common-controls-v6.manifest");

        println!("cargo:rerun-if-changed=tests/common-controls-v6.manifest");
        println!("cargo:rustc-link-arg-tests=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-tests=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
