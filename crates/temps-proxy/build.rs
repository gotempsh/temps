use std::process::Command;

fn main() {
    // Get the workspace root
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    let wasm_crate_dir = workspace_root.join("crates/temps-captcha-wasm");
    let wasm_pkg_dir = wasm_crate_dir.join("pkg");

    // Check if WASM files already exist
    let wasm_js = wasm_pkg_dir.join("temps_captcha_wasm.js");
    let wasm_bg = wasm_pkg_dir.join("temps_captcha_wasm_bg.wasm");

    if !wasm_js.exists() || !wasm_bg.exists() {
        println!("cargo:warning=WASM files not found, building temps-captcha-wasm...");

        // Build WASM module
        let status = Command::new("wasm-pack")
            .args(&["build", "--target", "web", "--release"])
            .current_dir(&wasm_crate_dir)
            .status();

        match status {
            Ok(status) => {
                if !status.success() {
                    panic!(
                        "Failed to build WASM module. Make sure wasm-pack is installed: \
                        'cargo install wasm-pack'"
                    );
                }
                println!(
                    "cargo:warning=Successfully built WASM module at {}",
                    wasm_pkg_dir.display()
                );
            }
            Err(e) => {
                panic!(
                    "Failed to execute wasm-pack. Make sure it's installed: \
                    'cargo install wasm-pack'. Error: {}",
                    e
                );
            }
        }
    } else {
        println!(
            "cargo:warning=WASM files found at {}, skipping build",
            wasm_pkg_dir.display()
        );
    }

    // Tell cargo to re-run this script if the WASM source changes
    println!(
        "cargo:rerun-if-changed={}",
        wasm_crate_dir.join("src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        wasm_crate_dir.join("Cargo.toml").display()
    );
}
