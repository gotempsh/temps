use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct EnvironmentVariableInfo {
    pub name: String,
    pub value: String,
    /// Whether this variable contains sensitive data (passwords, keys, tokens)
    #[schema(example = false)]
    pub sensitive: bool,
}
