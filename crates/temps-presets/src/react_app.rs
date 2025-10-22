use std::path::{Path, PathBuf};

use super::{PackageManager, Preset, ProjectType};

pub struct CreateReactApp;

impl Preset for CreateReactApp {
    fn slug(&self) -> String {
        "react-app".to_string()
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Static
    }

    fn label(&self) -> String {
        "React App".to_string()
    }

    fn icon_url(&self) -> String {
        "https://example.com/react-icon.png".to_string()
    }

    fn dockerfile(
        &self,
        _root_local_path: &PathBuf,
        local_path: &PathBuf,
        install_command: Option<&str>,
        build_command: Option<&str>,
        _output_dir: Option<&str>,
        build_vars: Option<&Vec<String>>,
        project_slug: &str,
    ) -> String {
        let pkg_manager = self.package_manager(local_path);

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

# Copy package files
{}

# Install dependencies
RUN --mount=type=cache,target=/app/node_modules,id=node_modules_{} {}
"#,
            pkg_manager.base_image(),
            lockfile,
            project_slug,
            install_command.unwrap_or(&self.install_command(local_path))
        );

        // Add build variables if present
        if let Some(vars) = build_vars {
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
            build_command.unwrap_or(&self.build_command(local_path)),
            pkg_manager.base_image(),
            match pkg_manager {
                PackageManager::Bun => "bun install -g serve",
                PackageManager::Yarn => "yarn global add serve",
                PackageManager::Npm => "npm install -g serve",
                PackageManager::Pnpm => "npm install -g serve",

            }
        ));

        dockerfile
    }

    fn dockerfile_with_build_dir(&self, local_path: &PathBuf) -> String {
        let pkg_manager = self.package_manager(local_path);

        format!(
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
        )
    }

    fn install_command(&self, local_path: &Path) -> String {
        match self.package_manager(local_path) {
            PackageManager::Bun => "bun install --frozen-lockfile".to_string(),
            PackageManager::Yarn => "yarn install".to_string(),
            PackageManager::Npm => "npm install".to_string(),
            PackageManager::Pnpm => "pnpm install".to_string(),
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
        vec!["build".to_string()]
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
