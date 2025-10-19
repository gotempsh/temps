use super::{Preset, ProjectType};
use std::fmt;
use std::path::PathBuf;

pub struct DockerCustomPreset;

impl Preset for DockerCustomPreset {
    fn slug(&self) -> String {
        "custom".to_string()
    }

    fn project_type(&self) -> ProjectType {
        ProjectType::Server
    }

    fn label(&self) -> String {
        "Docker Custom".to_string()
    }

    fn icon_url(&self) -> String {
        "/images/presets/docker.svg".to_string()
    }

    fn dockerfile(
        &self,
        _root_local_path: &PathBuf,
        _local_path: &PathBuf,
        install_command: Option<&str>,
        build_command: Option<&str>,
        output_dir: Option<&str>,
        build_vars: Option<&Vec<String>>,
        project_slug: &str,
    ) -> String {
        let base_image = "alpine:latest";
        
        // Create the initial part of the Dockerfile
        let mut dockerfile = format!(r#"FROM {}

# Set up working directory
WORKDIR /app

# Copy project files
COPY . .

"#, base_image);

        // Add build variables if present
        if let Some(vars) = build_vars {
            let build_vars_section = vars
                .iter()
                .map(|var| format!("ARG {}", var))
                .collect::<Vec<_>>()
                .join("\n");
            
            if !build_vars_section.is_empty() {
                dockerfile = format!("{}# Build arguments\n{}\n\n", dockerfile, build_vars_section);
            }
        }

        // Add the project slug as an ARG
        dockerfile = format!("{}ARG PROJECT_SLUG={}\n", dockerfile, project_slug);

        // Determine if we need to install any dependencies based on what files are present
        dockerfile = format!("{}
# Install needed dependencies
RUN apk add --no-cache nodejs npm git curl
", dockerfile);

        // Add the install command if provided
        if let Some(cmd) = install_command {
            dockerfile = format!("{}
# Install dependencies
RUN {}\n", dockerfile, cmd);
        }

        // Add the build command if provided
        if let Some(cmd) = build_command {
            dockerfile = format!("{}
# Build the application
RUN {}\n", dockerfile, cmd);
        }

        // Output directory handling
        let app_dir = if let Some(dir) = output_dir {
            format!("/app/{}", dir)
        } else {
            "/app".to_string()
        };

        // Finalize the dockerfile with web server setup
        dockerfile = format!("{}
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

EXPOSE 80

# Start the web server
CMD [\"nginx\", \"-g\", \"daemon off;\"]", 
            dockerfile, 
            app_dir
        );

        dockerfile
    }

    fn dockerfile_with_build_dir(&self, local_path: &PathBuf) -> String {
        // This method should return a Dockerfile that can be used with a build context directory
        // In this case, we'll use the same Dockerfile as the regular one
        self.dockerfile(
            &PathBuf::new(),
            local_path,
            None,
            None,
            None,
            None,
            "app",
        )
    }

    fn install_command(&self, _local_path: &PathBuf) -> String {
        // This will be overridden by the actual project's install command
        "".to_string()
    }

    fn build_command(&self, _local_path: &PathBuf) -> String {
        // This will be overridden by the actual project's build command
        "".to_string()
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