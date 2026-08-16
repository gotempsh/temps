use super::{
    DockerfileConfig, DockerfileWithArgs, Preset, PresetResolutionError, ProjectType, StoredPreset,
};
use async_trait::async_trait;
use std::fmt;
use std::path::Path;
use temps_entities::preset::{
    DockerfileConfig as StoredDockerfileConfig, DockerfileVariant, Preset as StoredPresetType,
    PresetConfig as StoredPresetConfig,
};

pub struct DockerCustomPreset;

#[async_trait]
impl Preset for DockerCustomPreset {
    fn slug(&self) -> String {
        "custom".to_string()
    }

    fn stored_preset(&self) -> Option<StoredPresetType> {
        Some(StoredPresetType::Dockerfile)
    }

    fn resolve_storage(
        &self,
        config: Option<StoredPresetConfig>,
    ) -> Result<StoredPreset, PresetResolutionError> {
        let mut config = match config {
            Some(StoredPresetConfig::Dockerfile(config)) => config,
            Some(other) => {
                return Err(PresetResolutionError::ConfigMismatch {
                    config_preset: other.preset_type(),
                    slug: self.slug(),
                });
            }
            None => StoredDockerfileConfig::default(),
        };
        config.variant = DockerfileVariant::Custom;

        Ok(StoredPreset {
            preset: StoredPresetType::Dockerfile,
            config: Some(StoredPresetConfig::Dockerfile(config)),
        })
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        "Docker Custom".to_string()
    }

    fn icon_url(&self) -> String {
        "/presets/docker.svg".to_string()
    }

    async fn dockerfile(&self, config: DockerfileConfig<'_>) -> DockerfileWithArgs {
        let mut dockerfile = r#"FROM alpine:latest

# Set up working directory
WORKDIR /app

# Copy project files
COPY . .

"#
        .to_string();

        if let Some(vars) = config.build_vars {
            let build_vars_section = vars
                .iter()
                .map(|var| format!("ARG {}", var))
                .collect::<Vec<_>>()
                .join("\n");

            if !build_vars_section.is_empty() {
                dockerfile = format!("{}# Build arguments\n{}\n\n", dockerfile, build_vars_section);
            }
        }

        dockerfile = format!("{}ARG PROJECT_SLUG={}\n", dockerfile, config.project_slug);
        dockerfile = format!(
            "{}
# Install needed dependencies
RUN apk add --no-cache nodejs npm git curl
",
            dockerfile
        );

        if let Some(cmd) = config.install_command {
            dockerfile = format!(
                "{}
# Install dependencies
RUN {}
",
                dockerfile, cmd
            );
        }

        if let Some(cmd) = config.build_command {
            dockerfile = format!(
                "{}
# Build the application
RUN {}
",
                dockerfile, cmd
            );
        }

        let app_dir = config
            .output_dir
            .map(|dir| format!("/app/{}", dir))
            .unwrap_or_else(|| "/app".to_string());

        dockerfile = format!(
            "{}
# Use a lightweight web server
RUN apk add --no-cache nginx

# Set up nginx configuration
RUN echo 'server {{ \\
    listen 80; \\
    server_name _; \\
    \\
    location / {{ \\
        root {}; \\
        try_files $uri $uri/ /index.html; \\
    }} \\
}}' > /etc/nginx/http.d/default.conf

# Security hardening - remove package manager and run as non-root
RUN rm -rf /sbin/apk /usr/bin/apk /etc/apk /var/cache/apk /lib/apk && \\
    chown -R nginx:nginx /var/lib/nginx /var/log/nginx /run/nginx {} && \\
    touch /run/nginx.pid && chown nginx:nginx /run/nginx.pid

USER nginx

EXPOSE 80

CMD [\"nginx\", \"-g\", \"daemon off;\"]",
            dockerfile, app_dir, app_dir
        );

        DockerfileWithArgs::new(dockerfile)
    }

    async fn dockerfile_with_build_dir(&self, local_path: &Path) -> DockerfileWithArgs {
        self.dockerfile(DockerfileConfig {
            root_local_path: Path::new(""),
            local_path,
            install_command: None,
            build_command: None,
            output_dir: None,
            build_vars: None,
            project_slug: "app",
            use_buildkit: false,
        })
        .await
    }

    fn install_command(&self, _local_path: &Path) -> String {
        String::new()
    }

    fn build_command(&self, _local_path: &Path) -> String {
        String::new()
    }

    fn dirs_to_upload(&self) -> Vec<String> {
        vec!["/dist".to_string()]
    }
}

impl fmt::Display for DockerCustomPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Docker Custom Preset")
    }
}
