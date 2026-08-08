use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::app::WipsawApp;

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub status: &'static str,
    pub platform: &'static str,
    pub paths: DoctorPaths,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Serialize)]
pub struct DoctorPaths {
    pub config: String,
    pub state: String,
    pub data: String,
    pub runtime: String,
    pub registry: String,
}

#[derive(Debug, Serialize)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub required: bool,
    pub ok: bool,
    pub detail: String,
}

impl DoctorReport {
    pub fn collect(app: &WipsawApp) -> Self {
        let checks = vec![
            check_path("registry", true, &app.paths.registry_path()),
            check_command("tmux", true, app.tmux.binary(), &["-V"]),
            check_command("codex", true, Path::new("codex"), &["--version"]),
            check_command("docker", false, Path::new("docker"), &["--version"]),
            check_command(
                "docker-compose",
                false,
                Path::new("docker"),
                &["compose", "version"],
            ),
            check_command("ssh", false, Path::new("ssh"), &["-V"]),
        ];
        let required_ok = checks
            .iter()
            .filter(|check| check.required)
            .all(|check| check.ok);
        Self {
            status: if required_ok { "ready" } else { "blocked" },
            platform: "linux",
            paths: DoctorPaths {
                config: display(&app.paths.config_dir),
                state: display(&app.paths.state_dir),
                data: display(&app.paths.data_dir),
                runtime: display(&app.paths.runtime_dir),
                registry: display(&app.paths.registry_path()),
            },
            checks,
        }
    }
}

fn check_path(name: &'static str, required: bool, path: &Path) -> DoctorCheck {
    DoctorCheck {
        name,
        required,
        ok: path.is_file(),
        detail: display(path),
    }
}

fn check_command(name: &'static str, required: bool, program: &Path, args: &[&str]) -> DoctorCheck {
    match Command::new(program).args(args).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            DoctorCheck {
                name,
                required,
                ok: output.status.success(),
                detail: if stdout.is_empty() { stderr } else { stdout },
            }
        }
        Err(error) => DoctorCheck {
            name,
            required,
            ok: false,
            detail: error.to_string(),
        },
    }
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
