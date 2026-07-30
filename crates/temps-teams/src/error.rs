use axum::http::StatusCode;
use temps_core::problemdetails::{self, Problem};
use temps_entities::TeamRoleParseError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum TeamError {
    #[error("Team {team_id} not found")]
    NotFound { team_id: i32 },

    #[error("Team member (team {team_id}, user {user_id}) not found")]
    MemberNotFound { team_id: i32, user_id: i32 },

    #[error("Project {project_id} has no access grant for team {team_id}")]
    ProjectAccessNotFound { project_id: i32, team_id: i32 },

    #[error("Team slug '{slug}' is already taken")]
    SlugConflict { slug: String },

    #[error("Team {team_id} already has user {user_id} as a member")]
    DuplicateMember { team_id: i32, user_id: i32 },

    #[error("Project {project_id} already grants access to team {team_id}")]
    DuplicateProjectAccess { project_id: i32, team_id: i32 },

    #[error("Validation error: {message}")]
    Validation { message: String },

    #[error("Invalid role: {0}")]
    InvalidRole(#[from] TeamRoleParseError),

    #[error("Custom role {role_id} not found")]
    CustomRoleNotFound { role_id: i32 },

    #[error("Custom role slug '{slug}' is already taken")]
    CustomRoleSlugConflict { slug: String },

    #[error("Invalid permission: '{permission}'")]
    InvalidPermission { permission: String },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

impl From<TeamError> for Problem {
    fn from(error: TeamError) -> Self {
        match error {
            TeamError::NotFound { .. }
            | TeamError::MemberNotFound { .. }
            | TeamError::ProjectAccessNotFound { .. }
            | TeamError::CustomRoleNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Resource Not Found")
                .with_detail(error.to_string()),

            TeamError::Validation { .. }
            | TeamError::InvalidRole(_)
            | TeamError::InvalidPermission { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),

            TeamError::SlugConflict { .. }
            | TeamError::DuplicateMember { .. }
            | TeamError::DuplicateProjectAccess { .. }
            | TeamError::CustomRoleSlugConflict { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Resource Conflict")
                .with_detail(error.to_string()),

            TeamError::Database(_) => problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                .with_title("Internal Server Error")
                .with_detail(error.to_string()),
        }
    }
}
