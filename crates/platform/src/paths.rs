//! Standard configuration, data, cache, and log directories.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

/// Paths for the current user. Resolving them does not create directories.
#[derive(Debug)]
pub struct AppPaths {
    project: ProjectDirs,
    logs: PathBuf,
}

#[derive(Debug, thiserror::Error)]
#[error("standard application directories are unavailable")]
pub struct PathsUnavailable;

impl AppPaths {
    pub fn discover() -> Result<Self, PathsUnavailable> {
        let project = ProjectDirs::from("org", "", "Ruscello").ok_or(PathsUnavailable)?;
        Ok(Self::from_project(project))
    }

    fn from_project(project: ProjectDirs) -> Self {
        let state = project
            .state_dir()
            .unwrap_or_else(|| project.data_local_dir());
        let logs = state.join("logs");
        Self { project, logs }
    }

    pub fn config(&self) -> &Path {
        self.project.config_dir()
    }

    pub fn data(&self) -> &Path {
        self.project.data_local_dir()
    }

    pub fn cache(&self) -> &Path {
        self.project.cache_dir()
    }

    pub fn logs(&self) -> &Path {
        &self.logs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_absolute_application_directories() -> Result<(), PathsUnavailable> {
        let paths = AppPaths::discover()?;
        for path in [paths.config(), paths.data(), paths.cache(), paths.logs()] {
            assert!(path.is_absolute());
        }
        assert!(paths.logs().ends_with("logs"));
        let state = paths.project.state_dir().unwrap_or_else(|| paths.data());
        assert!(paths.logs().starts_with(state));
        #[cfg(target_os = "windows")]
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            assert!(paths.logs().starts_with(local));
        }
        Ok(())
    }
}
