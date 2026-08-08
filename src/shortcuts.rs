use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::paths::AppPaths;

pub fn install(paths: &AppPaths, wipsaw_binary: &Path) -> Result<Vec<PathBuf>> {
    let bin_dir = paths.shortcut_bin_dir();
    fs::create_dir_all(&bin_dir)?;
    fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o700))?;

    let executable = shell_quote(wipsaw_binary);
    let definitions = [
        ("wipsaw", format!("#!/bin/sh\nexec {executable} \"$@\"\n")),
        (
            "codex",
            format!("#!/bin/sh\nexec {executable} shortcut codex -- \"$@\"\n"),
        ),
        (
            "manager",
            format!("#!/bin/sh\nexec {executable} shortcut manager\n"),
        ),
        (
            "lumbergh",
            format!("#!/bin/sh\nexec {executable} shortcut manager\n"),
        ),
        (
            "lumberg",
            format!("#!/bin/sh\nexec {executable} shortcut manager\n"),
        ),
    ];
    let mut installed = Vec::with_capacity(definitions.len());
    for (name, contents) in definitions {
        let path = bin_dir.join(name);
        let needs_write = fs::read_to_string(&path)
            .map(|existing| existing != contents)
            .unwrap_or(true);
        if needs_write {
            fs::write(&path, contents)?;
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        installed.push(path);
    }
    Ok(installed)
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use tempfile::tempdir;

    use super::install;
    use crate::paths::AppPaths;

    #[test]
    fn installs_private_executable_shortcuts_without_shell_mutation() {
        let root = tempdir().unwrap();
        let paths = AppPaths::for_test(root.path());
        paths.ensure().unwrap();
        let installed = install(&paths, Path::new("/opt/Wip's Saw/wipsaw")).unwrap();
        assert_eq!(installed.len(), 5);
        let codex = fs::read_to_string(paths.shortcut_bin_dir().join("codex")).unwrap();
        assert!(codex.contains("'/opt/Wip'\"'\"'s Saw/wipsaw' shortcut codex"));
        let mode = fs::metadata(paths.shortcut_bin_dir().join("lumbergh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
        assert!(paths.shortcut_bin_dir().join("lumberg").is_file());
        assert!(paths.shortcut_bin_dir().join("wipsaw").is_file());
        assert!(!root.path().join(".zshrc").exists());
    }
}
