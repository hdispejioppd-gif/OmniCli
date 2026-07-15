use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::OpenOptions,
    io::Write,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use uuid::Uuid;

use crate::{
    context::ContextEngine,
    permission::{GitReadOperation, PermissionRequest},
    protocol::{ToolOutput, ToolSpec},
};

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub max_output_bytes: usize,
    pub max_file_bytes: usize,
    pub shell_timeout: Duration,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError>;
    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn standard() -> Self {
        let mut registry = Self::default();
        registry.register(ReadFile).expect("unique built-in tool");
        registry.register(CreateFile).expect("unique built-in tool");
        registry.register(ApplyPatch).expect("unique built-in tool");
        registry.register(GitStatus).expect("unique built-in tool");
        registry.register(GitDiff).expect("unique built-in tool");
        registry.register(RunChecks).expect("unique built-in tool");
        registry.register(Shell).expect("unique built-in tool");
        registry
            .register(RepoMapTool)
            .expect("unique built-in tool");
        registry.register(SearchCode).expect("unique built-in tool");
        registry
            .register(SearchFiles)
            .expect("unique built-in tool");
        registry.register(WebFetch).expect("unique built-in tool");
        registry.register(WebSearch).expect("unique built-in tool");
        registry
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) -> Result<(), ToolError> {
        let name = tool.spec().name;
        if self.tools.insert(name.clone(), Arc::new(tool)).is_some() {
            return Err(ToolError::Duplicate(name));
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = self
            .tools
            .values()
            .map(|tool| tool.spec())
            .collect::<Vec<_>>();
        specs.sort_by(|left, right| left.name.cmp(&right.name));
        specs
    }
}

struct ReadFile;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: PathBuf,
}

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read and hash a UTF-8 regular file inside the workspace".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": { "path": { "type": "string", "minLength": 1 } }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: PathArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileRead { path: args.path })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: PathArgs = serde_json::from_value(arguments)?;
        let path = existing_regular_file(&context.workspace, &args.path)?;
        let data = read_file_limited(&path, context.max_file_bytes).await?;
        let sha256 = sha256_hex(&data);
        let text = std::str::from_utf8(&data)
            .map_err(|_| ToolError::Path("target is not valid UTF-8".into()))?;
        let (stdout, truncated) = truncate_text(text, context.max_output_bytes);
        let returned_bytes = stdout.len();
        Ok(ToolOutput {
            success: true,
            stdout,
            stderr: String::new(),
            truncated,
            metadata: json!({
                "bytes": data.len(),
                "returned_bytes": returned_bytes,
                "sha256": sha256,
            }),
        })
    }
}

struct CreateFile;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateFileArgs {
    path: PathBuf,
    content: String,
}

#[async_trait]
impl Tool for CreateFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "create_file".into(),
            description: "Atomically create a new UTF-8 file without overwriting an existing path"
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "content"],
                "properties": {
                    "path": { "type": "string", "minLength": 1 },
                    "content": { "type": "string" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: CreateFileArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileWrite { path: args.path })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: CreateFileArgs = serde_json::from_value(arguments)?;
        if args.content.len() > context.max_file_bytes {
            return Err(ToolError::Limit("new file exceeds max_file_bytes".into()));
        }
        let path = new_file_path(&context.workspace, &args.path)?;
        let content = args.content.into_bytes();
        tokio::task::spawn_blocking(move || atomic_create(&path, &content))
            .await
            .map_err(|error| ToolError::Join(error.to_string()))??;
        Ok(ToolOutput {
            success: true,
            stdout: format!("created {}", args.path.display()),
            stderr: String::new(),
            truncated: false,
            metadata: json!({
                "bytes": content_len(&context.workspace, &args.path)?,
                "sha256": sha256_file(&context.workspace.join(&args.path))?,
                "changed": true,
            }),
        })
    }
}

struct ApplyPatch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPatchArgs {
    path: PathBuf,
    expected_sha256: String,
    old_text: String,
    new_text: String,
}

#[async_trait]
impl Tool for ApplyPatch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "apply_patch".into(),
            description:
                "Atomically replace one exact text occurrence after verifying the file SHA-256"
                    .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "expected_sha256", "old_text", "new_text"],
                "properties": {
                    "path": { "type": "string", "minLength": 1 },
                    "expected_sha256": { "type": "string", "pattern": "^[0-9a-fA-F]{64}$" },
                    "old_text": { "type": "string", "minLength": 1 },
                    "new_text": { "type": "string" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: ApplyPatchArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileWrite { path: args.path })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ApplyPatchArgs = serde_json::from_value(arguments)?;
        if args.old_text.is_empty() {
            return Err(ToolError::Patch("old_text must not be empty".into()));
        }
        if hex::decode(&args.expected_sha256).map_or(true, |bytes| bytes.len() != 32) {
            return Err(ToolError::Patch(
                "expected_sha256 must contain exactly 64 hexadecimal characters".into(),
            ));
        }
        let path = existing_regular_file(&context.workspace, &args.path)?;
        let display_path = args.path.clone();
        let max_file_bytes = context.max_file_bytes;
        tokio::task::spawn_blocking(move || {
            apply_exact_patch(path, display_path, args, max_file_bytes)
        })
        .await
        .map_err(|error| ToolError::Join(error.to_string()))?
    }
}

fn apply_exact_patch(
    path: PathBuf,
    display_path: PathBuf,
    args: ApplyPatchArgs,
    max_file_bytes: usize,
) -> Result<ToolOutput, ToolError> {
    let original = std::fs::read(&path)?;
    if original.len() > max_file_bytes {
        return Err(ToolError::Limit("target exceeds max_file_bytes".into()));
    }
    let actual_before = sha256_hex(&original);
    let expected = args.expected_sha256.to_ascii_lowercase();
    if actual_before != expected {
        return Ok(patch_rejected(
            "hash_mismatch",
            &display_path,
            json!({"expected_sha256": expected, "actual_sha256": actual_before}),
        ));
    }
    let text = std::str::from_utf8(&original)
        .map_err(|_| ToolError::Patch("target is not valid UTF-8".into()))?;
    let matches = text.match_indices(&args.old_text).collect::<Vec<_>>();
    if matches.is_empty() {
        return Ok(patch_rejected(
            "old_text_not_found",
            &display_path,
            json!({}),
        ));
    }
    if matches.len() > 1 {
        return Ok(patch_rejected(
            "old_text_ambiguous",
            &display_path,
            json!({"matches": matches.len()}),
        ));
    }
    let offset = matches[0].0;
    let mut replacement =
        String::with_capacity(text.len() - args.old_text.len() + args.new_text.len());
    replacement.push_str(&text[..offset]);
    replacement.push_str(&args.new_text);
    replacement.push_str(&text[offset + args.old_text.len()..]);
    if replacement.len() > max_file_bytes {
        return Err(ToolError::Limit(
            "patched file exceeds max_file_bytes".into(),
        ));
    }

    let current = std::fs::read(&path)?;
    let current_hash = sha256_hex(&current);
    if current_hash != actual_before {
        return Ok(patch_rejected(
            "concurrent_modification",
            &display_path,
            json!({"expected_sha256": actual_before, "actual_sha256": current_hash}),
        ));
    }
    let permissions = std::fs::metadata(&path)?.permissions();
    atomic_replace(&path, replacement.as_bytes(), permissions)?;
    let sha256_after = sha256_hex(replacement.as_bytes());
    Ok(ToolOutput {
        success: true,
        stdout: format!("patched {}", display_path.display()),
        stderr: String::new(),
        truncated: false,
        metadata: json!({
            "changed": true,
            "path": display_path,
            "sha256_before": actual_before,
            "sha256_after": sha256_after,
            "bytes_before": original.len(),
            "bytes_after": replacement.len(),
            "offset": offset,
        }),
    })
}

fn patch_rejected(code: &str, path: &Path, extra: Value) -> ToolOutput {
    let mut metadata = json!({"code": code, "path": path, "changed": false});
    if let (Some(target), Some(extra)) = (metadata.as_object_mut(), extra.as_object()) {
        target.extend(extra.clone());
    }
    ToolOutput {
        success: false,
        stdout: String::new(),
        stderr: code.into(),
        truncated: false,
        metadata,
    }
}

struct GitStatus;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[async_trait]
impl Tool for GitStatus {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_status".into(),
            description: "Read porcelain-v2 status for the workspace Git repository".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let _: EmptyArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::GitRead {
            operation: GitReadOperation::Status,
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let _: EmptyArgs = serde_json::from_value(arguments)?;
        verify_repository_root(context).await?;
        let args = [
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.quotepath=true",
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
            "--ignore-submodules=dirty",
        ];
        git_output(context, &args, json!({"operation": "status"})).await
    }
}

struct GitDiff;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GitDiffArgs {
    #[serde(default)]
    staged: bool,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default = "default_diff_context")]
    context: u8,
}

fn default_diff_context() -> u8 {
    3
}

#[async_trait]
impl Tool for GitDiff {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_diff".into(),
            description: "Read a safe built-in Git diff without external diff drivers".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "staged": {"type": "boolean", "default": false},
                    "path": {"type": "string"},
                    "context": {"type": "integer", "minimum": 0, "maximum": 20, "default": 3}
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: GitDiffArgs = serde_json::from_value(arguments.clone())?;
        if let Some(path) = args.path {
            validate_relative_path(&path)?;
        }
        Ok(PermissionRequest::GitRead {
            operation: GitReadOperation::Diff,
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GitDiffArgs = serde_json::from_value(arguments)?;
        if args.context > 20 {
            return Err(ToolError::Git(
                "diff context must be between 0 and 20".into(),
            ));
        }
        verify_repository_root(context).await?;
        let mut command = vec![
            "--no-pager".to_string(),
            "--literal-pathspecs".to_string(),
            "diff".to_string(),
            "--no-ext-diff".to_string(),
            "--no-textconv".to_string(),
            "--no-color".to_string(),
            format!("--unified={}", args.context),
        ];
        if args.staged {
            command.push("--cached".into());
        }
        command.push("--".into());
        if let Some(path) = &args.path {
            validate_relative_path(path)?;
            command.push(path.to_string_lossy().into_owned());
        }
        let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
        git_output(
            context,
            &refs,
            json!({
                "operation": "diff",
                "staged": args.staged,
                "path": args.path,
                "context": args.context,
            }),
        )
        .await
    }
}

struct RunChecks;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CheckCommand {
    ecosystem: &'static str,
    program: String,
    args: Vec<String>,
}

#[async_trait]
impl Tool for RunChecks {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_checks".into(),
            description: "Detect and run fixed project checks for Rust, Node, and Python".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let _: EmptyArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::Validation)
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let _: EmptyArgs = serde_json::from_value(arguments)?;
        let checks = detect_checks(&context.workspace)?;
        if checks.is_empty() {
            return Ok(ToolOutput {
                success: false,
                stdout: String::new(),
                stderr: "no supported project checks detected".into(),
                truncated: false,
                metadata: json!({"code": "no_checks_detected", "checks": []}),
            });
        }

        let per_check_limit = (context.max_output_bytes / checks.len()).max(1024);
        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut results = Vec::new();
        let mut success = true;
        let mut truncated = false;
        for check in checks {
            let mut command = tokio::process::Command::new(&check.program);
            command
                .args(&check.args)
                .current_dir(&context.workspace)
                .env("CI", "1")
                .env("NO_COLOR", "1")
                .env("CARGO_TERM_COLOR", "never");
            let rendered = format!("{} {}", check.program, check.args.join(" "));
            match run_bounded(command, per_check_limit, context.shell_timeout).await {
                Ok(captured) => {
                    let passed = captured.exit_code == Some(0);
                    success &= passed;
                    truncated |= captured.stdout_bytes > captured.stdout.len() as u64
                        || captured.stderr_bytes > captured.stderr.len() as u64;
                    if !captured.stdout.is_empty() {
                        stdout.push_str(&format!(
                            "[{}: {rendered}]\n{}\n",
                            check.ecosystem,
                            captured.stdout_text()
                        ));
                    }
                    if !captured.stderr.is_empty() {
                        stderr.push_str(&format!(
                            "[{}: {rendered}]\n{}\n",
                            check.ecosystem,
                            captured.stderr_text()
                        ));
                    }
                    results.push(json!({
                        "ecosystem": check.ecosystem,
                        "program": check.program,
                        "args": check.args,
                        "exit_code": captured.exit_code,
                        "passed": passed,
                    }));
                }
                Err(error) => {
                    success = false;
                    stderr.push_str(&format!("[{}: {rendered}]\n{error}\n", check.ecosystem));
                    results.push(json!({
                        "ecosystem": check.ecosystem,
                        "program": check.program,
                        "args": check.args,
                        "exit_code": null,
                        "passed": false,
                        "error": error.to_string(),
                    }));
                }
            }
        }

        Ok(ToolOutput {
            success,
            stdout,
            stderr,
            truncated,
            metadata: json!({
                "code": if success { "checks_passed" } else { "checks_failed" },
                "checks": results,
            }),
        })
    }
}

fn detect_checks(workspace: &Path) -> Result<Vec<CheckCommand>, ToolError> {
    let mut checks = Vec::new();
    if workspace.join("Cargo.toml").is_file() {
        checks.push(CheckCommand {
            ecosystem: "rust",
            program: "cargo".into(),
            args: vec!["test".into(), "--all-targets".into()],
        });
    }

    let package_json = workspace.join("package.json");
    if package_json.is_file() {
        let raw = std::fs::read_to_string(&package_json)?;
        let package: Value = serde_json::from_str(&raw)?;
        let scripts = package.get("scripts").and_then(Value::as_object);
        let script = scripts.and_then(|scripts| {
            ["check", "test"]
                .into_iter()
                .find(|name| scripts.get(*name).is_some_and(Value::is_string))
        });
        if let Some(script) = script {
            let manager = detect_node_manager(workspace, &package);
            checks.push(CheckCommand {
                ecosystem: "node",
                program: manager,
                args: vec!["run".into(), script.into()],
            });
        }
    }

    if has_pytest_marker(workspace)? {
        checks.push(CheckCommand {
            ecosystem: "python",
            program: if cfg!(windows) { "python" } else { "python3" }.into(),
            args: vec!["-m".into(), "pytest".into()],
        });
    }
    Ok(checks)
}

fn detect_node_manager(workspace: &Path, package: &Value) -> String {
    if let Some(manager) = package
        .get("packageManager")
        .and_then(Value::as_str)
        .and_then(|value| value.split('@').next())
        .filter(|manager| matches!(*manager, "npm" | "pnpm" | "yarn" | "bun"))
    {
        return manager.into();
    }
    for (marker, manager) in [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lock", "bun"),
        ("bun.lockb", "bun"),
        ("package-lock.json", "npm"),
        ("npm-shrinkwrap.json", "npm"),
    ] {
        if workspace.join(marker).exists() {
            return manager.into();
        }
    }
    "npm".into()
}

fn has_pytest_marker(workspace: &Path) -> Result<bool, ToolError> {
    if workspace.join("pytest.ini").is_file() {
        return Ok(true);
    }
    for name in ["pyproject.toml", "setup.cfg", "tox.ini"] {
        let path = workspace.join(name);
        if path.is_file() {
            let raw = std::fs::read_to_string(path)?;
            if raw.contains("[tool.pytest") || raw.contains("[pytest]") {
                return Ok(true);
            }
        }
    }
    if workspace.join("tests").is_dir()
        && [
            "setup.py",
            "setup.cfg",
            "requirements.txt",
            "requirements-dev.txt",
        ]
        .into_iter()
        .any(|name| workspace.join(name).is_file())
    {
        return Ok(true);
    }
    Ok(false)
}

struct Shell;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellArgs {
    command: String,
}

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "shell".into(),
            description: "Execute a shell command in the workspace".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["command"],
                "properties": { "command": { "type": "string" } }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: ShellArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::Shell {
            command: args.command,
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ShellArgs = serde_json::from_value(arguments)?;
        let mut command = if cfg!(windows) {
            let mut cmd = tokio::process::Command::new("cmd");
            cmd.args(["/D", "/S", "/C", &args.command]);
            cmd
        } else {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.args(["-c", &args.command]);
            cmd
        };
        command.current_dir(&context.workspace);
        let captured =
            run_bounded(command, context.max_output_bytes, context.shell_timeout).await?;
        let exit_code = captured.exit_code;
        Ok(captured.into_tool_output(json!({"exit_code": exit_code})))
    }
}

async fn verify_repository_root(context: &ToolContext) -> Result<(), ToolError> {
    let mut command = git_command(&context.workspace);
    command.args(["rev-parse", "--show-toplevel"]);
    let captured = run_bounded(command, 16 * 1024, context.shell_timeout).await?;
    if captured.exit_code != Some(0) {
        return Err(ToolError::Git(captured.stderr_text()));
    }
    let reported = PathBuf::from(captured.stdout_text().trim());
    let workspace = dunce::canonicalize(&context.workspace)?;
    let reported = dunce::canonicalize(reported)?;
    if workspace != reported {
        return Err(ToolError::Git(
            "workspace must be the Git repository root".into(),
        ));
    }
    Ok(())
}

async fn git_output(
    context: &ToolContext,
    args: &[&str],
    metadata: Value,
) -> Result<ToolOutput, ToolError> {
    let mut command = git_command(&context.workspace);
    command.args(args);
    let captured = run_bounded(command, context.max_output_bytes, context.shell_timeout).await?;
    let mut output = captured.into_tool_output(metadata);
    output.success = output.metadata["exit_code"] == 0;
    Ok(output)
}

fn git_command(workspace: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command
        .current_dir(workspace)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("PAGER");
    command
}

pub(crate) struct CapturedProcess {
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stdout_bytes: u64,
    pub(crate) stderr_bytes: u64,
}

impl CapturedProcess {
    pub(crate) fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub(crate) fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    fn into_tool_output(self, mut metadata: Value) -> ToolOutput {
        let returned_bytes = self.stdout.len() + self.stderr.len();
        let total_bytes = self.stdout_bytes + self.stderr_bytes;
        if let Some(object) = metadata.as_object_mut() {
            object.extend(
                json!({
                    "exit_code": self.exit_code,
                    "stdout_bytes": self.stdout_bytes,
                    "stderr_bytes": self.stderr_bytes,
                    "returned_bytes": returned_bytes,
                    "omitted_bytes": total_bytes.saturating_sub(returned_bytes as u64),
                })
                .as_object()
                .expect("metadata object")
                .clone(),
            );
        }
        ToolOutput {
            success: self.exit_code == Some(0),
            stdout: String::from_utf8_lossy(&self.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&self.stderr).into_owned(),
            truncated: total_bytes > returned_bytes as u64,
            metadata,
        }
    }
}

pub(crate) async fn run_bounded(
    mut command: tokio::process::Command,
    limit: usize,
    timeout: Duration,
) -> Result<CapturedProcess, ToolError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::Process("missing stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::Process("missing stderr".into()))?;
    let stderr_limit = (limit / 4).min(8 * 1024);
    let stdout_limit = limit.saturating_sub(stderr_limit);
    let stdout_task = tokio::spawn(read_bounded(stdout, stdout_limit));
    let stderr_task = tokio::spawn(read_bounded(stderr, stderr_limit));
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(ToolError::Timeout(timeout));
        }
    };
    let (stdout, stdout_bytes) = stdout_task
        .await
        .map_err(|error| ToolError::Join(error.to_string()))??;
    let (stderr, stderr_bytes) = stderr_task
        .await
        .map_err(|error| ToolError::Join(error.to_string()))??;
    Ok(CapturedProcess {
        exit_code: status.code(),
        stdout,
        stderr,
        stdout_bytes,
        stderr_bytes,
    })
}

async fn read_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> Result<(Vec<u8>, u64), std::io::Error> {
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut total = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        total += count as u64;
        let keep = count.min(limit.saturating_sub(retained.len()));
        retained.extend_from_slice(&buffer[..keep]);
    }
    Ok((retained, total))
}

async fn read_file_limited(path: &Path, limit: usize) -> Result<Vec<u8>, ToolError> {
    let metadata = tokio::fs::metadata(path).await?;
    if metadata.len() > limit as u64 {
        return Err(ToolError::Limit(format!(
            "file is {} bytes, limit is {limit}",
            metadata.len()
        )));
    }
    Ok(tokio::fs::read(path).await?)
}

fn validate_relative_path(path: &Path) -> Result<(), ToolError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(ToolError::Path(
            "path must be non-empty, relative, and remain inside the workspace".into(),
        ));
    }
    Ok(())
}

fn existing_regular_file(workspace: &Path, relative: &Path) -> Result<PathBuf, ToolError> {
    validate_relative_path(relative)?;
    let root = dunce::canonicalize(workspace)?;
    let candidate = reject_link_components(&root, relative, true)?;
    let canonical = dunce::canonicalize(&candidate)?;
    if !canonical.starts_with(&root) {
        return Err(ToolError::Path(
            "resolved path escapes the workspace".into(),
        ));
    }
    if !std::fs::metadata(&canonical)?.is_file() {
        return Err(ToolError::Path("target is not a regular file".into()));
    }
    Ok(canonical)
}

fn new_file_path(workspace: &Path, relative: &Path) -> Result<PathBuf, ToolError> {
    validate_relative_path(relative)?;
    let root = dunce::canonicalize(workspace)?;
    let parent_relative = relative.parent().unwrap_or_else(|| Path::new(""));
    let parent = reject_link_components(&root, parent_relative, true)?;
    let parent = dunce::canonicalize(parent)?;
    if !parent.starts_with(&root) || !parent.is_dir() {
        return Err(ToolError::Path("parent is outside the workspace".into()));
    }
    let name = relative
        .file_name()
        .ok_or_else(|| ToolError::Path("missing file name".into()))?;
    let candidate = parent.join(name);
    if candidate.exists() {
        return Err(ToolError::AlreadyExists(relative.to_path_buf()));
    }
    Ok(candidate)
}

fn reject_link_components(
    root: &Path,
    relative: &Path,
    require_existing: bool,
) -> Result<PathBuf, ToolError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ToolError::Path("symbolic links are not accepted".into()));
            }
            Ok(_) => {}
            Err(error) if !require_existing && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(current)
}

fn atomic_create(path: &Path, content: &[u8]) -> Result<(), ToolError> {
    let temp = temporary_sibling(path)?;
    let result = (|| {
        write_synced(&temp, content, None)?;
        std::fs::hard_link(&temp, path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                ToolError::AlreadyExists(path.to_path_buf())
            } else {
                ToolError::Io(error)
            }
        })?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temp);
    result
}

fn atomic_replace(
    path: &Path,
    content: &[u8],
    permissions: std::fs::Permissions,
) -> Result<(), ToolError> {
    let temp = temporary_sibling(path)?;
    let result = (|| {
        write_synced(&temp, content, Some(permissions))?;
        replace_file(path, &temp)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn temporary_sibling(path: &Path) -> Result<PathBuf, ToolError> {
    let parent = path
        .parent()
        .ok_or_else(|| ToolError::Path("missing parent".into()))?;
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or("file");
    Ok(parent.join(format!(".{name}.omni-{}.tmp", Uuid::now_v7())))
}

fn write_synced(
    path: &Path,
    content: &[u8],
    permissions: Option<std::fs::Permissions>,
) -> Result<(), ToolError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(content)?;
    if let Some(permissions) = permissions {
        file.set_permissions(permissions)?;
    }
    file.sync_all()?;
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(destination: &Path, replacement: &Path) -> Result<(), ToolError> {
    std::fs::rename(replacement, destination)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(destination: &Path, replacement: &Path) -> Result<(), ToolError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{REPLACEFILE_IGNORE_MERGE_ERRORS, ReplaceFileW};

    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let replacement = replacement
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let success = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn sha256_file(path: &Path) -> Result<String, ToolError> {
    Ok(sha256_hex(&std::fs::read(path)?))
}

fn content_len(workspace: &Path, relative: &Path) -> Result<u64, ToolError> {
    Ok(std::fs::metadata(workspace.join(relative))?.len())
}

fn truncate_text(text: &str, limit: usize) -> (String, bool) {
    let truncated = text.len() > limit;
    let mut end = text.len().min(limit);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), truncated)
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("duplicate tool: {0}")]
    Duplicate(String),
    #[error("invalid tool arguments: {0}")]
    Arguments(#[from] serde_json::Error),
    #[error("tool I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("path rejected: {0}")]
    Path(String),
    #[error("path already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("patch rejected: {0}")]
    Patch(String),
    #[error("git operation failed: {0}")]
    Git(String),
    #[error("process failed: {0}")]
    Process(String),
    #[error("operation exceeded limit: {0}")]
    Limit(String),
    #[error("background task failed: {0}")]
    Join(String),
    #[error("tool timed out after {0:?}")]
    Timeout(Duration),
    #[error("MCP operation failed: {0}")]
    Mcp(String),
    #[error("worktree operation failed: {0}")]
    Worktree(String),
    #[error("plugin operation failed: {0}")]
    Plugin(String),
}

struct RepoMapTool;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoMapArgs {
    #[serde(default = "default_repo_map_bytes")]
    max_bytes: usize,
}

fn default_repo_map_bytes() -> usize {
    16384
}

#[async_trait]
impl Tool for RepoMapTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "repo_map".into(),
            description: "Return a compact outline of all definitions (functions, structs, classes) in the repository. Call this FIRST when you need to understand project structure.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "max_bytes": { "type": "integer", "minimum": 1024, "maximum": 262144 }
                }
            }),
        }
    }

    fn permission_request(&self, _arguments: &Value) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest::FileRead {
            path: PathBuf::from("."),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: RepoMapArgs = serde_json::from_value(arguments)?;
        let map = crate::repomap::build_repo_map(&context.workspace, context.max_file_bytes as u64)
            .map_err(|e| ToolError::Path(e.to_string()))?;
        let rendered = crate::repomap::render_map(&map, args.max_bytes);
        let count = map.len();
        Ok(ToolOutput {
            success: true,
            stdout: rendered,
            stderr: String::new(),
            truncated: false,
            metadata: json!({ "files_with_definitions": count }),
        })
    }
}

struct SearchCode;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchCodeArgs {
    query: String,
    #[serde(default = "default_search_code_limit")]
    limit: usize,
}

fn default_search_code_limit() -> usize {
    10
}

#[async_trait]
impl Tool for SearchCode {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_code".into(),
            description: "Full-text search across the codebase using BM25 ranking. Returns file paths with matching line numbers and snippets. Use for keyword and exact symbol search.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "minLength": 1 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                }
            }),
        }
    }

    fn permission_request(&self, _arguments: &Value) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest::FileRead {
            path: PathBuf::from("."),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SearchCodeArgs = serde_json::from_value(arguments)?;
        let index_dir = context.workspace.join(".omni").join("search-index");
        let idx = crate::search::CodeIndex::open(&index_dir)
            .map_err(|e| ToolError::Path(e.to_string()))?;
        if !index_dir.join("meta.json").exists() {
            let ws = context.workspace.clone();
            let max_bytes = context.max_file_bytes as u64;
            idx.reindex(&ws, max_bytes)
                .map_err(|e| ToolError::Path(e.to_string()))?;
        }
        let hits = idx
            .search(&args.query, args.limit)
            .map_err(|e| ToolError::Path(e.to_string()))?;
        if hits.is_empty() {
            Ok(ToolOutput {
                success: true,
                stdout: format!("No results for: {}", args.query),
                stderr: String::new(),
                truncated: false,
                metadata: json!({ "count": 0 }),
            })
        } else {
            let count = hits.len();
            let stdout = hits
                .into_iter()
                .map(|h| format!("{} ({:.2})\n  {}\n", h.path.display(), h.score, h.snippet))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(ToolOutput {
                success: true,
                stdout,
                stderr: String::new(),
                truncated: false,
                metadata: json!({ "count": count }),
            })
        }
    }
}

struct SearchFiles;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchFilesArgs {
    query: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    10
}

#[async_trait]
impl Tool for SearchFiles {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_files".into(),
            description: "Search the workspace for files relevant to a natural-language query."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "minLength": 1 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                }
            }),
        }
    }

    fn permission_request(&self, _arguments: &Value) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest::FileRead {
            path: PathBuf::from("."),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SearchFilesArgs = serde_json::from_value(arguments)?;
        let engine = ContextEngine::index(context.workspace.clone())
            .map_err(|error| ToolError::Path(error.to_string()))?;
        let results = engine.query(&args.query, args.limit);
        let stdout = serde_json::to_string_pretty(&results)?;
        Ok(ToolOutput {
            success: true,
            stdout,
            stderr: String::new(),
            truncated: false,
            metadata: json!({
                "count": results.len(),
                "total_files": engine.profile.total_files,
            }),
        })
    }
}

struct WebFetch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebFetchArgs {
    url: String,
    #[serde(default = "default_max_web_bytes")]
    max_bytes: usize,
}

fn default_max_web_bytes() -> usize {
    256 * 1024
}

#[async_trait]
impl Tool for WebFetch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".into(),
            description:
                "Fetch content from a URL (HTTP/HTTPS). Returns text with HTML tags stripped."
                    .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["url"],
                "properties": {
                    "url": { "type": "string", "minLength": 1, "description": "HTTP or HTTPS URL" },
                    "max_bytes": { "type": "integer", "default": 262144, "description": "Maximum bytes to return" }
                }
            }),
        }
    }

    fn permission_request(&self, _arguments: &Value) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest::Shell {
            command: "web_fetch".into(),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: WebFetchArgs = serde_json::from_value(arguments)?;
        let max = args.max_bytes.min(context.max_output_bytes.max(262144));
        let client = reqwest::Client::builder()
            .timeout(context.shell_timeout)
            .build()
            .map_err(|e| ToolError::Git(e.to_string()))?;
        let resp = client
            .get(&args.url)
            .header("User-Agent", "omni-cli/0.1")
            .send()
            .await
            .map_err(|e| ToolError::Git(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Ok(ToolOutput {
                success: false,
                stdout: String::new(),
                stderr: format!("HTTP {status}"),
                truncated: false,
                metadata: json!({"url": args.url, "status": status.as_u16()}),
            });
        }
        let body = resp
            .text()
            .await
            .map_err(|e| ToolError::Git(e.to_string()))?;
        let content = strip_html(&body);
        let (text, truncated) = if content.len() > max {
            (content[..max].to_string(), true)
        } else {
            (content, false)
        };
        Ok(ToolOutput {
            success: true,
            stdout: text,
            stderr: String::new(),
            truncated,
            metadata: json!({"url": args.url, "status": status.as_u16()}),
        })
    }
}

fn strip_html(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

struct WebSearch;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebSearchArgs {
    query: String,
    #[serde(default = "default_search_results")]
    max_results: usize,
}

fn default_search_results() -> usize {
    10
}

#[async_trait]
impl Tool for WebSearch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_search".into(),
            description: "Search the web via DuckDuckGo. Returns titles and URLs of results."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "minLength": 1 },
                    "max_results": { "type": "integer", "default": 10 }
                }
            }),
        }
    }

    fn permission_request(&self, _arguments: &Value) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest::Shell {
            command: "web_search".into(),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: WebSearchArgs = serde_json::from_value(arguments)?;
        let client = reqwest::Client::builder()
            .timeout(context.shell_timeout)
            .build()
            .map_err(|e| ToolError::Git(e.to_string()))?;
        let resp = client
            .get("https://html.duckduckgo.com/html/")
            .query(&[("q", args.query.as_str())])
            .header("User-Agent", "omni-cli/0.1")
            .send()
            .await
            .map_err(|e| ToolError::Git(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Ok(ToolOutput {
                success: false,
                stdout: String::new(),
                stderr: format!("HTTP {status}"),
                truncated: false,
                metadata: json!({"query": args.query, "status": status.as_u16()}),
            });
        }
        let body = resp
            .text()
            .await
            .map_err(|e| ToolError::Git(e.to_string()))?;
        let results = parse_ddg_results(&body, args.max_results);
        let output = results
            .iter()
            .enumerate()
            .map(|(i, (title, url))| format!("{}. {title}\n   {url}", i + 1))
            .collect::<Vec<_>>()
            .join("\n\n");
        Ok(ToolOutput {
            success: true,
            stdout: output,
            stderr: String::new(),
            truncated: false,
            metadata: json!({"query": args.query, "count": results.len()}),
        })
    }
}

fn parse_ddg_results(html: &str, max: usize) -> Vec<(String, String)> {
    let mut results = Vec::new();
    let mut rest = html;
    while results.len() < max {
        let Some(start) = rest.find("class=\"result__a\"") else {
            break;
        };
        rest = &rest[start..];
        let href_start = rest.find("href=\"").map(|p| p + 6);
        let href_end = href_start.and_then(|s| rest[s..].find('"').map(|e| s + e));
        let title_start = rest.find('>').map(|p| p + 1);
        let title_end = title_start.and_then(|s| rest[s..].find("</a>").map(|e| s + e));
        if let (Some(hs), Some(he), Some(ts), Some(te)) =
            (href_start, href_end, title_start, title_end)
        {
            let url = strip_html(&rest[hs..he]);
            let title = strip_html(&rest[ts..te]);
            if !title.is_empty() {
                results.push((title, url));
            }
            rest = &rest[te..];
        } else {
            break;
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_patch_applies_and_returns_new_hash() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let before = sha256_file(&path).unwrap();

        let output = apply_exact_patch(
            path.clone(),
            PathBuf::from("sample.txt"),
            ApplyPatchArgs {
                path: PathBuf::from("sample.txt"),
                expected_sha256: before.clone(),
                old_text: "beta".into(),
                new_text: "gamma".into(),
            },
            1024,
        )
        .unwrap();

        assert!(output.success);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\ngamma\n");
        assert_eq!(output.metadata["sha256_before"], before);
        assert_eq!(output.metadata["sha256_after"], sha256_file(&path).unwrap());
    }

    #[test]
    fn patch_conflicts_never_modify_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.txt");
        std::fs::write(&path, "same same").unwrap();
        let original = std::fs::read(&path).unwrap();

        for (expected, old_text, code) in [
            ("0".repeat(64), "same".to_string(), "hash_mismatch"),
            (
                sha256_hex(&original),
                "same".to_string(),
                "old_text_ambiguous",
            ),
            (
                sha256_hex(&original),
                "missing".to_string(),
                "old_text_not_found",
            ),
        ] {
            let output = apply_exact_patch(
                path.clone(),
                PathBuf::from("sample.txt"),
                ApplyPatchArgs {
                    path: PathBuf::from("sample.txt"),
                    expected_sha256: expected,
                    old_text,
                    new_text: "replacement".into(),
                },
                1024,
            )
            .unwrap();
            assert!(!output.success);
            assert_eq!(output.metadata["code"], code);
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
    }

    #[test]
    fn atomic_create_refuses_to_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("new.txt");
        atomic_create(&path, b"first").unwrap();
        let error = atomic_create(&path, b"second").unwrap_err();
        assert!(matches!(error, ToolError::AlreadyExists(_)));
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
    }

    #[test]
    fn relative_path_validation_rejects_empty_and_traversal() {
        assert!(validate_relative_path(Path::new("")).is_err());
        assert!(validate_relative_path(Path::new("../outside")).is_err());
        assert!(validate_relative_path(Path::new("src/lib.rs")).is_ok());
    }

    #[test]
    fn strip_html_removes_tags_and_entities() {
        let html = "<p>Hello <b>world</b> &amp; goodbye</p>";
        assert_eq!(strip_html(html), "Hello world & goodbye");
    }

    #[test]
    fn parse_ddg_results_extracts_titles_and_urls() {
        let html = r#"
        <div>
            <a class="result__a" href="https://example.com/foo">Foo Bar</a>
        </div>
        <div>
            <a class="result__a" href="https://example.com/baz">Baz Qux</a>
        </div>
        "#;
        let results = parse_ddg_results(html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "Foo Bar");
        assert_eq!(results[1].0, "Baz Qux");
    }

    #[test]
    fn check_detection_is_deterministic_across_ecosystems() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("Cargo.toml"), "[package]\nname='x'").unwrap();
        std::fs::write(
            directory.path().join("package.json"),
            r#"{"packageManager":"pnpm@10.0.0","scripts":{"test":"ignored","check":"real"}}"#,
        )
        .unwrap();
        std::fs::create_dir(directory.path().join("tests")).unwrap();
        std::fs::write(directory.path().join("pytest.ini"), "[pytest]\n").unwrap();

        let checks = detect_checks(directory.path()).unwrap();
        assert_eq!(
            checks
                .iter()
                .map(|check| check.ecosystem)
                .collect::<Vec<_>>(),
            ["rust", "node", "python"]
        );
        assert_eq!(checks[1].program, "pnpm");
        assert_eq!(checks[1].args, ["run", "check"]);
    }

    #[test]
    fn empty_workspace_has_no_implicit_checks() {
        let directory = tempfile::tempdir().unwrap();
        assert!(detect_checks(directory.path()).unwrap().is_empty());
    }
}
