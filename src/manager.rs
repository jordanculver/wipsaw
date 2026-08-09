use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use serde_json::{Value, json};

use crate::error::{Result, WipsawError};
use crate::model::{CodexHome, ManagerKind, ManagerSession};
use crate::paths::AppPaths;

pub const MANAGER_MODEL: &str = "gpt-5.6-terra";
pub const MANAGER_REASONING_EFFORT: &str = "medium";

const MANAGER_SKILL: &str = r#"---
name: wipsaw-manager
description: Manage Wipsaw workspaces, tabs, Codex sessions, identities, model profiles, and WIPs through the private Wipsaw manager MCP tools.
---

# Wipsaw manager

Use only the tools from the `wipsaw` MCP server for Wipsaw operations.

- Call `manager_guide` before the first operation in a session.
- Call `run_wipsaw` with a structured `args` array. Always begin with `--json`.
- Inspect current state before changing it.
- Wipsaw nouns are singular CLI groups. For example, list workspaces with `args: ["--json", "workspace", "list"]`.
- Use Wipsaw workspace, tab, thread, account, home, and profile commands instead of invoking tmux or Codex directly.
- Never print, copy, or request raw authentication tokens. Work with Wipsaw account and Codex-home references.
- Do not attach to a tmux client or launch an interactive TUI from this non-interactive manager session.
- Explain destructive or externally visible operations before doing them.
- Never invent a command result. If a Wipsaw command fails or returns no usable output, report the failure plainly and stop.
- If Wipsaw does not expose a requested operation yet, say so plainly and suggest the smallest safe next step.
"#;

#[derive(Debug, Clone)]
pub struct ManagerRuntime {
    pub codex_home: PathBuf,
    pub user_home: PathBuf,
    pub launcher_home: PathBuf,
    pub work_dir: PathBuf,
    pub disabled_user_skills: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ManagerTurnRequest {
    pub session_id: String,
    pub native_thread_id: Option<String>,
    pub codex_binary: PathBuf,
    pub runtime: ManagerRuntime,
    pub prompt: String,
    pub environment: Vec<(OsString, OsString)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagerUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone)]
pub struct ManagerTurnResult {
    pub session_id: String,
    pub native_thread_id: String,
    pub message: String,
    pub usage: Option<ManagerUsage>,
}

#[derive(Debug, Clone)]
pub enum ManagerEvent {
    Started,
    Activity(String),
    Finished(std::result::Result<ManagerTurnResult, String>),
}

#[derive(Debug, Clone)]
pub struct ManagerCommandSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub current_dir: PathBuf,
    pub environment: Vec<(OsString, OsString)>,
}

pub fn prepare_runtime(
    paths: &AppPaths,
    source_home: &CodexHome,
    session: &ManagerSession,
    launcher_home: &Path,
) -> Result<ManagerRuntime> {
    let root = paths.manager_home_root(&source_home.id);
    let codex_home = root.join("codex");
    let user_home = root.join("user");
    let work_dir = root.join("work").join(&session.id);
    for directory in [
        &root,
        &codex_home,
        &user_home,
        &work_dir,
        &codex_home.join("skills"),
        &codex_home.join("skills/wipsaw-manager"),
    ] {
        fs::create_dir_all(directory)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }

    link_auth(
        &source_home.path.join("auth.json"),
        &codex_home.join("auth.json"),
    )?;
    write_private_file(
        &codex_home.join("skills/wipsaw-manager/SKILL.md"),
        MANAGER_SKILL,
    )?;
    write_private_file(&work_dir.join("AGENTS.md"), &manager_instructions(session))?;

    Ok(ManagerRuntime {
        codex_home,
        user_home,
        launcher_home: launcher_home.to_path_buf(),
        work_dir,
        disabled_user_skills: discover_user_skills(&launcher_home.join(".agents/skills"))?,
    })
}

pub fn spawn_turn(request: ManagerTurnRequest) -> Receiver<ManagerEvent> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(ManagerEvent::Started);
        let result = run_turn(request, &sender).map_err(|error| error.to_string());
        let _ = sender.send(ManagerEvent::Finished(result));
    });
    receiver
}

/// Serve the small, capability-scoped MCP surface used by Lumbergh and Middle Managers.
/// The model never receives a general shell tool; this process is the only route to Wipsaw.
pub fn run_manager_mcp_server() -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                write_mcp_message(
                    &mut writer,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32700, "message": format!("parse error: {error}") }
                    }),
                )?;
                continue;
            }
        };
        if let Some(response) = manager_mcp_response(&request) {
            write_mcp_message(&mut writer, &response)?;
        }
    }
    Ok(())
}

fn write_mcp_message(writer: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn manager_mcp_response(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    Some(match method {
        "initialize" => {
            let protocol_version = request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-03-26");
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": protocol_version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "wipsaw-manager",
                        "title": "Wipsaw Manager",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            })
        }
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": manager_mcp_tools()
            }
        }),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let result = match name {
                "manager_guide" => manager_tool_result(MANAGER_SKILL, false, None),
                "run_wipsaw" => run_wipsaw_tool(request.pointer("/params/arguments")),
                _ => manager_tool_result(
                    &format!("unknown Wipsaw manager tool '{name}'"),
                    true,
                    None,
                ),
            };
            json!({ "jsonrpc": "2.0", "id": id, "result": result })
        }
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("method not found: {method}") }
        }),
    })
}

fn manager_mcp_tools() -> Value {
    json!([
        {
            "name": "manager_guide",
            "description": "Read the complete Wipsaw manager operating guide before the first operation.",
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "openWorldHint": false
            },
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": "run_wipsaw",
            "description": "Run one validated Wipsaw JSON command. Use singular groups such as workspace, tab, account, home, thread, and profile.",
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": false,
                "openWorldHint": false
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "args": {
                        "type": "array",
                        "description": "CLI arguments beginning with --json, for example [\"--json\", \"workspace\", \"list\"].",
                        "items": { "type": "string", "maxLength": 4096 },
                        "minItems": 3,
                        "maxItems": 128
                    }
                },
                "required": ["args"],
                "additionalProperties": false
            }
        }
    ])
}

fn run_wipsaw_tool(arguments: Option<&Value>) -> Value {
    let args = arguments
        .and_then(|arguments| arguments.get("args"))
        .and_then(Value::as_array)
        .and_then(|args| {
            args.iter()
                .map(|arg| arg.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        });
    let Some(args) = args else {
        return manager_tool_result("'args' must be an array of strings", true, None);
    };
    if let Err(message) = validate_manager_command(&args) {
        return manager_tool_result(&message, true, None);
    }
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => {
            return manager_tool_result(
                &format!("could not resolve the Wipsaw executable: {error}"),
                true,
                None,
            );
        }
    };
    match Command::new(&executable)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if !output.status.success() {
                let message = if stderr.is_empty() {
                    format!("Wipsaw command failed with exit {:?}", output.status.code())
                } else {
                    stderr
                };
                return manager_tool_result(&message, true, None);
            }
            let structured = serde_json::from_str::<Value>(&stdout)
                .ok()
                .map(|result| json!({ "result": result }));
            manager_tool_result(
                if stdout.is_empty() {
                    "Wipsaw command completed without output"
                } else {
                    &stdout
                },
                false,
                structured,
            )
        }
        Err(error) => manager_tool_result(
            &format!("could not start the Wipsaw command: {error}"),
            true,
            None,
        ),
    }
}

fn manager_tool_result(text: &str, is_error: bool, structured: Option<Value>) -> Value {
    let mut result = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error
    });
    if let Some(structured) = structured {
        result["structuredContent"] = structured;
    }
    result
}

fn validate_manager_command(args: &[String]) -> std::result::Result<(), String> {
    if args.len() < 3 || args.len() > 128 {
        return Err("a manager command must contain --json, a group, and an action".to_string());
    }
    if args.first().map(String::as_str) != Some("--json") {
        return Err("manager commands must begin with --json".to_string());
    }
    if args.iter().any(|arg| arg.chars().count() > 4_096) {
        return Err("a manager command argument is too long".to_string());
    }
    if args.iter().any(|arg| arg == "--attach") {
        return Err("manager commands cannot attach an interactive terminal".to_string());
    }
    let group = args[1].as_str();
    let action = args[2].as_str();
    let allowed = matches!(
        (group, action),
        ("workspace", "create" | "list" | "start")
            | ("tab", "create" | "list" | "rename")
            | ("account", "add" | "list")
            | ("home", "add" | "list")
            | ("thread", "create" | "list" | "inspect" | "resume")
            | ("profile", "add" | "list")
    );
    if !allowed {
        return Err(format!(
            "Wipsaw manager command '{group} {action}' is not allowed"
        ));
    }
    Ok(())
}

pub fn command_spec(request: &ManagerTurnRequest) -> ManagerCommandSpec {
    let manager_executable = request
        .environment
        .iter()
        .find(|(name, _)| name == OsStr::new("WIPSAW_MANAGER_EXECUTABLE"))
        .map(|(_, value)| value.as_os_str())
        .unwrap_or_else(|| request.runtime.work_dir.as_os_str());
    let mut args = vec![OsString::from("exec")];
    if request.native_thread_id.is_some() {
        args.push(OsString::from("resume"));
    }
    args.extend([
        OsString::from("--json"),
        OsString::from("--ignore-user-config"),
        OsString::from("--ignore-rules"),
        OsString::from("--skip-git-repo-check"),
        OsString::from("--model"),
        OsString::from(MANAGER_MODEL),
    ]);
    push_config(&mut args, "model_reasoning_effort=\"medium\"");
    push_config(&mut args, "sandbox_mode=\"read-only\"");
    push_config(&mut args, "approval_policy=\"never\"");
    push_config(&mut args, "skills.bundled.enabled=false");
    push_config(&mut args, "skills.include_instructions=true");
    push_config(&mut args, "tools.web_search=false");
    push_config(&mut args, "tools.view_image=false");
    push_config(&mut args, "features.apps=false");
    push_config(&mut args, "features.tool_search=false");
    push_config(&mut args, "features.tool_suggest=false");
    push_config(&mut args, "features.plugins=false");
    push_config(&mut args, "features.image_generation=false");
    push_config(&mut args, "features.multi_agent=false");
    push_config(&mut args, "features.skill_mcp_dependency_install=false");
    push_config(&mut args, "features.shell_tool=false");
    push_config(&mut args, "features.unified_exec=false");
    push_config(&mut args, "features.code_mode=false");
    push_config(&mut args, "features.js_repl=false");
    push_config(&mut args, "features.apply_patch_freeform=false");
    push_config(
        &mut args,
        &format!(
            "mcp_servers.wipsaw.command={}",
            serde_json::to_string(&manager_executable.to_string_lossy())
                .expect("manager executable serializes")
        ),
    );
    push_config(&mut args, "mcp_servers.wipsaw.args=[\"manager-mcp\"]");
    push_config(&mut args, "mcp_servers.wipsaw.enabled=true");
    push_config(&mut args, "mcp_servers.wipsaw.required=true");
    push_config(&mut args, "mcp_servers.wipsaw.startup_timeout_sec=10");
    push_config(&mut args, "shell_environment_policy.inherit=\"core\"");
    push_config(
        &mut args,
        &format!(
            "shell_environment_policy.set.HOME={}",
            serde_json::to_string(&request.runtime.user_home.to_string_lossy())
                .expect("manager HOME serializes")
        ),
    );
    if !request.runtime.disabled_user_skills.is_empty() {
        let disabled = request
            .runtime
            .disabled_user_skills
            .iter()
            .map(|path| {
                format!(
                    "{{ path = {}, enabled = false }}",
                    serde_json::to_string(&path.to_string_lossy()).expect("skill path serializes")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        push_config(&mut args, &format!("skills.config=[{disabled}]"));
    }
    for (name, value) in &request.environment {
        let name = name.to_string_lossy();
        let value =
            serde_json::to_string(&value.to_string_lossy()).expect("environment serializes");
        push_config(
            &mut args,
            &format!("shell_environment_policy.set.{name}={value}"),
        );
    }
    if let Some(thread_id) = &request.native_thread_id {
        args.push(OsString::from(thread_id));
    } else {
        args.push(OsString::from("--cd"));
        args.push(request.runtime.work_dir.as_os_str().to_owned());
    }
    // Reading the prompt from stdin keeps user text out of the process list.
    args.push(OsString::from("-"));

    let mut environment = request.environment.clone();
    environment.push((
        OsString::from("CODEX_HOME"),
        request.runtime.codex_home.as_os_str().to_owned(),
    ));
    environment.push((
        OsString::from("HOME"),
        request.runtime.launcher_home.as_os_str().to_owned(),
    ));
    ManagerCommandSpec {
        program: request.codex_binary.clone(),
        args,
        current_dir: request.runtime.work_dir.clone(),
        environment,
    }
}

fn run_turn(
    request: ManagerTurnRequest,
    sender: &Sender<ManagerEvent>,
) -> Result<ManagerTurnResult> {
    let spec = command_spec(&request);
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in &spec.environment {
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| WipsawError::ExecutableUnavailable {
            program: spec.program.display().to_string(),
            detail: error.to_string(),
        })?;
    child
        .stdin
        .take()
        .ok_or_else(|| WipsawError::CodexProtocol("codex exec stdin was unavailable".to_string()))?
        .write_all(request.prompt.as_bytes())?;

    let stderr = child.stderr.take().ok_or_else(|| {
        WipsawError::CodexProtocol("codex exec stderr was unavailable".to_string())
    })?;
    let stderr_reader = thread::spawn(move || {
        let mut output = String::new();
        let mut reader = BufReader::new(stderr);
        let _ = reader.read_to_string(&mut output);
        output
    });
    let stdout = child.stdout.take().ok_or_else(|| {
        WipsawError::CodexProtocol("codex exec stdout was unavailable".to_string())
    })?;

    let mut native_thread_id = request.native_thread_id.clone();
    let mut final_message = None;
    let mut usage = None;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(&line).map_err(|error| {
            WipsawError::CodexProtocol(format!("invalid codex exec JSONL event: {error}"))
        })?;
        match event.get("type").and_then(Value::as_str) {
            Some("thread.started") => {
                native_thread_id = event
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(native_thread_id);
            }
            Some("item.started") | Some("item.updated") | Some("item.completed") => {
                if let Some(item) = event.get("item") {
                    match item.get("type").and_then(Value::as_str) {
                        Some("agent_message") if event["type"] == "item.completed" => {
                            final_message =
                                item.get("text").and_then(Value::as_str).map(str::to_string);
                        }
                        Some("command_execution") => {
                            let command = item
                                .get("command")
                                .and_then(Value::as_str)
                                .unwrap_or("Wipsaw command");
                            let summary = truncate_activity(command, 72);
                            let _ = sender.send(ManagerEvent::Activity(format!(
                                "{} {summary}",
                                if event["type"] == "item.completed" {
                                    "finished"
                                } else {
                                    "running"
                                }
                            )));
                        }
                        Some("file_change") => {
                            let _ = sender.send(ManagerEvent::Activity(
                                "applying a managed change".to_string(),
                            ));
                        }
                        Some("mcp_tool_call") => {
                            let tool = item
                                .get("tool")
                                .or_else(|| item.get("name"))
                                .and_then(Value::as_str)
                                .unwrap_or("Wipsaw tool");
                            let _ = sender.send(ManagerEvent::Activity(format!(
                                "{} {tool}",
                                if event["type"] == "item.completed" {
                                    "finished"
                                } else {
                                    "running"
                                }
                            )));
                        }
                        _ => {}
                    }
                }
            }
            Some("turn.completed") => {
                let data = event.get("usage").unwrap_or(&Value::Null);
                usage = Some(ManagerUsage {
                    input_tokens: data
                        .get("input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    cached_input_tokens: data
                        .get("cached_input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    output_tokens: data
                        .get("output_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                });
            }
            Some("turn.failed") | Some("error") => {
                let message = event
                    .pointer("/error/message")
                    .or_else(|| event.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex manager turn failed");
                return Err(WipsawError::CodexProtocol(message.to_string()));
            }
            _ => {}
        }
    }

    let status = child.wait()?;
    let stderr = stderr_reader
        .join()
        .unwrap_or_else(|_| "failed to read codex exec stderr".to_string());
    if !status.success() {
        return Err(WipsawError::CodexProtocol(format!(
            "manager exec failed (exit {:?}): {}",
            status.code(),
            tail(&stderr, 2_000)
        )));
    }
    let native_thread_id = native_thread_id.ok_or_else(|| {
        WipsawError::CodexProtocol("codex exec did not report a thread ID".to_string())
    })?;
    let message = final_message.ok_or_else(|| {
        WipsawError::CodexProtocol("codex exec completed without an agent message".to_string())
    })?;
    Ok(ManagerTurnResult {
        session_id: request.session_id,
        native_thread_id,
        message,
        usage,
    })
}

fn manager_instructions(session: &ManagerSession) -> String {
    let identity = match session.kind {
        ManagerKind::Lumbergh => {
            "You are Lumbergh, Wipsaw's single top-level manager in the dashboard. Coordinate across all registered workspaces and explain the overall state clearly.".to_string()
        }
        ManagerKind::MiddleManager => format!(
            "You are the Middle Manager for Wipsaw workspace '{}' ({}) at '{}'. Stay focused on this workspace and escalate cross-workspace decisions to Lumbergh.",
            session.workspace_name.as_deref().unwrap_or("workspace"),
            session.workspace_id.as_deref().unwrap_or("unknown"),
            session.cwd.display(),
        ),
    };
    format!(
        "# Wipsaw manager context\n\n{identity}\n\nBefore acting, use the `wipsaw-manager` skill and call the private `wipsaw` MCP server's `manager_guide`. Operate only through its Wipsaw tools; no shell, personal MCP, plugin, app, or unrelated skill is available. Never invent state when a tool fails. Do not attach tmux or launch an interactive TUI. Be concise, confirm resulting IDs, and tell the user when a requested Wipsaw capability is not implemented yet.\n"
    )
}

fn link_auth(source: &Path, destination: &Path) -> Result<()> {
    if !source.is_file() {
        return Err(WipsawError::InvalidInput {
            field: "manager authentication",
            message: format!(
                "'{}' is missing; log in with the selected Codex home before using its manager",
                source.display()
            ),
        });
    }
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current = fs::read_link(destination)?;
            if current != source {
                return Err(WipsawError::InvalidInput {
                    field: "manager authentication",
                    message: format!(
                        "'{}' points to an unexpected authentication file",
                        destination.display()
                    ),
                });
            }
        }
        Ok(_) => {
            return Err(WipsawError::InvalidInput {
                field: "manager authentication",
                message: format!(
                    "'{}' already exists and is not a Wipsaw-managed link",
                    destination.display()
                ),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => symlink(source, destination)?,
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn discover_user_skills(root: &Path) -> Result<Vec<PathBuf>> {
    let mut skills = Vec::new();
    discover_user_skills_under(root, root, 0, &mut skills)?;
    skills.sort();
    skills.dedup();
    Ok(skills)
}

fn discover_user_skills_under(
    root: &Path,
    directory: &Path,
    depth: usize,
    skills: &mut Vec<PathBuf>,
) -> Result<()> {
    if depth > 5 || !directory.is_dir() {
        return Ok(());
    }
    if directory != root && directory.join("SKILL.md").is_file() {
        let skill_file = directory.join("SKILL.md");
        skills.push(fs::canonicalize(&skill_file).unwrap_or(skill_file));
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        if entry.file_type()?.is_dir() || entry.file_type()?.is_symlink() {
            discover_user_skills_under(root, &entry.path(), depth + 1, skills)?;
        }
    }
    Ok(())
}

fn write_private_file(path: &Path, contents: &str) -> Result<()> {
    let needs_write = fs::read_to_string(path)
        .map(|existing| existing != contents)
        .unwrap_or(true);
    if needs_write {
        fs::write(path, contents)?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn push_config(args: &mut Vec<OsString>, value: &str) {
    args.push(OsString::from("--config"));
    args.push(OsString::from(value));
}

fn truncate_activity(value: &str, limit: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= limit {
        return value;
    }
    format!(
        "{}…",
        value
            .chars()
            .take(limit.saturating_sub(1))
            .collect::<String>()
    )
}

fn tail(value: &str, max_chars: usize) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    chars[chars.len().saturating_sub(max_chars)..]
        .iter()
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::{
        MANAGER_MODEL, MANAGER_REASONING_EFFORT, ManagerTurnRequest, command_spec,
        manager_mcp_response, prepare_runtime, validate_manager_command,
    };
    use crate::model::{CodexHome, ManagerKind, ManagerSession};
    use crate::paths::AppPaths;

    fn session() -> ManagerSession {
        ManagerSession {
            id: "manager_01900000000070008000000000000000".to_string(),
            kind: ManagerKind::Lumbergh,
            workspace_id: None,
            workspace_name: None,
            source_codex_home_id: "home_01900000000070008000000000000000".to_string(),
            native_thread_id: None,
            cwd: PathBuf::from("/tmp"),
            model: MANAGER_MODEL.to_string(),
            reasoning_effort: MANAGER_REASONING_EFFORT.to_string(),
            status: "idle".to_string(),
            last_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn runtime_links_auth_and_installs_only_the_manager_skill() {
        let root = tempdir().unwrap();
        let paths = AppPaths::for_test(root.path());
        paths.ensure().unwrap();
        let source = root.path().join("source-home");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("auth.json"), "{}").unwrap();
        fs::set_permissions(source.join("auth.json"), fs::Permissions::from_mode(0o600)).unwrap();
        let home = CodexHome {
            id: "home_01900000000070008000000000000000".to_string(),
            name: "current".to_string(),
            host_id: "host_local".to_string(),
            account_id: "acct_01900000000070008000000000000000".to_string(),
            account_alias: "current".to_string(),
            path: source.clone(),
            codex_binary: PathBuf::from("codex"),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let launcher_home = root.path().join("launcher-home");
        fs::create_dir_all(launcher_home.join(".agents/skills/unrelated")).unwrap();
        fs::write(
            launcher_home.join(".agents/skills/unrelated/SKILL.md"),
            "---\nname: unrelated\ndescription: unrelated\n---\n",
        )
        .unwrap();
        let runtime = prepare_runtime(&paths, &home, &session(), &launcher_home).unwrap();
        assert_eq!(
            fs::read_link(runtime.codex_home.join("auth.json")).unwrap(),
            source.join("auth.json")
        );
        let skills = fs::read_dir(runtime.codex_home.join("skills"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(skills, ["wipsaw-manager"]);
        assert!(!runtime.codex_home.join("plugins").exists());
        assert_eq!(
            runtime.disabled_user_skills,
            [fs::canonicalize(launcher_home.join(".agents/skills/unrelated/SKILL.md")).unwrap()]
        );
        assert!(
            fs::read_to_string(runtime.work_dir.join("AGENTS.md"))
                .unwrap()
                .contains("single top-level manager")
        );
        let mode = fs::metadata(runtime.codex_home.join("auth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn exec_spec_is_resumable_terra_medium_and_ignores_user_config() {
        let request = ManagerTurnRequest {
            session_id: session().id,
            native_thread_id: Some("01900000-0000-7000-8000-000000000000".to_string()),
            codex_binary: PathBuf::from("/opt/codex"),
            runtime: super::ManagerRuntime {
                codex_home: PathBuf::from("/tmp/manager/codex"),
                user_home: PathBuf::from("/tmp/manager/user"),
                launcher_home: PathBuf::from("/home/real-user"),
                work_dir: PathBuf::from("/tmp/manager/work"),
                disabled_user_skills: vec![PathBuf::from("/home/real-user/.agents/skills/git")],
            },
            prompt: "hello".to_string(),
            environment: vec![("WIPSAW_MANAGER_EXECUTABLE".into(), "/opt/wipsaw".into())],
        };
        let spec = command_spec(&request);
        let args = spec
            .args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args[0..2], ["exec", "resume"]);
        assert!(args.iter().any(|argument| argument == "--json"));
        assert!(
            args.iter()
                .any(|argument| argument == "--ignore-user-config")
        );
        assert!(args.iter().any(|argument| argument == MANAGER_MODEL));
        assert!(
            args.iter()
                .any(|argument| argument == "model_reasoning_effort=\"medium\"")
        );
        assert!(
            args.iter()
                .any(|argument| argument == "skills.bundled.enabled=false")
        );
        assert!(
            args.iter()
                .any(|argument| argument == "tools.web_search=false")
        );
        for feature in [
            "features.apps=false",
            "features.tool_search=false",
            "features.tool_suggest=false",
            "features.plugins=false",
            "features.image_generation=false",
            "features.multi_agent=false",
            "features.skill_mcp_dependency_install=false",
            "features.shell_tool=false",
            "features.unified_exec=false",
            "features.code_mode=false",
            "features.js_repl=false",
            "features.apply_patch_freeform=false",
        ] {
            assert!(args.iter().any(|argument| argument == feature));
        }
        assert!(
            args.iter()
                .any(|argument| argument == "sandbox_mode=\"read-only\"")
        );
        assert!(
            args.iter()
                .any(|argument| argument == "mcp_servers.wipsaw.command=\"/opt/wipsaw\"")
        );
        assert!(
            args.iter()
                .any(|argument| argument == "mcp_servers.wipsaw.args=[\"manager-mcp\"]")
        );
        assert!(args.iter().any(|argument| {
            argument.starts_with("skills.config=[")
                && argument.contains("/home/real-user/.agents/skills/git")
                && argument.contains("enabled = false")
        }));
        assert!(
            args.iter()
                .any(|argument| argument
                    == "shell_environment_policy.set.HOME=\"/tmp/manager/user\"")
        );
        assert_eq!(args.last().map(String::as_str), Some("-"));
        assert!(!args.iter().any(|argument| argument == "hello"));
        assert!(spec.environment.iter().any(|(name, value)| {
            name == "HOME" && value == Path::new("/home/real-user").as_os_str()
        }));
    }

    #[test]
    fn manager_command_capability_rejects_plural_groups_and_terminal_attach() {
        assert!(
            validate_manager_command(&["--json", "workspace", "list"].map(str::to_string)).is_ok()
        );
        assert!(
            validate_manager_command(&["--json", "workspaces", "list"].map(str::to_string))
                .is_err()
        );
        assert!(
            validate_manager_command(
                &["--json", "workspace", "start", "demo", "--attach"].map(str::to_string)
            )
            .is_err()
        );
    }

    #[test]
    fn manager_mcp_exposes_only_guide_and_validated_wipsaw_tools() {
        let response = manager_mcp_response(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "tools/list",
            "params": {}
        }))
        .unwrap();
        let names = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["manager_guide", "run_wipsaw"]);
    }
}
