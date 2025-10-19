use super::{PackageManager, Preset, ProjectType};
use std::path::PathBuf;

pub struct Vite;

impl Preset for Vite {
    fn slug(&self) -> String {
        "vite".to_string()
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Static
    }

    fn label(&self) -> String {
        "Vite".to_string()
    }

    fn icon_url(&self) -> String {
        "https://example.com/vite-icon.png".to_string()
    }

    fn dockerfile(
        &self,
        _root_local_path: &PathBuf,
        local_path: &PathBuf,
        install_command: Option<&str>,
        build_command: Option<&str>,
        output_dir: Option<&str>,
        build_vars: Option<&Vec<String>>,
        project_slug: &str,
    ) -> String {
        let package_manager = PackageManager::detect(local_path);
        let install_cmd = install_command.unwrap_or(package_manager.install_command());
        let build_cmd = build_command.unwrap_or(package_manager.build_command());
        let output = output_dir.unwrap_or("dist");

        let mut dockerfile = format!(
            r#"FROM {} as builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/app/node_modules,id=node_modules_{} {}
"#,
            package_manager.base_image(),
            project_slug,
            install_cmd
        );

        // Add build variables if present
        if let Some(vars) = build_vars {
            for var in vars {
                dockerfile.push_str(&format!("ARG {}\n", var));
            }
        }

        dockerfile.push_str(&format!(
            r#"
RUN {}

FROM nginx:alpine
COPY --from=builder /app/{} /usr/share/nginx/html
"#,
            build_cmd, output
        ));

        dockerfile
    }

    fn dockerfile_with_build_dir(&self, local_path: &PathBuf) -> String {
        let pkg_manager = PackageManager::detect(local_path);

        format!(
            r#"
FROM {}

WORKDIR /app

# Copy only the dist directory
COPY dist ./dist

# Install serve
RUN {}

# Expose the port the app runs on
EXPOSE 3000

CMD ["serve", "-s", "dist", "-l", "3000"]
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
        vec!["dist".to_string()]
    }
}

impl std::fmt::Display for Vite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}
