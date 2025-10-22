use super::{Preset, ProjectType};
use std::fmt;
use std::path::Path;

#[derive(Clone)]
pub struct CustomPreset {
    label: String,
    icon_url: String,
    project_type: ProjectType,
    dockerfile: String,
    dockerfile_with_build_dir: String,
    slug: String,
    install_command: String,
    build_command: String,
    output_dir: Option<String>,
    dirs_to_upload: Vec<String>,
}

impl CustomPreset {
    pub fn new(
        label: String,
        icon_url: String,
        project_type: ProjectType,
        dockerfile: String,
        dockerfile_with_build_dir: String,
        slug: String,
        install_command: String,
        build_command: String,
    ) -> Self {
        Self {
            label,
            icon_url,
            project_type,
            dockerfile,
            dockerfile_with_build_dir,
            slug,
            install_command,
            build_command,
            output_dir: None,
            dirs_to_upload: vec![".".to_string()],
        }
    }

    pub fn with_output_dir(mut self, output_dir: String) -> Self {
        self.output_dir = Some(output_dir);
        self
    }

    pub fn with_dirs_to_upload(mut self, dirs: Vec<String>) -> Self {
        self.dirs_to_upload = dirs;
        self
    }
}

impl Preset for CustomPreset {
    fn slug(&self) -> String {
        self.slug.clone()
    }

    fn project_type(&self) -> ProjectType {
        self.project_type
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn icon_url(&self) -> String {
        self.icon_url.clone()
    }

    fn dockerfile(
        &self,
        _root_local_path: &Path,
        _local_path: &Path,
        install_command: Option<&str>,
        build_command: Option<&str>,
        output_dir: Option<&str>,
        build_vars: Option<&Vec<String>>,
        _project_slug: &str,
    ) -> String {
        let mut dockerfile = self.dockerfile.clone();

        // Add build variables if present
        if let Some(vars) = build_vars {
            let build_vars_section = vars
                .iter()
                .map(|var| format!("ARG {}", var))
                .collect::<Vec<_>>()
                .join("\n");
            dockerfile = format!("{}\n{}", build_vars_section, dockerfile);
        }

        // Replace placeholders if they exist
        if let Some(cmd) = install_command {
            dockerfile = dockerfile.replace("{{INSTALL_COMMAND}}", cmd);
        }
        if let Some(cmd) = build_command {
            dockerfile = dockerfile.replace("{{BUILD_COMMAND}}", cmd);
        }
        if let Some(dir) = output_dir {
            dockerfile = dockerfile.replace("{{OUTPUT_DIR}}", dir);
        }

        dockerfile
    }

    fn dockerfile_with_build_dir(&self, _local_path: &Path) -> String {
        self.dockerfile_with_build_dir.clone()
    }

    fn install_command(&self, _local_path: &Path) -> String {
        self.install_command.clone()
    }

    fn build_command(&self, _local_path: &Path) -> String {
        self.build_command.clone()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        self.dirs_to_upload.clone()
    }
}

impl fmt::Display for CustomPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Custom Preset: {}", self.label)
    }
}
