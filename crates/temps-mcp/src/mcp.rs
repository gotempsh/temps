use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info};

use rmcp::{
    handler::server::{
        router::{prompt::PromptRouter, tool::ToolRouter},
        wrapper::Parameters,
    },
    model::*,
    prompt, prompt_handler, prompt_router, schemars,
    service::RequestContext,
    tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler,
};

// Import project service from temps-projects crate
use temps_projects::services::project::ProjectService;
use temps_projects::services::types::ProjectError;

#[derive(Clone)]
pub struct McpService {
    clients: Arc<RwLock<Vec<McpClient>>>,
    prompts: Arc<RwLock<Vec<McpPrompt>>>,
    resources: Arc<RwLock<Vec<McpResource>>>,
    project_service: Option<Arc<ProjectService>>,
    tool_router: ToolRouter<McpService>,
    prompt_router: PromptRouter<McpService>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct McpClient {
    pub id: String,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct McpPrompt {
    pub id: String,
    pub name: String,
    pub description: String,
    pub arguments: Vec<McpArgument>,
    pub template: String,
    pub client_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct McpArgument {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub argument_type: String,
}

// Define request/response structures for tools and prompts
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProjectInfoArgs {
    /// The slug of the project to get information about
    pub project_slug: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct McpResource {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: Option<String>,
    pub client_id: Option<String>,
}

#[tool_router]
impl McpService {
    pub fn new() -> Self {
        let prompts = Vec::new();
        let resources = Vec::new(); // Will be populated dynamically

        Self {
            clients: Arc::new(RwLock::new(Vec::new())),
            prompts: Arc::new(RwLock::new(prompts)),
            resources: Arc::new(RwLock::new(resources)),
            project_service: None,
            tool_router: Self::tool_router(),
            prompt_router: Self::prompt_router(),
        }
    }

    // Tool implementations
    #[tool(description = "List all available projects")]
    async fn list_projects(&self) -> Result<CallToolResult, McpError> {
        if let Some(project_service) = &self.project_service {
            match project_service.get_projects().await {
                Ok(projects) => {
                    let projects_json = serde_json::to_string_pretty(&projects)
                        .unwrap_or_else(|_| "Failed to serialize projects".to_string());

                    Ok(CallToolResult::success(vec![Content::text(format!(
                        "Found {} projects:\n{}",
                        projects.len(),
                        projects_json
                    ))]))
                }
                Err(e) => {
                    error!("Failed to fetch projects: {}", e);
                    Err(McpError::internal_error(
                        "Failed to fetch projects",
                        Some(json!({"error": e.to_string()})),
                    ))
                }
            }
        } else {
            Err(McpError::internal_error(
                "Project service not available",
                None,
            ))
        }
    }

    #[tool(description = "Get information about a specific project by slug")]
    async fn get_project(
        &self,
        Parameters(args): Parameters<ProjectInfoArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(project_service) = &self.project_service {
            match project_service
                .get_project_by_slug(&args.project_slug)
                .await
            {
                Ok(project) => {
                    let mut result = "Project Information:\n".to_string();
                    result.push_str(&format!("ID: {}\n", project.id));
                    result.push_str(&format!("Name: {}\n", project.name));
                    result.push_str(&format!("Slug: {}\n", project.slug));
                    result.push_str(&format!(
                        "Repository: {}/{}\n",
                        project.repo_owner.unwrap_or("unknown".to_string()),
                        project.repo_name.unwrap_or("unknown".to_string())
                    ));
                    result.push_str(&format!("Directory: {}\n", project.directory));
                    result.push_str(&format!("Branch: {}\n", project.main_branch));
                    result.push_str(&format!("Auto Deploy: {}\n", project.automatic_deploy));
                    result.push_str(&format!("Created: {}\n", project.created_at));
                    result.push_str(&format!("Updated: {}\n", project.updated_at));

                    result.push_str(
                        "\nNote: Deployment information is not available in this service.\n",
                    );

                    Ok(CallToolResult::success(vec![Content::text(result)]))
                }
                Err(ProjectError::NotFound(_)) => {
                    Err(McpError::invalid_params("Project not found", None))
                }
                Err(e) => {
                    error!("Failed to fetch project {}: {}", args.project_slug, e);
                    Err(McpError::internal_error(
                        "Failed to fetch project",
                        Some(json!({"error": e.to_string()})),
                    ))
                }
            }
        } else {
            Err(McpError::internal_error(
                "Project service not available",
                None,
            ))
        }
    }

    pub fn with_project_service(mut self, project_service: Arc<ProjectService>) -> Self {
        self.project_service = Some(project_service);
        self
    }

    pub async fn initialize_mcp_server(&self) -> anyhow::Result<()> {
        info!("Initializing MCP server with built-in prompts and resources");

        // Populate resources dynamically
        self.populate_resources().await?;

        info!(
            "MCP server initialized with {} prompts and {} resources",
            self.prompts.read().await.len(),
            self.resources.read().await.len()
        );
        Ok(())
    }

    async fn populate_resources(&self) -> anyhow::Result<()> {
        let mut resources = self.resources.write().await;
        resources.clear();

        // Add general project listing resource
        resources.push(McpResource {
            id: "projects-resource".to_string(),
            uri: "project://".to_string(),
            name: "Projects".to_string(),
            description: "Access to all project data and configurations".to_string(),
            mime_type: Some("application/json".to_string()),
            client_id: None,
        });

        // Add individual project resources if project service is available
        if let Some(project_service) = &self.project_service {
            match project_service.get_projects().await {
                Ok(projects) => {
                    for project in projects {
                        resources.push(McpResource {
                            id: format!("project-{}", project.slug),
                            uri: format!("project://{}", project.slug),
                            name: format!("Project: {}", project.name),
                            description: format!(
                                "Access to {} project data and configurations",
                                project.name
                            ),
                            mime_type: Some("application/json".to_string()),
                            client_id: None,
                        });
                    }
                }
                Err(e) => {
                    error!("Failed to populate project resources: {}", e);
                }
            }
        }

        Ok(())
    }

    pub async fn add_client(
        &self,
        id: String,
        name: String,
        command: String,
        args: Vec<String>,
    ) -> anyhow::Result<()> {
        let client = McpClient {
            id,
            name: name.clone(),
            command,
            args,
        };
        let mut clients = self.clients.write().await;
        clients.push(client);
        info!("Added MCP client: {}", name);
        Ok(())
    }

    pub async fn list_clients(&self) -> Vec<McpClient> {
        let clients = self.clients.read().await;
        clients.clone()
    }

    pub async fn remove_client(&self, id: &str) -> anyhow::Result<bool> {
        let mut clients = self.clients.write().await;
        let initial_len = clients.len();
        clients.retain(|client| client.id != id);
        let removed = clients.len() != initial_len;
        if removed {
            info!("Removed MCP client with id: {}", id);
        }
        Ok(removed)
    }

    pub async fn connect_to_client(&self, id: &str) -> anyhow::Result<Value> {
        let clients = self.clients.read().await;
        let client = clients
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| anyhow::anyhow!("Client not found"))?;

        debug!("Connecting to MCP client: {}", client.name);

        // For now, we'll return a mock response
        // In production, this would establish actual MCP client connections
        info!("Mock connection to MCP client: {}", client.name);

        Ok(serde_json::json!({
            "status": "connected",
            "client_id": id,
            "client_name": client.name
        }))
    }

    pub async fn execute_tool(
        &self,
        client_id: &str,
        tool_name: &str,
        arguments: Value,
    ) -> anyhow::Result<Value> {
        debug!("Executing tool {} on client {}", tool_name, client_id);

        Ok(serde_json::json!({
            "result": "Tool execution not yet implemented",
            "tool": tool_name,
            "client": client_id,
            "arguments": arguments
        }))
    }

    // Prompt management methods
    pub async fn list_prompts(&self) -> Vec<McpPrompt> {
        let prompts = self.prompts.read().await;
        prompts.clone()
    }

    pub async fn get_prompt(&self, id: &str) -> anyhow::Result<McpPrompt> {
        let prompts = self.prompts.read().await;
        prompts
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Prompt not found"))
    }

    pub async fn execute_prompt(
        &self,
        id: &str,
        arguments: HashMap<String, String>,
    ) -> anyhow::Result<String> {
        let prompt = self.get_prompt(id).await?;
        let mut result = prompt.template.clone();

        for (key, value) in arguments {
            result = result.replace(&format!("{{{{{}}}}}", key), &value);
        }

        debug!("Executed prompt {}: {}", id, result);
        Ok(result)
    }

    // Resource management methods
    pub async fn list_resources(&self) -> Vec<McpResource> {
        let resources = self.resources.read().await;
        resources.clone()
    }

    pub async fn get_resource(&self, uri: &str) -> anyhow::Result<Value> {
        debug!("Fetching resource: {}", uri);

        if uri.starts_with("project://") {
            return self.get_project_resource(uri).await;
        }

        Err(anyhow::anyhow!("Unsupported resource URI: {}", uri))
    }

    async fn get_project_resource(&self, uri: &str) -> anyhow::Result<Value> {
        let project_service = self
            .project_service
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Project service not available"))?;

        let path = uri.strip_prefix("project://").unwrap_or("");

        if path.is_empty() {
            // Return list of all projects
            match project_service.get_projects().await {
                Ok(projects) => Ok(serde_json::json!({
                    "type": "project_list",
                    "uri": uri,
                    "data": {
                        "projects": projects,
                        "count": projects.len()
                    }
                })),
                Err(e) => Err(anyhow::anyhow!("Failed to fetch projects: {}", e)),
            }
        } else {
            // Try to get project by slug first (preferred)
            match project_service.get_project_by_slug(path).await {
                Ok(project) => Ok(serde_json::json!({
                    "type": "project_detail",
                    "uri": uri,
                    "data": {
                        "project": project
                    }
                })),
                Err(_) => {
                    // If slug lookup fails, try ID as fallback (for backward compatibility)
                    if let Ok(project_id) = path.parse::<i32>() {
                        match project_service.get_project(project_id).await {
                            Ok(project) => Ok(serde_json::json!({
                                "type": "project_detail",
                                "uri": uri,
                                "data": {
                                    "project": project
                                }
                            })),
                            Err(e) => Err(anyhow::anyhow!(
                                "Failed to fetch project '{}' by slug or ID: {}",
                                path,
                                e
                            )),
                        }
                    } else {
                        Err(anyhow::anyhow!("Project '{}' not found by slug", path))
                    }
                }
            }
        }
    }

    pub async fn add_resource(&self, resource: McpResource) -> anyhow::Result<()> {
        let mut resources = self.resources.write().await;
        resources.push(resource);
        Ok(())
    }

    pub async fn remove_resource(&self, id: &str) -> anyhow::Result<bool> {
        let mut resources = self.resources.write().await;
        let initial_len = resources.len();
        resources.retain(|r| r.id != id);
        Ok(resources.len() != initial_len)
    }
}

// Prompt router implementation
#[prompt_router]
impl McpService {
    /// Get detailed information about a specific project
    #[prompt(name = "project_info")]
    async fn project_info_prompt(
        &self,
        Parameters(args): Parameters<ProjectInfoArgs>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let messages = vec![
            PromptMessage::new_text(
                PromptMessageRole::Assistant,
                "You are a helpful assistant that provides information about projects.",
            ),
            PromptMessage::new_text(
                PromptMessageRole::User,
                format!(
                    "Please provide detailed information about project with slug: {}\n\
                    Include:\n\
                    - Project configuration\n\
                    - Current deployment status\n\
                    - Recent pipeline runs\n\
                    - Associated domains\n\
                    - Environment variables",
                    args.project_slug
                ),
            ),
        ];

        Ok(GetPromptResult {
            description: Some(format!(
                "Get information about project {}",
                args.project_slug
            )),
            messages,
        })
    }
}

// ServerHandler implementation for MCP protocol
#[tool_handler]
#[prompt_handler]
impl ServerHandler for McpService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_prompts()
                .enable_resources()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "indie-hacker-engine-mcp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                title: Some("Indie Hacker Engine MCP Server".to_string()),
                website_url: None,
                icons: None,
            },
            instructions: Some(
                "This MCP server provides access to project data from the Indie Hacker Engine platform. \
                Available tools: list_projects, get_project (uses project slug). \
                Available prompts: project_info (uses project slug for detailed information). \
                Available resources: project:// (access project data by slug/ID). \
                Slugs are preferred over IDs for better usability."
                .to_string()
            ),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let resources = self.resources.read().await;
        let raw_resources = resources
            .iter()
            .map(|resource| RawResource::new(&resource.uri, resource.name.clone()).no_annotation())
            .collect();

        Ok(ListResourcesResult {
            resources: raw_resources,
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        ReadResourceRequestParam { uri }: ReadResourceRequestParam,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        match self.get_resource(&uri).await {
            Ok(data) => Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(
                    serde_json::to_string_pretty(&data)
                        .unwrap_or_else(|_| "Error serializing data".to_string()),
                    uri,
                )],
            }),
            Err(_) => Err(McpError::resource_not_found(
                "Resource not found",
                Some(json!({ "uri": uri })),
            )),
        }
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult {
            next_cursor: None,
            resource_templates: Vec::new(),
        })
    }

    async fn initialize(
        &self,
        _request: InitializeRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        info!("MCP server initialized");
        Ok(self.get_info())
    }
}

impl Default for McpService {
    fn default() -> Self {
        Self::new()
    }
}
