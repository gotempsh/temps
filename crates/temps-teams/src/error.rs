use axum::http::StatusCode;
use temps_core::problemdetails::{self, Problem};
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

    /// Adding the first grant to a project, or revoking its last one,
    /// changes whether the project is gated at all — an administrative act.
    #[error(
        "Restricting a project to teams, or removing its last grant and re-opening it, \
         requires an instance administrator"
    )]
    GatingRequiresAdmin,

    #[error(
        "Changing who can reach project {project_id} requires the {required} permission on it"
    )]
    ProjectPermissionDenied { project_id: i32, required: String },

    #[error("Cannot apply the '{role}' role: it carries '{permission}', which you do not hold on this project")]
    RoleCeilingExceeded { role: String, permission: String },

    /// A `role` column holds a string this build cannot interpret.
    ///
    /// Always a data-integrity problem, never a bad request: the API takes
    /// roles as a typed enum, so an invalid value from a caller is rejected
    /// by deserialization long before it reaches a service. Reporting it as
    /// 400 would send an operator looking for a fault in their own request.
    #[error(
        "Stored role '{role}' on {entity} {id} is not one of owner/admin/deployer/viewer — \
         the teams tables hold a value this build cannot interpret"
    )]
    CorruptStoredRole {
        entity: &'static str,
        id: i32,
        role: String,
    },

    /// The caller's permissions on the project could not be resolved, so
    /// the mutation is refused rather than authorized on a guess.
    #[error(
        "Could not resolve user {user_id}'s permissions on project {project_id} while \
         authorizing an access change: {reason}"
    )]
    PermissionResolution {
        user_id: i32,
        project_id: i32,
        reason: String,
    },

    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
}

impl From<TeamError> for Problem {
    fn from(error: TeamError) -> Self {
        match error {
            TeamError::NotFound { .. }
            | TeamError::MemberNotFound { .. }
            | TeamError::ProjectAccessNotFound { .. } => problemdetails::new(StatusCode::NOT_FOUND)
                .with_title("Resource Not Found")
                .with_detail(error.to_string()),

            TeamError::Validation { .. } => problemdetails::new(StatusCode::BAD_REQUEST)
                .with_title("Validation Error")
                .with_detail(error.to_string()),

            TeamError::GatingRequiresAdmin
            | TeamError::ProjectPermissionDenied { .. }
            | TeamError::RoleCeilingExceeded { .. } => problemdetails::new(StatusCode::FORBIDDEN)
                .with_title("Project Access Change Denied")
                .with_detail(error.to_string()),

            TeamError::SlugConflict { .. }
            | TeamError::DuplicateMember { .. }
            | TeamError::DuplicateProjectAccess { .. } => problemdetails::new(StatusCode::CONFLICT)
                .with_title("Resource Conflict")
                .with_detail(error.to_string()),

            // `CorruptStoredRole` and `PermissionResolution` are server-side
            // faults, not caller mistakes — a 4xx here would have an operator
            // debugging a request that was perfectly well-formed.
            TeamError::Database(_)
            | TeamError::CorruptStoredRole { .. }
            | TeamError::PermissionResolution { .. } => {
                problemdetails::new(StatusCode::INTERNAL_SERVER_ERROR)
                    .with_title("Internal Server Error")
                    .with_detail(error.to_string())
            }
        }
    }
}
