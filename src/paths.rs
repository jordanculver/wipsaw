use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use directories::BaseDirs;

use crate::error::{Result, WipsawError};

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl AppPaths {
    pub fn from_env() -> Result<Self> {
        let base = BaseDirs::new().ok_or_else(|| WipsawError::InvalidInput {
            field: "home directory",
            message: "could not resolve Linux user directories".to_string(),
        })?;

        let config_dir = path_override("WIPSAW_CONFIG_DIR").unwrap_or_else(|| {
            env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| base.home_dir().join(".config"))
                .join("wipsaw")
        });
        let state_dir = path_override("WIPSAW_STATE_DIR").unwrap_or_else(|| {
            env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| base.home_dir().join(".local/state"))
                .join("wipsaw")
        });
        let data_dir = path_override("WIPSAW_DATA_DIR").unwrap_or_else(|| {
            env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| base.home_dir().join(".local/share"))
                .join("wipsaw")
        });
        let runtime_dir = path_override("WIPSAW_RUNTIME_DIR").unwrap_or_else(|| {
            env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| state_dir.join("runtime"))
                .join("wipsaw")
        });

        Ok(Self {
            config_dir,
            state_dir,
            data_dir,
            runtime_dir,
        })
    }

    pub fn for_test(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            state_dir: root.join("state"),
            data_dir: root.join("data"),
            runtime_dir: root.join("runtime"),
        }
    }

    pub fn ensure(&self) -> Result<()> {
        for path in [
            &self.config_dir,
            &self.state_dir,
            &self.data_dir,
            &self.runtime_dir,
        ] {
            fs::create_dir_all(path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        fs::create_dir_all(self.shortcut_bin_dir())?;
        fs::set_permissions(self.shortcut_bin_dir(), fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    pub fn registry_path(&self) -> PathBuf {
        self.state_dir.join("registry.sqlite3")
    }

    pub fn tmux_config_path(&self) -> PathBuf {
        self.config_dir.join("tmux.conf")
    }

    pub fn bundled_tmux_path(&self) -> PathBuf {
        self.data_dir.join("bin/tmux")
    }

    pub fn shortcut_bin_dir(&self) -> PathBuf {
        self.data_dir.join("shortcuts/bin")
    }

    pub fn shell_dir(&self) -> PathBuf {
        self.data_dir.join("shell")
    }

    pub fn shell_launcher_path(&self) -> PathBuf {
        self.shell_dir().join("wipsaw-shell")
    }
}

fn path_override(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::AppPaths;

    #[test]
    fn test_paths_are_created_private() {
        let root = tempdir().unwrap();
        let paths = AppPaths::for_test(root.path());
        paths.ensure().unwrap();
        assert!(paths.registry_path().parent().unwrap().is_dir());
        let mode = paths.state_dir.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
