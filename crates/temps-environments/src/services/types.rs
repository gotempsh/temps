use serde::Serialize;
use temps_core::UtcDateTime;

// Environment variable types
#[derive(Debug, Clone, Serialize)]
pub struct EnvVarEnvironment {
    pub id: i32,
    pub name: String,
    pub main_url: String,
    pub current_deployment_id: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct EnvVarWithEnvironments {
    pub id: i32,
    pub project_id: i32,
    pub key: String,
    pub value: String,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub environments: Vec<EnvVarEnvironment>,
    pub include_in_preview: bool,
}
