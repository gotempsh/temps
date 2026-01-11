use super::{DockerfileWithArgs, PackageManager, Preset, ProjectType};
use async_trait::async_trait;
use std::fmt;
use std::path::Path;

pub struct DockerfilePreset;

#[async_trait]
impl Preset for DockerfilePreset {
    fn slug(&self) -> String {
        "dockerfile".to_string()
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        "Dockerfile".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/docker.svg".to_string()
    }

    async fn dockerfile(&self, config: super::DockerfileConfig<'_>) -> DockerfileWithArgs {
        // Read the existing Dockerfile content
        let dockerfile_path = config.local_path.join("Dockerfile");
        let mut dockerfile = std::fs::read_to_string(&dockerfile_path)
            .unwrap_or_else(|_| "# No Dockerfile found".to_string());

        // Add build variables if present
        if let Some(vars) = config.build_vars {
            let build_vars_section = vars
                .iter()
                .map(|var| format!("ARG {}", var))
                .collect::<Vec<_>>()
                .join("\n");

            // Insert ARG statements after the first FROM
            if let Some(from_pos) = dockerfile.find("FROM") {
                if let Some(newline_pos) = dockerfile[from_pos..].find('\n') {
                    let insert_pos = from_pos + newline_pos + 1;
                    dockerfile.insert_str(insert_pos, &format!("\n{}\n", build_vars_section));
                }
            }
        }

        DockerfileWithArgs::new(dockerfile)
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        // For projects with their own Dockerfile, we'll use it directly
        let dockerfile_path = local_path.join("Dockerfile");
        let content = std::fs::read_to_string(&dockerfile_path)
            .unwrap_or_else(|_| "# No Dockerfile found".to_string());
        DockerfileWithArgs::new(content)
    }

    fn install_command(&self, local_path: &Path) -> String {
        // Try to detect the package manager and return appropriate command
        PackageManager::detect(local_path)
            .install_command()
            .to_string()
    }

    fn build_command(&self, local_path: &Path) -> String {
        // Try to detect the package manager and return appropriate command
        PackageManager::detect(local_path)
            .build_command()
            .to_string()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        // Upload everything since we don't know the specific structure
        vec![".".to_string()]
    }
}

impl fmt::Display for DockerfilePreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}
