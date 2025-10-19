use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use std::fmt::Display;

/// PipelineStatus enum for pipeline state tracking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DeriveActiveEnum, EnumIter)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum PipelineStatus {
    #[sea_orm(num_value = 0)]
    Running,
    #[sea_orm(num_value = 1)]
    Completed,
    #[sea_orm(num_value = 2)]
    Failed,
    #[sea_orm(num_value = 3)]
    Cancelled,
    #[sea_orm(num_value = 4)]
    Pending,
    #[sea_orm(num_value = 5)]
    Built,
}

impl Display for PipelineStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl PipelineStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PipelineStatus::Running => "running",
            PipelineStatus::Completed => "completed",
            PipelineStatus::Failed => "failed",
            PipelineStatus::Cancelled => "cancelled",
            PipelineStatus::Pending => "pending",
            PipelineStatus::Built => "built",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(PipelineStatus::Running),
            "completed" => Some(PipelineStatus::Completed),
            "failed" => Some(PipelineStatus::Failed),
            "cancelled" => Some(PipelineStatus::Cancelled),
            "pending" => Some(PipelineStatus::Pending),
            "built" => Some(PipelineStatus::Built),
            _ => None,
        }
    }
}

/// ProjectType enum for project classification.
/// NOTE: Use db_type = "Text" for SQLite compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DeriveActiveEnum, EnumIter)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum ProjectType {
    #[sea_orm(string_value = "static")]
    Static,
    #[sea_orm(string_value = "server")]
    Server,
}

impl Display for ProjectType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl ProjectType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectType::Static => "static",
            ProjectType::Server => "server",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "static" => Some(ProjectType::Static),
            "server" => Some(ProjectType::Server),
            _ => None,
        }
    }
}

/// Framework enum for supported frameworks.
/// NOTE: Use db_type = "Text" for SQLite compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DeriveActiveEnum, EnumIter)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum Framework {
    #[sea_orm(string_value = "nextjs")]
    NextJs,
    #[sea_orm(string_value = "vite")]
    Vite,
    #[sea_orm(string_value = "rsbuild")]
    Rsbuild,
    #[sea_orm(string_value = "create-react-app")]
    CreateReactApp,
    #[sea_orm(string_value = "docusaurus")]
    Docusaurus,
    #[sea_orm(string_value = "dockerfile")]
    Dockerfile,
    #[sea_orm(string_value = "unknown")]
    Unknown,
}

impl Display for Framework {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Framework {
    pub fn as_str(&self) -> &'static str {
        match self {
            Framework::NextJs => "nextjs",
            Framework::Vite => "vite",
            Framework::Rsbuild => "rsbuild",
            Framework::CreateReactApp => "create-react-app",
            Framework::Docusaurus => "docusaurus",
            Framework::Dockerfile => "dockerfile",
            Framework::Unknown => "unknown",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "nextjs" => Some(Framework::NextJs),
            "vite" => Some(Framework::Vite),
            "rsbuild" => Some(Framework::Rsbuild),
            "create-react-app" => Some(Framework::CreateReactApp),
            "docusaurus" => Some(Framework::Docusaurus),
            "dockerfile" => Some(Framework::Dockerfile),
            "unknown" => Some(Framework::Unknown),
            _ => None,
        }
    }
}

/// JobStatus enum for deployment job state tracking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DeriveActiveEnum, EnumIter)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum JobStatus {
    #[sea_orm(num_value = 0)]
    Pending,
    #[sea_orm(num_value = 1)]
    Waiting,
    #[sea_orm(num_value = 2)]
    Running,
    #[sea_orm(num_value = 3)]
    Success,
    #[sea_orm(num_value = 4)]
    Failure,
    #[sea_orm(num_value = 5)]
    Cancelled,
    #[sea_orm(num_value = 6)]
    Skipped,
}

impl Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Waiting => "waiting",
            JobStatus::Running => "running",
            JobStatus::Success => "success",
            JobStatus::Failure => "failure",
            JobStatus::Cancelled => "cancelled",
            JobStatus::Skipped => "skipped",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(JobStatus::Pending),
            "waiting" => Some(JobStatus::Waiting),
            "running" => Some(JobStatus::Running),
            "success" => Some(JobStatus::Success),
            "failure" => Some(JobStatus::Failure),
            "cancelled" => Some(JobStatus::Cancelled),
            "skipped" => Some(JobStatus::Skipped),
            _ => None,
        }
    }
}

/// RoleType enum for user roles.
/// NOTE: Use db_type = "Text" for SQLite compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, DeriveActiveEnum, EnumIter)]
#[sea_orm(rs_type = "String", db_type = "Text")]
pub enum RoleType {
    #[sea_orm(string_value = "admin")]
    Admin,
    #[sea_orm(string_value = "user")]
    User,
}

impl Display for RoleType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl RoleType {
    pub fn as_str(&self) -> &'static str {
        match self {
            RoleType::Admin => "admin",
            RoleType::User => "user",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "admin" => Some(RoleType::Admin),
            "user" => Some(RoleType::User),
            _ => None,
        }
    }
}