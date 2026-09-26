// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

#[path = "src/release_manifest.rs"]
mod release_manifest;

use release_manifest::{parse_manifest, ManifestError, IMAGES};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
enum BuildError {
    #[error("failed to read release image manifest at {path}: {source}")]
    ReadManifest {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid release image manifest at {path}: {source}")]
    InvalidManifest {
        path: PathBuf,
        #[source]
        source: ManifestError,
    },
    #[error("Cargo did not provide OUT_DIR for release image generation: {source}")]
    OutputDirectory {
        #[source]
        source: env::VarError,
    },
    #[error("failed to write generated release image constants at {path}: {source}")]
    WriteGenerated {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn main() -> Result<(), BuildError> {
    println!("cargo:rerun-if-env-changed=TEMPS_RELEASE_IMAGE_MANIFEST");
    println!("cargo:rerun-if-changed=src/release_manifest.rs");
    let manifest = env::var_os("TEMPS_RELEASE_IMAGE_MANIFEST")
        .map(|path| -> Result<_, BuildError> {
            println!("cargo:rerun-if-changed={}", Path::new(&path).display());
            let contents =
                fs::read_to_string(&path).map_err(|source| BuildError::ReadManifest {
                    path: PathBuf::from(&path),
                    source,
                })?;
            parse_manifest(&contents).map_err(|source| BuildError::InvalidManifest {
                path: PathBuf::from(&path),
                source,
            })
        })
        .transpose()?;
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
    let out_dir = env::var("OUT_DIR").map_err(|source| BuildError::OutputDirectory { source })?;
    let output_path = Path::new(&out_dir).join("release_images.rs");
    fs::write(&output_path, generated).map_err(|source| BuildError::WriteGenerated {
        path: output_path,
        source,
    })?;
    Ok(())
}
