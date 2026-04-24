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

// Secret types. Deliberately NO `value` field — secret plaintext never leaves
// the service boundary except via SecretService::get_for_deploy.
#[derive(Debug, Clone, Serialize)]
pub struct SecretEnvironmentRef {
    pub id: i32,
    pub name: String,
    pub main_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SecretWithEnvironments {
    pub id: i32,
    pub project_id: i32,
    pub key: String,
    pub include_in_preview: bool,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
    pub environments: Vec<SecretEnvironmentRef>,
}
