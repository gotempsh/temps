use std::path::{Path, PathBuf};
use super::PackageManager;
#[derive(Debug, Clone)]
pub enum MonorepoTool {
    Lerna,
    Turbo,
    Nx,
    None,
}

impl MonorepoTool {
    pub fn detect(path: &Path) -> Self {
        if path.join("lerna.json").exists() {
            MonorepoTool::Lerna
        } else if path.join("turbo.json").exists() {
            MonorepoTool::Turbo
        } else if path.join("nx.json").exists() {
            MonorepoTool::Nx
        } else {
            MonorepoTool::None
        }
    }

    pub fn install_command(&self) -> &'static str {
        match self {
            MonorepoTool::Lerna => "npx lerna bootstrap",
            MonorepoTool::Turbo => "npm install",  // Turbo uses the package manager's install
            MonorepoTool::Nx => "npx nx exec -- npm install",
            MonorepoTool::None => "",
        }
    }

    pub fn build_command(&self, package_name: Option<&str>) -> String {
        match self {
            MonorepoTool::Lerna => {
                if let Some(pkg) = package_name {
                    format!("npx lerna run build --scope={}", pkg)
                } else {
                    "npx lerna run build".to_string()
                }
            }
            MonorepoTool::Turbo => {
                if let Some(_pkg) = package_name {
                    "npx turbo build".to_string()
                } else {
                    "npx turbo run build".to_string()
                }
            }
            MonorepoTool::Nx => {
                if let Some(pkg) = package_name {
                    format!("npx nx build {}", pkg)
                } else {
                    "npx nx run-many --target=build --all".to_string()
                }
            }
            MonorepoTool::None => "".to_string(),
        }
    }
}

impl ToString for MonorepoTool {
    fn to_string(&self) -> String {
        match self {
            MonorepoTool::Lerna => "lerna".to_string(),
            MonorepoTool::Turbo => "turbo".to_string(), 
            MonorepoTool::Nx => "nx".to_string(),
            MonorepoTool::None => "none".to_string(),
        }
    }
}


#[derive(Debug, Clone)]
pub struct BuildSystem {
    pub package_manager: PackageManager,
    pub monorepo_tool: MonorepoTool,
}

impl BuildSystem {
    pub fn detect(path: &Path) -> Self {
        Self {
            package_manager: PackageManager::detect(path),
            monorepo_tool: MonorepoTool::detect(path),
        }
    }

    pub fn get_install_command(&self) -> String {
        match (&self.package_manager, &self.monorepo_tool) {
            (PackageManager::Npm, MonorepoTool::Lerna) => "npx lerna bootstrap".to_string(),
            (PackageManager::Yarn, MonorepoTool::Lerna) => "yarn lerna bootstrap".to_string(),
            (PackageManager::Pnpm, MonorepoTool::Lerna) => "pnpm lerna bootstrap".to_string(),
            (PackageManager::Bun, MonorepoTool::Lerna) => "bun lerna bootstrap".to_string(),
            
            (PackageManager::Npm, MonorepoTool::Turbo) => "npm install".to_string(),
            (PackageManager::Yarn, MonorepoTool::Turbo) => "yarn install".to_string(),
            (PackageManager::Pnpm, MonorepoTool::Turbo) => "pnpm install".to_string(),
            (PackageManager::Bun, MonorepoTool::Turbo) => "bun install".to_string(),
            
            (PackageManager::Npm, MonorepoTool::Nx) => "npx nx exec -- npm install".to_string(),
            (PackageManager::Yarn, MonorepoTool::Nx) => "npx nx exec -- yarn install".to_string(),
            (PackageManager::Pnpm, MonorepoTool::Nx) => "npx nx exec -- pnpm install".to_string(),
            (PackageManager::Bun, MonorepoTool::Nx) => "npx nx exec -- bun install".to_string(),
            
            (_, MonorepoTool::None) => self.package_manager.install_command().to_string(),
        }
    }

    pub fn get_build_command(&self, package_name: Option<&str>) -> String {
        match (&self.package_manager, &self.monorepo_tool) {
            (PackageManager::Npm, MonorepoTool::Lerna) => {
                if let Some(pkg) = package_name {
                    format!("npx lerna run build --scope={}", pkg)
                } else {
                    "npx lerna run build".to_string()
                }
            }
            (PackageManager::Yarn, MonorepoTool::Lerna) => {
                if let Some(pkg) = package_name {
                    format!("yarn lerna run build --scope={}", pkg) 
                } else {
                    "yarn lerna run build".to_string()
                }
            }
            (PackageManager::Pnpm, MonorepoTool::Lerna) => {
                if let Some(pkg) = package_name {
                    format!("pnpm lerna run build --scope={}", pkg)
                } else {
                    "pnpm lerna run build".to_string()
                }
            }
            (PackageManager::Bun, MonorepoTool::Lerna) => {
                if let Some(pkg) = package_name {
                    format!("bun lerna run build --scope={}", pkg)
                } else {
                    "bun lerna run build".to_string()
                }
            }
            
            (PackageManager::Npm, MonorepoTool::Turbo) => {
                if let Some(_pkg) = package_name {
                    "npx turbo build".to_string()
                } else {
                    "npx turbo run build".to_string()
                }
            }
            (PackageManager::Yarn, MonorepoTool::Turbo) => {
                if let Some(_pkg) = package_name {
                    "yarn turbo build".to_string()
                } else {
                    "yarn turbo run build".to_string()
                }
            }
            (PackageManager::Pnpm, MonorepoTool::Turbo) => {
                if let Some(_pkg) = package_name {
                    "pnpm turbo build".to_string()
                } else {
                    "pnpm turbo run build".to_string()
                }
            }
            (PackageManager::Bun, MonorepoTool::Turbo) => {
                if let Some(_pkg) = package_name {
                    "bun turbo build".to_string()
                } else {
                    "bun turbo run build".to_string()
                }
            }
            
            (PackageManager::Npm, MonorepoTool::Nx) => {
                if let Some(pkg) = package_name {
                    format!("npx nx build {}", pkg)
                } else {
                    "npx nx run-many --target=build --all".to_string()
                }
            }
            (PackageManager::Yarn, MonorepoTool::Nx) => {
                if let Some(pkg) = package_name {
                    format!("yarn nx build {}", pkg)
                } else {
                    "yarn nx run-many --target=build --all".to_string()
                }
            }
            (PackageManager::Pnpm, MonorepoTool::Nx) => {
                if let Some(pkg) = package_name {
                    format!("pnpm nx build {}", pkg)
                } else {
                    "pnpm nx run-many --target=build --all".to_string()
                }
            }
            (PackageManager::Bun, MonorepoTool::Nx) => {
                if let Some(pkg) = package_name {
                    format!("bun nx build {}", pkg)
                } else {
                    "bun nx run-many --target=build --all".to_string()
                }
            }
            
            (_, MonorepoTool::None) => self.package_manager.build_command().to_string(),
        }
    }
} 