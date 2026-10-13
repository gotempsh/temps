// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Variables a service inherits from the project (group) it belongs to.
//! Which variables apply is decided once, in
//! [`temps_entities::project_group_env_vars::inherited_for_project`]; this
//! module only decrypts them for the deploy paths.

use std::collections::HashMap;

use sea_orm::DatabaseConnection;
use temps_core::EncryptionService;
use temps_entities::{environments, project_group_env_vars};

/// The decrypted variables `project_id` inherits in `environment`. Empty when
/// the service is in no project, so every deploy path can call it
/// unconditionally and a container never boots without them.
pub async fn inherited_group_env_vars(
    db: &DatabaseConnection,
    encryption: &EncryptionService,
    project_id: i32,
    environment: &environments::Model,
) -> Result<HashMap<String, String>, String> {
    let Some((group_id, selected)) =
        project_group_env_vars::inherited_for_project(db, project_id, environment)
            .await
            .map_err(|e| format!("loading the variables it inherits: {e}"))?
    else {
        return Ok(HashMap::new());
    };
    selected
        .into_iter()
        .map(|(key, variable)| {
            encryption
                .decrypt_string(&variable.value)
                .map(|value| (key.clone(), value))
                .map_err(|e| format!("decrypting {key} of project group {group_id}: {e}"))
        })
        .collect()
}
