// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#[path = "src/release_manifest.rs"]
mod release_manifest;

use release_manifest::{parse_manifest, IMAGES};
use std::{env, fs, path::Path};

fn main() {
    println!("cargo:rerun-if-env-changed=TEMPS_RELEASE_IMAGE_MANIFEST");
    println!("cargo:rerun-if-changed=src/release_manifest.rs");
    let manifest = env::var_os("TEMPS_RELEASE_IMAGE_MANIFEST").map(|path| {
        println!("cargo:rerun-if-changed={}", Path::new(&path).display());
        let contents = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read TEMPS_RELEASE_IMAGE_MANIFEST {}: {error}",
                Path::new(&path).display()
            )
        });
        parse_manifest(&contents).unwrap_or_else(|error| {
            panic!(
                "invalid TEMPS_RELEASE_IMAGE_MANIFEST {}: {error}",
                Path::new(&path).display()
            )
        })
    });
    let mut generated = String::from("// SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n// SPDX-License-Identifier: MIT OR Apache-2.0\n\n");
    let revision = manifest.as_ref().map(|value| value.revision.as_str());
    generated.push_str(&format!(
        "pub const REVISION: Option<&str> = {revision:?};\n"
    ));
    for (key, _) in IMAGES {
        let value = manifest.as_ref().map(|value| value.images.get(key));
        generated.push_str(&format!(
            "pub const {}: Option<&str> = {value:?};\n",
            key.as_str().to_ascii_uppercase()
        ));
    }
    let out_dir = env::var("OUT_DIR").expect("Cargo must set OUT_DIR");
    fs::write(Path::new(&out_dir).join("release_images.rs"), generated)
        .expect("write release images constants");
}
