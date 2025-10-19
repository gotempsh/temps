use std::path::PathBuf;

use super::{PackageManager, Preset, ProjectType};

pub struct Docusaurus;

impl Preset for Docusaurus {
    fn slug(&self) -> String {
        "docusaurus".to_string()
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Static
    }

    fn label(&self) -> String {
        "Docusaurus".to_string()
    }

    fn icon_url(&self) -> String {
        "https://example.com/docusaurus-icon.png".to_string()
    }

    fn dockerfile(
        &self,
        _root_local_path: &PathBuf,
        local_path: &PathBuf,
        install_command: Option<&str>,
        build_command: Option<&str>,
        _output_dir: Option<&str>,
        build_vars: Option<&Vec<String>>,
        _project_slug: &str,
    ) -> String {
        let pkg_manager = PackageManager::detect(local_path);

        let lockfile = match pkg_manager {
            PackageManager::Bun => "COPY package.json bun.lock* ./",
            PackageManager::Yarn => "COPY package.json yarn.lock ./",
            PackageManager::Npm => "COPY package.json package-lock.json ./",
            PackageManager::Pnpm => "COPY package.json pnpm-lock.yaml ./",
        };

        let mut dockerfile = format!(
            r#"
# Stage 1: Build the Docusaurus application
FROM {} AS builder

WORKDIR /app

# Copy package files
{}

# Install dependencies
RUN {}
"#,
            pkg_manager.base_image(),
            lockfile,
            install_command.unwrap_or(pkg_manager.install_command())
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
            build_command.unwrap_or(pkg_manager.build_command()),
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

    fn dockerfile_with_build_dir(&self, _local_path: &PathBuf) -> String {
        format!(
            r#"
# Use a lightweight base image
FROM oven/bun:1.2-alpine

WORKDIR /app

# Copy only the dist directory from the build context
COPY build ./build

# Install serve globally
RUN bun install -g serve

# Expose the port the app runs on
EXPOSE 3000

# Use serve to host the static files
CMD ["serve", "-s", "build", "-l", "3000"]
      
"#,
        )
    }

    fn install_command(&self, local_path: &PathBuf) -> String {
        PackageManager::detect(local_path)
            .install_command()
            .to_string()
    }

    fn build_command(&self, local_path: &PathBuf) -> String {
        PackageManager::detect(local_path)
            .build_command()
            .to_string()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        vec!["build".to_string()]
    }
}

impl std::fmt::Display for Docusaurus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}
