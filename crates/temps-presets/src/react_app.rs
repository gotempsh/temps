// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::path::Path;

use super::{DockerfileWithArgs, PackageManager, Preset, ProjectType};
use async_trait::async_trait;

pub struct CreateReactApp;

#[async_trait]
impl Preset for CreateReactApp {
    fn slug(&self) -> String {
        "react-app".to_string()
    }

    fn stored_preset(&self) -> Option<temps_entities::preset::Preset> {
        Some(temps_entities::preset::Preset::React)
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Static
    }

    fn label(&self) -> String {
        "React App".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/react.svg".to_string()
    }

    async fn dockerfile(&self, config: super::DockerfileConfig<'_>) -> DockerfileWithArgs {
        let pkg_manager = self.package_manager(config.local_path);

        let pnpm_setup = if matches!(pkg_manager, PackageManager::Pnpm) {
            "RUN corepack enable\n\n"
        } else {
            ""
        };

        let lockfile = match pkg_manager {
            PackageManager::Bun => "COPY package.json bun.lock* ./",
            PackageManager::Yarn => "COPY package.json yarn.lock ./",
            PackageManager::Npm => "COPY package.json package-lock.json ./",
            PackageManager::Pnpm => "COPY package.json pnpm-lock.yaml ./",
        };

        let mut dockerfile = format!(
            r#"
# Stage 1: Build the React application
FROM {} AS builder

WORKDIR /app

{pnpm_setup}# Copy package files
{}
{}
# Install dependencies
RUN {}
"#,
            pkg_manager.base_image(),
            lockfile,
            pkg_manager.dependency_config_copy(config.local_path),
            config.install_command.unwrap_or(&self.install_command(config.local_path))
        );

        // Add build variables if present
        if let Some(vars) = config.build_vars {
            for var in vars {
                dockerfile.push_str(&format!("ARG {}\n", var));
            }
        }

        dockerfile.push_str(&format!(
            r#"
# Copy the rest of the application code
COPY . .

# Build the application
RUN {}

# Stage 2: Create the production image
FROM {} AS runner

WORKDIR /app

# Install serve
RUN {}

# Copy necessary files from the builder stage
COPY --from=builder /app/build ./build

# Expose the port the app runs on
EXPOSE 3000

CMD ["serve", "-s", "build", "-l", "3000"]
"#,
            config.build_command.unwrap_or(&self.build_command(config.local_path)),
            pkg_manager.base_image(),
            match pkg_manager {
                PackageManager::Bun => "bun install -g serve",
                PackageManager::Yarn => "yarn global add serve",
                PackageManager::Npm => "npm install -g serve",
                PackageManager::Pnpm => "npm install -g serve",

            }
        ));

        DockerfileWithArgs::new(dockerfile)
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        let pkg_manager = self.package_manager(local_path);

        let content = format!(
            r#"
FROM {}

WORKDIR /app

# Copy only the build directory
COPY build ./build

# Install serve
RUN {}

# Expose the port the app runs on
EXPOSE 3000

CMD ["serve", "-s", "build", "-l", "3000"]
"#,
            pkg_manager.base_image(),
            match pkg_manager {
                PackageManager::Bun => "bun install -g serve",
                PackageManager::Yarn => "yarn global add serve",
                PackageManager::Npm => "npm install -g serve",
                PackageManager::Pnpm => "npm install -g serve",
            }
        );
        DockerfileWithArgs::new(content)
    }

    fn install_command(&self, local_path: &Path) -> String {
        match self.package_manager(local_path) {
            PackageManager::Bun => "bun install --frozen-lockfile".to_string(),
            PackageManager::Yarn => "yarn install".to_string(),
            PackageManager::Npm => "npm install".to_string(),
            PackageManager::Pnpm => "pnpm install --frozen-lockfile".to_string(),
        }
    }

    fn build_command(&self, local_path: &Path) -> String {
        match self.package_manager(local_path) {
            PackageManager::Bun => "bun run build".to_string(),
            PackageManager::Yarn => "yarn run build".to_string(),
            PackageManager::Npm => "npm run build".to_string(),
            PackageManager::Pnpm => "pnpm run build".to_string(),
        }
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        vec!["/app/build".to_string()]
    }

    fn static_output_dir(&self) -> Option<String> {
        Some("/app/build".to_string())
    }
}

impl CreateReactApp {
    fn package_manager(&self, local_path: &Path) -> PackageManager {
        if local_path.join("package-lock.json").exists() {
            PackageManager::Npm
        } else if local_path.join("bun.lockb").exists() ||  local_path.join("bun.lock").exists() {
            PackageManager::Bun
        } else if local_path.join("yarn.lock").exists() {
            PackageManager::Yarn
        } else if local_path.join("pnpm-lock.yaml").exists() {
            PackageManager::Pnpm
        } else {
            // Default to npm if unknown
            PackageManager::Npm
        }
    }
}
impl std::fmt::Display for CreateReactApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_pnpm_lockfile_selects_pnpm_install_and_build() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"packageManager":"pnpm@11.9.0"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .unwrap();

        assert!(matches!(
            CreateReactApp.package_manager(dir.path()),
            PackageManager::Pnpm
        ));
        assert_eq!(
            CreateReactApp.install_command(dir.path()),
            "pnpm install --frozen-lockfile"
        );
        assert_eq!(CreateReactApp.build_command(dir.path()), "pnpm run build");
    }

    #[test]
    fn test_package_manager_keeps_existing_lockfile_precedence() {
        for (lockfile, expected) in [
            ("package-lock.json", "npm install"),
            ("bun.lock", "bun install --frozen-lockfile"),
            ("yarn.lock", "yarn install"),
        ] {
            let dir = TempDir::new().unwrap();
            std::fs::write(dir.path().join(lockfile), "").unwrap();
            std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
            assert_eq!(CreateReactApp.install_command(dir.path()), expected);
        }
        let dir = TempDir::new().unwrap();
        assert_eq!(CreateReactApp.install_command(dir.path()), "npm install");
    }
}
