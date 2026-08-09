use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::error::{Result, WipsawError};
use crate::model::{CodexHome, ModelProfile};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_ATTEMPTS: usize = 3;
const RETRY_DELAY: Duration = Duration::from_millis(150);

#[derive(Debug, Clone, Serialize)]
pub struct CodexHomeProbe {
    pub home_id: String,
    pub home_name: String,
    pub account_id: String,
    pub account_alias: String,
    pub configured_path: String,
    pub reported_path: String,
    pub codex_version: String,
    pub thread_count: usize,
    pub app_server_compatible: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeCodexThread {
    pub native_thread_id: String,
    pub name: String,
    pub cwd: PathBuf,
    pub model: String,
    pub model_provider: String,
    pub reasoning_effort: Option<String>,
    pub status: String,
    pub rollout_path: Option<PathBuf>,
    pub native_created_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NativeCodexThreadInspection {
    pub native_thread_id: String,
    pub name: Option<String>,
    pub cwd: PathBuf,
    pub model_provider: String,
    pub status: String,
    pub rollout_path: Option<PathBuf>,
    pub preview: String,
    pub native_created_at: Option<i64>,
    pub native_updated_at: Option<i64>,
}

pub fn probe_home(home: &CodexHome) -> Result<CodexHomeProbe> {
    let version_output = Command::new(&home.codex_binary)
        .arg("--version")
        .env("CODEX_HOME", &home.path)
        .env("PATH", launch_path(home))
        .output()
        .map_err(|error| WipsawError::ExecutableUnavailable {
            program: home.codex_binary.display().to_string(),
            detail: error.to_string(),
        })?;
    if !version_output.status.success() {
        return Err(WipsawError::CommandFailed {
            program: home.codex_binary.display().to_string(),
            args: "--version".to_string(),
            code: version_output.status.code(),
            stderr: String::from_utf8_lossy(&version_output.stderr)
                .trim()
                .to_string(),
        });
    }
    let codex_version = String::from_utf8_lossy(&version_output.stdout)
        .trim()
        .to_string();

    let mut client = AppServerClient::connect(home)?;
    let listed = client.request("thread/list", json!({"limit": 100}))?;
    let result = listed.get("result").ok_or_else(|| {
        WipsawError::CodexProtocol("thread/list response did not contain a result".to_string())
    })?;
    let threads = result
        .get("data")
        .or_else(|| result.get("threads"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    Ok(CodexHomeProbe {
        home_id: home.id.clone(),
        home_name: home.name.clone(),
        account_id: home.account_id.clone(),
        account_alias: home.account_alias.clone(),
        configured_path: home.path.to_string_lossy().into_owned(),
        reported_path: client.reported_home.clone(),
        codex_version,
        thread_count: threads,
        app_server_compatible: true,
    })
}

pub fn start_named_thread(
    home: &CodexHome,
    cwd: &Path,
    name: &str,
    profile: Option<&ModelProfile>,
) -> Result<NativeCodexThread> {
    let mut client = AppServerClient::connect(home)?;
    let params = thread_start_params(cwd, profile);
    let response = client.request("thread/start", Value::Object(params))?;
    let result = response.get("result").ok_or_else(|| {
        WipsawError::CodexProtocol("thread/start response did not contain a result".to_string())
    })?;
    let thread = result.get("thread").ok_or_else(|| {
        WipsawError::CodexProtocol(
            "thread/start response did not contain result.thread".to_string(),
        )
    })?;
    let native_thread_id = required_string(thread, "id", "thread/start result.thread.id")?;

    if let Err(error) = client.request(
        "thread/name/set",
        json!({"threadId": native_thread_id, "name": name}),
    ) {
        let _ = client.request("thread/archive", json!({"threadId": native_thread_id}));
        return Err(error);
    }

    Ok(NativeCodexThread {
        native_thread_id,
        name: name.to_string(),
        cwd: PathBuf::from(required_string(result, "cwd", "thread/start result.cwd")?),
        model: required_string(result, "model", "thread/start result.model")?,
        model_provider: required_string(
            result,
            "modelProvider",
            "thread/start result.modelProvider",
        )?,
        reasoning_effort: optional_string(result, "reasoningEffort"),
        status: thread_status(thread),
        rollout_path: optional_string(thread, "path").map(PathBuf::from),
        native_created_at: thread.get("createdAt").and_then(Value::as_i64),
    })
}

pub fn archive_thread(home: &CodexHome, native_thread_id: &str) -> Result<()> {
    let mut client = AppServerClient::connect(home)?;
    client.request("thread/archive", json!({"threadId": native_thread_id}))?;
    Ok(())
}

/// Permanently remove a native Codex thread and its persisted rollout data.
pub fn delete_thread(home: &CodexHome, native_thread_id: &str) -> Result<()> {
    let mut client = AppServerClient::connect(home)?;
    client.request("thread/delete", json!({"threadId": native_thread_id}))?;
    Ok(())
}

pub fn inspect_thread(
    home: &CodexHome,
    native_thread_id: &str,
) -> Result<NativeCodexThreadInspection> {
    let mut client = AppServerClient::connect(home)?;
    let response = client.request(
        "thread/read",
        json!({"threadId": native_thread_id, "includeTurns": false}),
    )?;
    let thread = response
        .get("result")
        .and_then(|result| result.get("thread"))
        .ok_or_else(|| {
            WipsawError::CodexProtocol(
                "thread/read response did not contain result.thread".to_string(),
            )
        })?;
    let returned_id = required_string(thread, "id", "thread/read result.thread.id")?;
    if returned_id != native_thread_id {
        return Err(WipsawError::CodexProtocol(format!(
            "thread/read returned ID '{returned_id}' while '{native_thread_id}' was requested"
        )));
    }
    Ok(NativeCodexThreadInspection {
        native_thread_id: returned_id,
        name: optional_string(thread, "name"),
        cwd: PathBuf::from(required_string(
            thread,
            "cwd",
            "thread/read result.thread.cwd",
        )?),
        model_provider: required_string(
            thread,
            "modelProvider",
            "thread/read result.thread.modelProvider",
        )?,
        status: thread_status(thread),
        rollout_path: optional_string(thread, "path").map(PathBuf::from),
        preview: optional_string(thread, "preview").unwrap_or_default(),
        native_created_at: thread.get("createdAt").and_then(Value::as_i64),
        native_updated_at: thread.get("updatedAt").and_then(Value::as_i64),
    })
}

fn thread_start_params(cwd: &Path, profile: Option<&ModelProfile>) -> Map<String, Value> {
    let mut params = Map::from_iter([
        (
            "cwd".to_string(),
            Value::String(cwd.to_string_lossy().into_owned()),
        ),
        (
            "serviceName".to_string(),
            Value::String("wipsaw".to_string()),
        ),
    ]);
    let Some(profile) = profile else {
        return params;
    };

    params.insert("model".to_string(), Value::String(profile.model.clone()));
    if let Some(provider) = &profile.provider {
        params.insert("modelProvider".to_string(), Value::String(provider.clone()));
    }
    if let Some(sandbox) = &profile.sandbox {
        params.insert("sandbox".to_string(), Value::String(sandbox.clone()));
    }
    if let Some(approval_policy) = &profile.approval_policy {
        params.insert(
            "approvalPolicy".to_string(),
            Value::String(approval_policy.clone()),
        );
    }

    let mut config = Map::new();
    if let Some(reasoning_effort) = &profile.reasoning_effort {
        config.insert(
            "model_reasoning_effort".to_string(),
            Value::String(reasoning_effort.clone()),
        );
    }
    if let Some(search) = profile.search {
        config.insert(
            "web_search".to_string(),
            Value::String(if search { "live" } else { "disabled" }.to_string()),
        );
    }
    if !config.is_empty() {
        params.insert("config".to_string(), Value::Object(config));
    }
    params
}

fn required_string(value: &Value, field: &str, description: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            WipsawError::CodexProtocol(format!("{description} was missing or not a string"))
        })
}

fn optional_string(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_string)
}

fn thread_status(thread: &Value) -> String {
    thread
        .get("status")
        .and_then(|status| status.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("notLoaded")
        .to_string()
}

struct AppServerClient {
    stdin: std::process::ChildStdin,
    receiver: Receiver<std::io::Result<String>>,
    diagnostics: CapturedStderr,
    next_id: i64,
    reported_home: String,
    _child: ChildGuard,
}

impl AppServerClient {
    fn connect(home: &CodexHome) -> Result<Self> {
        let mut last_error = None;
        let mut attempts = 0;
        for attempt in 1..=CONNECT_ATTEMPTS {
            attempts = attempt;
            match Self::connect_once(home) {
                Ok(client) => return Ok(client),
                Err(error) => {
                    let retry = retryable_connect_error(&error) && attempt < CONNECT_ATTEMPTS;
                    last_error = Some(error);
                    if !retry {
                        break;
                    }
                    thread::sleep(RETRY_DELAY * attempt as u32);
                }
            }
        }
        let detail = last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "unknown initialization failure".to_string());
        Err(WipsawError::CodexInitialization {
            home: home.name.clone(),
            attempts,
            detail,
        })
    }

    fn connect_once(home: &CodexHome) -> Result<Self> {
        let mut child = Command::new(&home.codex_binary)
            .args(["app-server", "--stdio"])
            .env("CODEX_HOME", &home.path)
            .env("PATH", launch_path(home))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| WipsawError::ExecutableUnavailable {
                program: home.codex_binary.display().to_string(),
                detail: error.to_string(),
            })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            WipsawError::CodexProtocol("app-server stdin was unavailable".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            WipsawError::CodexProtocol("app-server stdout was unavailable".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            WipsawError::CodexProtocol("app-server stderr was unavailable".to_string())
        })?;
        let receiver = spawn_line_reader(stdout);
        let diagnostics = CapturedStderr::spawn(stderr);
        let mut client = Self {
            stdin,
            receiver,
            diagnostics,
            next_id: 1,
            reported_home: String::new(),
            _child: ChildGuard(child),
        };
        let initialized = client.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "wipsaw",
                    "title": "Wipsaw",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        let reported_home = initialized
            .get("result")
            .and_then(|result| result.get("codexHome"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                WipsawError::CodexProtocol(
                    "initialize response did not contain result.codexHome".to_string(),
                )
            })?
            .to_string();
        if Path::new(&reported_home) != home.path {
            return Err(WipsawError::CodexProtocol(format!(
                "server reported Codex home '{}' but '{}' was configured",
                reported_home,
                home.path.display()
            )));
        }
        client.reported_home = reported_home;
        client.notify("initialized", json!({}))?;
        Ok(client)
    }

    fn request(&mut self, method: &'static str, params: Value) -> Result<Value> {
        let request_id = self.next_id;
        self.next_id += 1;
        send(
            &mut self.stdin,
            &json!({"method": method, "id": request_id, "params": params}),
        )
        .map_err(|error| {
            WipsawError::CodexProtocol(with_diagnostics(error.to_string(), &self.diagnostics))
        })?;
        wait_for_response(&self.receiver, &self.diagnostics, request_id, method)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        send(
            &mut self.stdin,
            &json!({"method": method, "params": params}),
        )
        .map_err(|error| {
            WipsawError::CodexProtocol(with_diagnostics(error.to_string(), &self.diagnostics))
        })
    }
}

fn launch_path(home: &CodexHome) -> std::ffi::OsString {
    let launcher_home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    codex_launch_path(&home.codex_binary, &launcher_home)
}

pub(crate) fn codex_launch_path(codex_binary: &Path, launcher_home: &Path) -> OsString {
    let inherited = env::var_os("PATH").unwrap_or_default();
    let mut entries = Vec::new();
    if let Some(runtime) = codex_node_runtime(codex_binary, launcher_home) {
        entries.push(runtime);
    }
    entries.extend(env::split_paths(&inherited));
    let mut seen = HashSet::new();
    entries.retain(|entry| seen.insert(entry.clone()));
    env::join_paths(entries).unwrap_or(inherited)
}

/// npm's Codex entrypoint uses `#!/usr/bin/env node`. Long-lived tmux servers
/// can retain an obsolete Node at the front of PATH, so locate the Node that
/// belongs to the registered Codex installation and place it first.
fn codex_node_runtime(codex_binary: &Path, launcher_home: &Path) -> Option<PathBuf> {
    let canonical = fs::canonicalize(codex_binary).ok();
    for path in std::iter::once(codex_binary).chain(canonical.as_deref()) {
        for ancestor in path.ancestors() {
            let bin = ancestor.join("bin");
            if bin.join("node").is_file() && bin.join("codex").exists() {
                return Some(bin);
            }
        }
    }

    let script = fs::read_to_string(codex_binary).ok()?;
    for marker in ["$HOME/", "${HOME}/"] {
        let mut remainder = script.as_str();
        while let Some(start) = remainder.find(marker) {
            let after = &remainder[start + marker.len()..];
            if let Some(end) = after.find("/bin/codex") {
                let candidate = launcher_home.join(&after[..end + "/bin/codex".len()]);
                if let Some(bin) = candidate.parent()
                    && bin.join("node").is_file()
                {
                    return Some(bin.to_path_buf());
                }
            }
            remainder = after;
        }
    }
    None
}

fn send(stdin: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *stdin, value)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn spawn_line_reader(
    stdout: impl std::io::Read + Send + 'static,
) -> Receiver<std::io::Result<String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

fn wait_for_response(
    receiver: &Receiver<std::io::Result<String>>,
    diagnostics: &CapturedStderr,
    request_id: i64,
    operation: &'static str,
) -> Result<Value> {
    loop {
        let line = match receiver.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(line) => line?,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err(WipsawError::CodexProtocol(with_diagnostics(
                    format!(
                        "app-server timed out after {}s while waiting for {operation}",
                        RESPONSE_TIMEOUT.as_secs()
                    ),
                    diagnostics,
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(WipsawError::CodexProtocol(with_diagnostics(
                    format!("app-server closed while waiting for {operation}"),
                    diagnostics,
                )));
            }
        };
        let value: Value = serde_json::from_str(&line).map_err(|error| {
            WipsawError::CodexProtocol(with_diagnostics(
                format!("invalid JSON from app-server: {error}"),
                diagnostics,
            ))
        })?;
        if value.get("id").and_then(Value::as_i64) != Some(request_id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(WipsawError::CodexProtocol(with_diagnostics(
                format!("request {request_id} failed: {error}"),
                diagnostics,
            )));
        }
        return Ok(value);
    }
}

fn retryable_connect_error(error: &WipsawError) -> bool {
    match error {
        WipsawError::CodexProtocol(message) => {
            message.contains("closed while waiting for initialize")
                || message.contains("timed out")
                || message.contains("Broken pipe")
        }
        // During initialization, any pipe I/O error can be a child startup
        // race. Deterministic executable/config errors use other variants.
        WipsawError::Io(_) => true,
        WipsawError::Timeout { .. } => true,
        _ => false,
    }
}

#[derive(Clone, Default)]
struct CapturedStderr(Arc<Mutex<Vec<String>>>);

impl CapturedStderr {
    fn spawn(stderr: impl std::io::Read + Send + 'static) -> Self {
        let captured = Self::default();
        let writer = captured.clone();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(|line| line.ok()) {
                let line = line
                    .chars()
                    .filter(|character| !character.is_control() || *character == '\t')
                    .collect::<String>();
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(mut lines) = writer.0.lock() {
                    lines.push(line);
                    if lines.len() > 12 {
                        lines.remove(0);
                    }
                }
            }
        });
        captured
    }

    fn snapshot(&self) -> String {
        // stdout and stderr close together; give the stderr reader one short
        // scheduling window before constructing the actionable error.
        thread::sleep(Duration::from_millis(20));
        let joined = self
            .0
            .lock()
            .map(|lines| lines.join(" | "))
            .unwrap_or_default();
        joined.chars().take(2_000).collect()
    }
}

fn with_diagnostics(message: String, diagnostics: &CapturedStderr) -> String {
    let stderr = diagnostics.snapshot();
    if stderr.is_empty() {
        message
    } else {
        format!("{message}; stderr: {stderr}")
    }
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::{delete_thread, inspect_thread, probe_home, start_named_thread};
    use crate::model::{CodexHome, ModelProfile};

    fn test_home(binary: std::path::PathBuf, path: std::path::PathBuf) -> CodexHome {
        CodexHome {
            id: "home_test".to_string(),
            name: "test".to_string(),
            host_id: "host_local".to_string(),
            account_id: "acct_test".to_string(),
            account_alias: "test".to_string(),
            path,
            codex_binary: binary,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn probe_uses_the_registered_home_and_counts_threads() {
        let root = tempdir().unwrap();
        let home_path = root.path().join("codex-home");
        fs::create_dir(&home_path).unwrap();
        let binary = root.path().join("fake-codex");
        fs::write(
            &binary,
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s\n' 'codex-cli test'
  exit 0
fi
IFS= read -r initialize
printf '{"id":1,"result":{"codexHome":"%s"}}\n' "$CODEX_HOME"
IFS= read -r initialized
IFS= read -r list
printf '%s\n' '{"id":2,"result":{"data":[{"id":"one"},{"id":"two"}]}}'
"#,
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let home = test_home(binary, home_path.clone());
        let report = probe_home(&home).unwrap();
        assert_eq!(report.reported_path, home_path.to_string_lossy());
        assert_eq!(report.thread_count, 2);
    }

    #[test]
    fn named_thread_receives_all_profile_overrides() {
        let root = tempdir().unwrap();
        let home_path = root.path().join("codex-home");
        let project_path = home_path.join("project");
        fs::create_dir_all(&project_path).unwrap();
        let binary = root.path().join("fake-codex");
        fs::write(
            &binary,
            r#"#!/bin/sh
IFS= read -r initialize
printf '{"id":1,"result":{"codexHome":"%s"}}\n' "$CODEX_HOME"
IFS= read -r initialized
IFS= read -r request
case "$request" in
  *thread/start*)
    printf '%s\n' "$request" >> "$CODEX_HOME/requests.jsonl"
    printf '{"id":2,"result":{"thread":{"id":"native-123","createdAt":1700000000,"status":{"type":"idle"},"path":"%s/rollout.jsonl"},"model":"gpt-test","modelProvider":"openai","cwd":"%s/project","reasoningEffort":"high"}}\n' "$CODEX_HOME" "$CODEX_HOME"
    IFS= read -r set_name
    printf '%s\n' "$set_name" >> "$CODEX_HOME/requests.jsonl"
    printf '%s\n' '{"id":3,"result":{}}'
    ;;
  *thread/read*)
    printf '{"id":2,"result":{"thread":{"id":"native-123","name":"API work","preview":"","modelProvider":"openai","createdAt":1700000000,"updatedAt":1700000001,"status":{"type":"notLoaded"},"path":"%s/rollout.jsonl","cwd":"%s/project"}}}\n' "$CODEX_HOME" "$CODEX_HOME"
    ;;
  *thread/delete*)
    printf '%s\n' "$request" >> "$CODEX_HOME/requests.jsonl"
    printf '%s\n' '{"id":2,"result":{}}'
    ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let home = test_home(binary, home_path.clone());
        let profile = ModelProfile {
            id: "profile_test".to_string(),
            name: "careful".to_string(),
            provider: Some("openai".to_string()),
            model: "gpt-test".to_string(),
            reasoning_effort: Some("high".to_string()),
            search: Some(true),
            sandbox: Some("workspace-write".to_string()),
            approval_policy: Some("on-request".to_string()),
            created_at: String::new(),
            updated_at: String::new(),
        };

        let thread = start_named_thread(&home, &project_path, "API work", Some(&profile)).unwrap();
        assert_eq!(thread.native_thread_id, "native-123");
        assert_eq!(thread.name, "API work");
        assert_eq!(thread.reasoning_effort.as_deref(), Some("high"));

        let requests = fs::read_to_string(home_path.join("requests.jsonl")).unwrap();
        let values = requests
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        let params = &values[0]["params"];
        assert_eq!(params["model"], "gpt-test");
        assert_eq!(params["modelProvider"], "openai");
        assert_eq!(params["sandbox"], "workspace-write");
        assert_eq!(params["approvalPolicy"], "on-request");
        assert_eq!(params["config"]["model_reasoning_effort"], "high");
        assert_eq!(params["config"]["web_search"], "live");
        assert_eq!(values[1]["params"]["threadId"], "native-123");
        assert_eq!(values[1]["params"]["name"], "API work");

        let inspection = inspect_thread(&home, "native-123").unwrap();
        assert_eq!(inspection.name.as_deref(), Some("API work"));
        assert_eq!(inspection.status, "notLoaded");
        assert_eq!(inspection.native_updated_at, Some(1_700_000_001));

        delete_thread(&home, "native-123").unwrap();
        let requests = fs::read_to_string(home_path.join("requests.jsonl")).unwrap();
        let delete = requests.lines().last().unwrap();
        let delete: Value = serde_json::from_str(delete).unwrap();
        assert_eq!(delete["method"], "thread/delete");
        assert_eq!(delete["params"]["threadId"], "native-123");
    }

    #[test]
    fn initialization_retries_transient_app_server_closes() {
        let root = tempdir().unwrap();
        let home_path = root.path().join("codex-home");
        fs::create_dir(&home_path).unwrap();
        let binary = root.path().join("flaky-codex");
        fs::write(
            &binary,
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s\n' 'codex-cli test'
  exit 0
fi
counter="$CODEX_HOME/start-count"
count=0
if [ -r "$counter" ]; then count=$(sed -n '1p' "$counter"); fi
count=$((count + 1))
printf '%s\n' "$count" > "$counter"
if [ "$count" -lt 3 ]; then
  printf '%s\n' 'temporary app-server startup failure' >&2
  exit 1
fi
IFS= read -r initialize
printf '{"id":1,"result":{"codexHome":"%s"}}\n' "$CODEX_HOME"
IFS= read -r initialized
IFS= read -r list
printf '%s\n' '{"id":2,"result":{"data":[]}}'
"#,
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();

        let report = probe_home(&test_home(binary, home_path.clone())).unwrap();
        assert!(report.app_server_compatible);
        assert_eq!(
            fs::read_to_string(home_path.join("start-count"))
                .unwrap()
                .trim(),
            "3"
        );
    }

    #[test]
    fn initialization_error_includes_captured_stderr_and_recovery_hint() {
        let root = tempdir().unwrap();
        let home_path = root.path().join("codex-home");
        fs::create_dir(&home_path).unwrap();
        let binary = root.path().join("broken-codex");
        fs::write(
            &binary,
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf '%s\n' 'codex-cli test'
  exit 0
fi
printf '%s\n' 'fatal init: state database is locked' >&2
exit 1
"#,
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();

        let error = probe_home(&test_home(binary, home_path)).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("after 3 attempt(s)"), "{message}");
        assert!(
            message.contains("fatal init: state database is locked"),
            "{message}"
        );
        assert!(message.contains("wipsaw init"), "{message}");
    }
}
