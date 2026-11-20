// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::str::FromStr;

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
}

impl FromStr for PipelineStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(PipelineStatus::Running),
            "completed" => Ok(PipelineStatus::Completed),
            "failed" => Ok(PipelineStatus::Failed),
            "cancelled" => Ok(PipelineStatus::Cancelled),
            "pending" => Ok(PipelineStatus::Pending),
            "built" => Ok(PipelineStatus::Built),
            _ => Err(format!("Invalid PipelineStatus: {}", s)),
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
    /// Curated, template-backed service whose applied release is persisted on
    /// the project and can be upgraded independently of the live catalog.
    #[sea_orm(string_value = "service")]
    Service,
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
            ProjectType::Service => "service",
        }
    }
}

impl FromStr for ProjectType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "static" => Ok(ProjectType::Static),
            "server" => Ok(ProjectType::Server),
            "service" => Ok(ProjectType::Service),
            _ => Err(format!("Invalid ProjectType: {}", s)),
        }
    }
}

/// Whether Temps runs a project's app.
///
/// `External` projects run elsewhere (Vercel, Netlify, a VPS) and only send
/// Temps their analytics, errors and traces; they are never deployed.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    DeriveActiveEnum,
    EnumIter,
    utoipa::ToSchema,
)]
#[sea_orm(rs_type = "String", db_type = "Text")]
#[serde(rename_all = "snake_case")]
pub enum Hosting {
    /// Temps builds and runs the app.
    #[default]
    #[sea_orm(string_value = "hosted")]
    Hosted,
    /// The app runs elsewhere; Temps only observes it.
    #[sea_orm(string_value = "external")]
    External,
}

impl Hosting {
    pub fn as_str(&self) -> &'static str {
        match self {
            Hosting::Hosted => "hosted",
            Hosting::External => "external",
        }
    }
}

impl Hosting {
    /// Why a project with this hosting cannot be deployed, if it cannot.
    pub fn deploy_refusal(&self, project_name: &str) -> Option<String> {
        match self {
            Hosting::Hosted => None,
            Hosting::External => Some(format!(
                "{project_name} runs outside Temps (hosting: external), so Temps does not \
                 deploy it. To deploy it here, switch it to hosted in the project's settings."
            )),
        }
    }
}

impl Display for Hosting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
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
}

impl FromStr for Framework {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nextjs" => Ok(Framework::NextJs),
            "vite" => Ok(Framework::Vite),
            "rsbuild" => Ok(Framework::Rsbuild),
            "create-react-app" => Ok(Framework::CreateReactApp),
            "docusaurus" => Ok(Framework::Docusaurus),
            "dockerfile" => Ok(Framework::Dockerfile),
            "unknown" => Ok(Framework::Unknown),
            _ => Err(format!("Invalid Framework: {}", s)),
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
}

impl FromStr for JobStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(JobStatus::Pending),
            "waiting" => Ok(JobStatus::Waiting),
            "running" => Ok(JobStatus::Running),
            "success" => Ok(JobStatus::Success),
            "failure" => Ok(JobStatus::Failure),
            "cancelled" => Ok(JobStatus::Cancelled),
            "skipped" => Ok(JobStatus::Skipped),
            _ => Err(format!("Invalid JobStatus: {}", s)),
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
}

impl FromStr for RoleType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "admin" => Ok(RoleType::Admin),
            "user" => Ok(RoleType::User),
            _ => Err(format!("Invalid RoleType: {}", s)),
        }
    }
}
