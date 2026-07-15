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

/// A best-effort, read-only preview of the change a write tool would make, shown
/// to the user before the write is authorized. Never affects the actual write.
#[derive(Clone, Debug, serde::Serialize)]
pub struct WritePreview {
    pub path: String,
    pub summary: String,
    pub diff: String,
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

    /// Optional preview of the pending change, rendered before authorization.
    /// Defaults to no preview. Implementations must be read-only and infallible
    /// (return `None` on any error) since this runs purely for display.
    fn preview(&self, _arguments: &Value, _context: &ToolContext) -> Option<WritePreview> {
        None
    }
}

/// Render a compact unified-style diff between two texts, line by line, using a
/// classic LCS. Purely for human display in write previews; never affects the
/// actual write. Bounded so huge files never blow up memory or output.
fn render_unified_diff(path: &str, old: &str, new: &str) -> String {
    const MAX_EMITTED: usize = 400;
    const MAX_LCS_LINES: usize = 2000;
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let n = old_lines.len();
    let m = new_lines.len();
    let header = format!("--- a/{path}\n+++ b/{path}\n");
    if n > MAX_LCS_LINES || m > MAX_LCS_LINES {
        return format!(
            "{header}(large change: {n} old line(s) \u{2192} {m} new line(s); diff preview omitted)\n"
        );
    }
    // lcs[i][j] = length of the longest common subsequence of old[i..] and new[j..].
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old_lines[i] == new_lines[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = header;
    let mut i = 0usize;
    let mut j = 0usize;
    let mut emitted = 0usize;
    let push = |out: &mut String, emitted: &mut usize, prefix: char, line: &str| -> bool {
        if *emitted >= MAX_EMITTED {
            return false;
        }
        out.push(prefix);
        out.push_str(line);
        out.push('\n');
        *emitted += 1;
        true
    };
    while i < n && j < m {
        let ok = if old_lines[i] == new_lines[j] {
            let ok = push(&mut out, &mut emitted, ' ', old_lines[i]);
            i += 1;
            j += 1;
            ok
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            let ok = push(&mut out, &mut emitted, '-', old_lines[i]);
            i += 1;
            ok
        } else {
            let ok = push(&mut out, &mut emitted, '+', new_lines[j]);
            j += 1;
            ok
        };
        if !ok {
            out.push_str("\u{2026} (diff truncated)\n");
            return out;
        }
    }
    while i < n {
        if !push(&mut out, &mut emitted, '-', old_lines[i]) {
            out.push_str("\u{2026} (diff truncated)\n");
            return out;
        }
        i += 1;
    }
    while j < m {
        if !push(&mut out, &mut emitted, '+', new_lines[j]) {
            out.push_str("\u{2026} (diff truncated)\n");
            return out;
        }
        j += 1;
    }
    out
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn standard() -> Self {
        let mut registry = Self::default();
        registry.register(ReadFile).expect("unique built-in tool");
        registry
            .register(ListDirectory)
            .expect("unique built-in tool");
        registry.register(CreateFile).expect("unique built-in tool");
        registry.register(ApplyPatch).expect("unique built-in tool");
        registry.register(MoveFile).expect("unique built-in tool");
        registry.register(DeleteFile).expect("unique built-in tool");
        registry
            .register(CreateDirectory)
            .expect("unique built-in tool");
        registry.register(CopyFile).expect("unique built-in tool");
        registry
            .register(DeleteDirectory)
            .expect("unique built-in tool");
        registry
            .register(ReplaceInFiles)
            .expect("unique built-in tool");
        registry.register(GitStatus).expect("unique built-in tool");
        registry.register(GitDiff).expect("unique built-in tool");
        registry.register(GitCommit).expect("unique built-in tool");
        registry.register(RunChecks).expect("unique built-in tool");
        registry.register(UpdatePlan).expect("unique built-in tool");
        registry.register(Shell).expect("unique built-in tool");
        registry
            .register(RepoMapTool)
            .expect("unique built-in tool");
        registry.register(SearchCode).expect("unique built-in tool");
        registry
            .register(SearchFiles)
            .expect("unique built-in tool");
        registry
            .register(GotoDefinition)
            .expect("unique built-in tool");
        registry.register(Hover).expect("unique built-in tool");
        registry
            .register(FindReferences)
            .expect("unique built-in tool");
        registry
            .register(WorkspaceSymbols)
            .expect("unique built-in tool");
        registry
            .register(RenameSymbol)
            .expect("unique built-in tool");
        registry.register(WebFetch).expect("unique built-in tool");
        registry.register(WebSearch).expect("unique built-in tool");
        registry
            .register(WebDownload)
            .expect("unique built-in tool");
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

struct ListDirectory;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListDirectoryArgs {
    #[serde(default = "default_list_path")]
    path: PathBuf,
}

fn default_list_path() -> PathBuf {
    PathBuf::from(".")
}

#[async_trait]
impl Tool for ListDirectory {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_directory".into(),
            description: "List the entries of a directory inside the workspace. Directories are marked with a trailing slash and files show their size. Defaults to the workspace root."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "path": { "type": "string", "minLength": 1, "description": "Workspace-relative directory (defaults to the workspace root)" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: ListDirectoryArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileRead { path: args.path })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ListDirectoryArgs = serde_json::from_value(arguments)?;
        let directory = existing_directory(&context.workspace, &args.path)?;
        let mut entries: Vec<(bool, String, u64)> = Vec::new();
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if file_type.is_dir() {
                entries.push((true, name, 0));
            } else {
                let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                entries.push((false, name, size));
            }
        }
        // Directories first, then alphabetical — a stable, predictable listing.
        entries.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
        let count = entries.len();
        let stdout = entries
            .iter()
            .map(|(is_dir, name, size)| {
                if *is_dir {
                    format!("{name}/")
                } else {
                    format!("{name} ({size} bytes)")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput {
            success: true,
            stdout,
            stderr: String::new(),
            truncated: false,
            metadata: json!({ "count": count, "path": args.path }),
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

    fn preview(&self, arguments: &Value, _context: &ToolContext) -> Option<WritePreview> {
        let args: CreateFileArgs = serde_json::from_value(arguments.clone()).ok()?;
        let display = args.path.display().to_string();
        let added = args.content.lines().count();
        let diff = render_unified_diff(&display, "", &args.content);
        Some(WritePreview {
            summary: format!("create {display} (+{added} line(s))"),
            path: display,
            diff,
        })
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

    fn preview(&self, arguments: &Value, context: &ToolContext) -> Option<WritePreview> {
        let args: ApplyPatchArgs = serde_json::from_value(arguments.clone()).ok()?;
        if args.old_text.is_empty() {
            return None;
        }
        let original = std::fs::read(context.workspace.join(&args.path)).ok()?;
        if original.len() > context.max_file_bytes {
            return None;
        }
        let text = std::str::from_utf8(&original).ok()?;
        let matches = text.match_indices(&args.old_text).collect::<Vec<_>>();
        if matches.len() != 1 {
            return None;
        }
        let offset = matches[0].0;
        let mut replacement =
            String::with_capacity(text.len() - args.old_text.len() + args.new_text.len());
        replacement.push_str(&text[..offset]);
        replacement.push_str(&args.new_text);
        replacement.push_str(&text[offset + args.old_text.len()..]);
        let display = args.path.display().to_string();
        let diff = render_unified_diff(&display, text, &replacement);
        Some(WritePreview {
            summary: format!("patch {display}"),
            path: display,
            diff,
        })
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

struct GitCommit;

fn default_stage_all() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GitCommitArgs {
    message: String,
    #[serde(default = "default_stage_all")]
    all: bool,
    #[serde(default)]
    paths: Vec<PathBuf>,
}

#[async_trait]
impl Tool for GitCommit {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_commit".into(),
            description: "Stage changes and create a Git commit in the workspace repository. By default all changes are staged (git add -A); pass explicit paths to stage only those, or all=false to commit only what is already staged. Commits are created with --no-gpg-sign. Requires --allow-write."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["message"],
                "properties": {
                    "message": { "type": "string", "minLength": 1, "description": "Commit message" },
                    "all": { "type": "boolean", "description": "Stage every change with git add -A before committing (default true; ignored when paths are given)" },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string", "minLength": 1 },
                        "description": "Specific workspace-relative paths to stage before committing"
                    }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: GitCommitArgs = serde_json::from_value(arguments.clone())?;
        for path in &args.paths {
            validate_relative_path(path)?;
        }
        Ok(PermissionRequest::FileWrite {
            path: default_list_path(),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GitCommitArgs = serde_json::from_value(arguments)?;
        let message = args.message.trim().to_string();
        if message.is_empty() {
            return Err(ToolError::Git("commit message must not be empty".into()));
        }
        for path in &args.paths {
            validate_relative_path(path)?;
        }
        verify_repository_root(context).await?;

        let mut stage_args: Vec<String> = vec!["add".into()];
        if !args.paths.is_empty() {
            stage_args.push("--".into());
            for path in &args.paths {
                stage_args.push(path.to_string_lossy().into_owned());
            }
        } else if args.all {
            stage_args.push("-A".into());
        }
        if stage_args.len() > 1 {
            let mut command = git_command(&context.workspace);
            command.args(&stage_args);
            let staged =
                run_bounded(command, context.max_output_bytes, context.shell_timeout).await?;
            if staged.exit_code != Some(0) {
                return Err(ToolError::Git(staged.stderr_text()));
            }
        }

        let commit_args = [
            "--no-pager",
            "commit",
            "--no-gpg-sign",
            "-m",
            message.as_str(),
        ];
        git_output(
            context,
            &commit_args,
            json!({
                "operation": "commit",
                "staged_all": args.paths.is_empty() && args.all,
                "changed": true,
            }),
        )
        .await
    }
}

struct UpdatePlan;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanStep {
    step: String,
    status: PlanStatus,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdatePlanArgs {
    #[serde(default)]
    explanation: Option<String>,
    steps: Vec<PlanStep>,
}

#[async_trait]
impl Tool for UpdatePlan {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "update_plan".into(),
            description: "Record or update a short, ordered task plan for the current work. Use it to break a non-trivial task into steps and keep their status current. Each step has a status of pending, in_progress, or completed, and at most one step may be in_progress at a time. This is a planning aid with no side effects on the filesystem."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["steps"],
                "properties": {
                    "explanation": { "type": "string", "description": "Optional one-line note about why the plan changed" },
                    "steps": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["step", "status"],
                            "properties": {
                                "step": { "type": "string", "minLength": 1 },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] }
                            }
                        }
                    }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let _: UpdatePlanArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileRead {
            path: default_list_path(),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        _context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: UpdatePlanArgs = serde_json::from_value(arguments)?;
        if args.steps.is_empty() {
            return Err(ToolError::Arguments(serde::de::Error::custom(
                "plan must contain at least one step",
            )));
        }
        let in_progress = args
            .steps
            .iter()
            .filter(|item| item.status == PlanStatus::InProgress)
            .count();
        if in_progress > 1 {
            return Err(ToolError::Arguments(serde::de::Error::custom(
                "at most one step may be in_progress",
            )));
        }
        let completed = args
            .steps
            .iter()
            .filter(|item| item.status == PlanStatus::Completed)
            .count();
        let total = args.steps.len();
        let mut rendered = String::new();
        if let Some(explanation) = args.explanation.as_ref()
            && !explanation.trim().is_empty()
        {
            rendered.push_str(explanation.trim());
            rendered.push_str("\n\n");
        }
        for item in &args.steps {
            let marker = match item.status {
                PlanStatus::Completed => "[x]",
                PlanStatus::InProgress => "[~]",
                PlanStatus::Pending => "[ ]",
            };
            rendered.push_str(marker);
            rendered.push(' ');
            rendered.push_str(&item.step);
            rendered.push('\n');
        }
        rendered.push_str(&format!("\n{completed}/{total} completed"));
        Ok(ToolOutput {
            success: true,
            stdout: rendered,
            stderr: String::new(),
            truncated: false,
            metadata: json!({
                "total": total,
                "completed": completed,
                "in_progress": in_progress,
            }),
        })
    }
}

struct RunChecks;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RunChecksArgs {
    /// Restrict checks to one ecosystem: "rust", "node", or "python".
    #[serde(default)]
    ecosystem: Option<String>,
    /// Test-name filter forwarded to the runner (cargo test <f>, pytest -k <f>, npm run <script> -- <f>).
    #[serde(default)]
    filter: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct CheckDiagnostic {
    ecosystem: &'static str,
    severity: String,
    file: Option<String>,
    line: Option<u64>,
    column: Option<u64>,
    message: String,
}

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
            description: "Detect and run the project's real test/lint suite for Rust, Node, and Python. Returns structured failures[] (file/line/column/message) parsed from the output. Optionally focus a single ecosystem or a single test via filter.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "ecosystem": {
                        "type": "string",
                        "enum": ["rust", "node", "python"],
                        "description": "Restrict checks to one ecosystem."
                    },
                    "filter": {
                        "type": "string",
                        "description": "Test-name filter for focused reruns (cargo test <f>, pytest -k <f>, npm run <script> -- <f>)."
                    }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let _: RunChecksArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::Validation)
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: RunChecksArgs = serde_json::from_value(arguments)?;
        if let Some(ecosystem) = &args.ecosystem
            && !matches!(ecosystem.as_str(), "rust" | "node" | "python")
        {
            return Err(ToolError::Arguments(serde::de::Error::custom(format!(
                "unknown ecosystem '{ecosystem}'; expected rust, node, or python"
            ))));
        }
        let checks = detect_checks(
            &context.workspace,
            args.filter.as_deref(),
            args.ecosystem.as_deref(),
        )?;
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
        let mut all_diagnostics: Vec<CheckDiagnostic> = Vec::new();
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
                    let diagnostics = parse_check_diagnostics(
                        check.ecosystem,
                        &format!("{}\n{}", captured.stdout_text(), captured.stderr_text()),
                    );
                    all_diagnostics.extend(diagnostics.iter().cloned());
                    results.push(json!({
                        "ecosystem": check.ecosystem,
                        "program": check.program,
                        "args": check.args,
                        "exit_code": captured.exit_code,
                        "passed": passed,
                        "diagnostics": serde_json::to_value(&diagnostics)?,
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
                "failure_count": all_diagnostics.len(),
                "failures": serde_json::to_value(&all_diagnostics)?,
            }),
        })
    }
}

fn detect_checks(
    workspace: &Path,
    filter: Option<&str>,
    ecosystem: Option<&str>,
) -> Result<Vec<CheckCommand>, ToolError> {
    let wanted = |eco: &str| ecosystem.is_none_or(|selected| selected == eco);
    let mut checks = Vec::new();
    if wanted("rust") && workspace.join("Cargo.toml").is_file() {
        let mut args = vec!["test".into(), "--all-targets".into()];
        if let Some(filter) = filter {
            args.push(filter.into());
        }
        checks.push(CheckCommand {
            ecosystem: "rust",
            program: "cargo".into(),
            args,
        });
    }

    let package_json = workspace.join("package.json");
    if wanted("node") && package_json.is_file() {
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
            let mut args = vec!["run".into(), script.into()];
            if let Some(filter) = filter {
                args.push("--".into());
                args.push(filter.into());
            }
            checks.push(CheckCommand {
                ecosystem: "node",
                program: manager,
                args,
            });
        }
    }

    if wanted("python") && has_pytest_marker(workspace)? {
        let mut args = vec!["-m".into(), "pytest".into()];
        if let Some(filter) = filter {
            args.push("-k".into());
            args.push(filter.into());
        }
        checks.push(CheckCommand {
            ecosystem: "python",
            program: if cfg!(windows) { "python" } else { "python3" }.into(),
            args,
        });
    }
    Ok(checks)
}

/// Parse compiler/test output into structured diagnostics the agent can act on.
fn parse_check_diagnostics(ecosystem: &'static str, output: &str) -> Vec<CheckDiagnostic> {
    match ecosystem {
        "rust" => parse_rust_diagnostics(output),
        "node" => parse_node_diagnostics(output),
        "python" => parse_pytest_diagnostics(output),
        _ => Vec::new(),
    }
}

fn parse_rust_diagnostics(output: &str) -> Vec<CheckDiagnostic> {
    let header = regex::Regex::new(r"^(error|warning)(?:\[[A-Za-z0-9]+\])?: (.*)$").unwrap();
    let span = regex::Regex::new(r"^\s*-->\s+(.+?):(\d+):(\d+)\s*$").unwrap();
    let mut diagnostics = Vec::new();
    let mut pending: Option<(String, String)> = None;
    for line in output.lines() {
        if let Some(caps) = header.captures(line) {
            pending = Some((caps[1].to_string(), caps[2].trim().to_string()));
        } else if let Some(caps) = span.captures(line)
            && let Some((severity, message)) = pending.take()
        {
            diagnostics.push(CheckDiagnostic {
                ecosystem: "rust",
                severity,
                file: Some(caps[1].to_string()),
                line: caps[2].parse().ok(),
                column: caps[3].parse().ok(),
                message,
            });
        }
    }
    diagnostics
}

fn parse_node_diagnostics(output: &str) -> Vec<CheckDiagnostic> {
    let tsc =
        regex::Regex::new(r"^(.+?)\((\d+),(\d+)\):\s+(error|warning)\s+[A-Za-z]+\d+:\s+(.*)$")
            .unwrap();
    let generic = regex::Regex::new(r"^(.+?):(\d+):(\d+):\s+(error|warning):?\s+(.*)$").unwrap();
    let mut diagnostics = Vec::new();
    for raw in output.lines() {
        let line = raw.trim_end();
        let caps = tsc.captures(line).or_else(|| generic.captures(line));
        if let Some(caps) = caps {
            diagnostics.push(CheckDiagnostic {
                ecosystem: "node",
                severity: caps[4].to_string(),
                file: Some(caps[1].to_string()),
                line: caps[2].parse().ok(),
                column: caps[3].parse().ok(),
                message: caps[5].trim().to_string(),
            });
        }
    }
    diagnostics
}

fn parse_pytest_diagnostics(output: &str) -> Vec<CheckDiagnostic> {
    let failed =
        regex::Regex::new(r"^(?:FAILED|ERROR)\s+([^\s:]+\.py)(?:::\S+)?(?:\s+-\s+(.*))?$").unwrap();
    let mut diagnostics = Vec::new();
    for line in output.lines() {
        if let Some(caps) = failed.captures(line.trim()) {
            diagnostics.push(CheckDiagnostic {
                ecosystem: "python",
                severity: "error".to_string(),
                file: Some(caps[1].to_string()),
                line: None,
                column: None,
                message: caps
                    .get(2)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_else(|| "test failed".to_string()),
            });
        }
    }
    diagnostics
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

fn existing_directory(workspace: &Path, relative: &Path) -> Result<PathBuf, ToolError> {
    validate_relative_path(relative)?;
    let root = dunce::canonicalize(workspace)?;
    let candidate = reject_link_components(&root, relative, true)?;
    let canonical = dunce::canonicalize(&candidate)?;
    if !canonical.starts_with(&root) {
        return Err(ToolError::Path(
            "resolved path escapes the workspace".into(),
        ));
    }
    if !std::fs::metadata(&canonical)?.is_dir() {
        return Err(ToolError::Path("target is not a directory".into()));
    }
    Ok(canonical)
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

fn new_dir_path(workspace: &Path, relative: &Path) -> Result<PathBuf, ToolError> {
    validate_relative_path(relative)?;
    let root = dunce::canonicalize(workspace)?;
    let candidate = reject_link_components(&root, relative, false)?;
    if !candidate.starts_with(&root) {
        return Err(ToolError::Path(
            "resolved path escapes the workspace".into(),
        ));
    }
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LspQueryArgs {
    path: PathBuf,
    line: u32,
    column: u32,
    #[serde(default)]
    server: Option<String>,
}

fn resolve_lsp_server(
    path: &Path,
    server_override: &Option<String>,
) -> Result<(String, Vec<String>), ToolError> {
    if let Some(raw) = server_override {
        let mut parts = raw.split_whitespace().map(|piece| piece.to_string());
        let command = parts
            .next()
            .ok_or_else(|| ToolError::Path("empty language server command".into()))?;
        return Ok((command, parts.collect()));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let (command, args): (&str, &[&str]) = match ext {
        "rs" => ("rust-analyzer", &[]),
        "py" => ("pyright-langserver", &["--stdio"]),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => ("typescript-language-server", &["--stdio"]),
        "go" => ("gopls", &[]),
        other => {
            return Err(ToolError::Path(format!(
                "no default language server for '.{other}'; pass an explicit `server` command"
            )));
        }
    };
    Ok((
        command.to_string(),
        args.iter().map(|arg| arg.to_string()).collect(),
    ))
}

fn lsp_command_line(command: &str, args: &[String]) -> String {
    if args.is_empty() {
        command.to_string()
    } else {
        format!("{command} {}", args.join(" "))
    }
}

fn lsp_position_params(uri: &str, line: u32, column: u32) -> Value {
    json!({
        "textDocument": { "uri": uri },
        "position": {
            "line": line.saturating_sub(1),
            "character": column.saturating_sub(1)
        }
    })
}

async fn start_lsp_for(
    context: &ToolContext,
    path: &Path,
    server_override: &Option<String>,
) -> Result<(crate::lsp::LspClient, PathBuf), ToolError> {
    validate_relative_path(path)?;
    let absolute = context.workspace.join(path);
    let text = tokio::fs::read_to_string(&absolute).await?;
    let (command, args) = resolve_lsp_server(path, server_override)?;
    let client = crate::lsp::LspClient::start(&command, &args, &context.workspace)
        .await
        .map_err(|error| ToolError::Process(format!("language server start failed: {error}")))?;
    client
        .open_document(&absolute, &text, 1)
        .await
        .map_err(|error| ToolError::Process(format!("language server didOpen failed: {error}")))?;
    Ok((client, absolute))
}

fn collect_lsp_locations(value: &Value, out: &mut Vec<Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_lsp_locations(item, out);
            }
        }
        Value::Object(map) => {
            let uri = map
                .get("uri")
                .or_else(|| map.get("targetUri"))
                .and_then(|value| value.as_str());
            let range = map
                .get("range")
                .or_else(|| map.get("targetSelectionRange"))
                .or_else(|| map.get("targetRange"));
            if let (Some(uri), Some(range)) = (uri, range) {
                let line = range["start"]["line"].as_u64().unwrap_or(0) + 1;
                let column = range["start"]["character"].as_u64().unwrap_or(0) + 1;
                out.push(json!({ "uri": uri, "line": line, "column": column }));
            }
        }
        _ => {}
    }
}

fn lsp_hover_text(contents: &Value) -> String {
    match contents {
        Value::String(text) => text.clone(),
        Value::Object(map) => map
            .get("value")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .unwrap_or_default(),
        Value::Array(items) => items
            .iter()
            .map(lsp_hover_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

struct GotoDefinition;

#[async_trait]
impl Tool for GotoDefinition {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "goto_definition".into(),
            description: "Resolve the definition of the symbol at a 1-based line/column in a workspace file using a real Language Server (rust-analyzer, pyright, typescript-language-server, gopls by default; override with `server`). Returns matching definition locations as path:line:column. The server binary must be installed and on PATH; launching it requires --allow-shell."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "line", "column"],
                "properties": {
                    "path": { "type": "string", "minLength": 1, "description": "Workspace-relative file path" },
                    "line": { "type": "integer", "minimum": 1, "description": "1-based line number" },
                    "column": { "type": "integer", "minimum": 1, "description": "1-based column number" },
                    "server": { "type": "string", "description": "Optional explicit language server command line, e.g. 'pyright-langserver --stdio'" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: LspQueryArgs = serde_json::from_value(arguments.clone())?;
        validate_relative_path(&args.path)?;
        let (command, cmd_args) = resolve_lsp_server(&args.path, &args.server)?;
        Ok(PermissionRequest::Shell {
            command: lsp_command_line(&command, &cmd_args),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: LspQueryArgs = serde_json::from_value(arguments)?;
        let (client, absolute) = start_lsp_for(context, &args.path, &args.server).await?;
        let uri = crate::lsp::path_to_uri(&absolute);
        let params = lsp_position_params(&uri, args.line, args.column);
        let response = client
            .request(
                "textDocument/definition",
                params,
                std::time::Duration::from_secs(20),
            )
            .await
            .map_err(|error| {
                ToolError::Process(format!("language server request failed: {error}"))
            });
        client.shutdown().await;
        let response = response?;

        let mut locations = Vec::new();
        collect_lsp_locations(&response["result"], &mut locations);
        if locations.is_empty() {
            return Ok(ToolOutput {
                success: true,
                stdout: "No definition found (the language server may still be indexing).".into(),
                stderr: String::new(),
                truncated: false,
                metadata: json!({ "locations": [] }),
            });
        }
        let mut lines = Vec::new();
        let mut meta = Vec::new();
        for location in &locations {
            let uri = location["uri"].as_str().unwrap_or_default();
            let resolved = crate::lsp::uri_to_path(uri);
            let display = resolved
                .strip_prefix(&context.workspace)
                .unwrap_or(&resolved)
                .to_string_lossy()
                .into_owned();
            let line = location["line"].as_u64().unwrap_or(0);
            let column = location["column"].as_u64().unwrap_or(0);
            lines.push(format!("{display}:{line}:{column}"));
            meta.push(json!({ "path": display, "line": line, "column": column }));
        }
        Ok(ToolOutput {
            success: true,
            stdout: lines.join("\n"),
            stderr: String::new(),
            truncated: false,
            metadata: json!({ "locations": meta }),
        })
    }
}

struct Hover;

#[async_trait]
impl Tool for Hover {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "hover".into(),
            description: "Fetch hover documentation (type signature and docs) for the symbol at a 1-based line/column in a workspace file using a real Language Server. Same server selection and --allow-shell requirement as goto_definition."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "line", "column"],
                "properties": {
                    "path": { "type": "string", "minLength": 1, "description": "Workspace-relative file path" },
                    "line": { "type": "integer", "minimum": 1, "description": "1-based line number" },
                    "column": { "type": "integer", "minimum": 1, "description": "1-based column number" },
                    "server": { "type": "string", "description": "Optional explicit language server command line" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: LspQueryArgs = serde_json::from_value(arguments.clone())?;
        validate_relative_path(&args.path)?;
        let (command, cmd_args) = resolve_lsp_server(&args.path, &args.server)?;
        Ok(PermissionRequest::Shell {
            command: lsp_command_line(&command, &cmd_args),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: LspQueryArgs = serde_json::from_value(arguments)?;
        let (client, absolute) = start_lsp_for(context, &args.path, &args.server).await?;
        let uri = crate::lsp::path_to_uri(&absolute);
        let params = lsp_position_params(&uri, args.line, args.column);
        let response = client
            .request(
                "textDocument/hover",
                params,
                std::time::Duration::from_secs(20),
            )
            .await
            .map_err(|error| {
                ToolError::Process(format!("language server request failed: {error}"))
            });
        client.shutdown().await;
        let response = response?;

        let result = &response["result"];
        if result.is_null() {
            return Ok(ToolOutput {
                success: true,
                stdout: "No hover information available.".into(),
                stderr: String::new(),
                truncated: false,
                metadata: json!({ "found": false }),
            });
        }
        let raw = lsp_hover_text(&result["contents"]);
        let raw = if raw.trim().is_empty() {
            "No hover information available.".to_string()
        } else {
            raw
        };
        let (text, truncated) = truncate_text(&raw, context.max_output_bytes);
        Ok(ToolOutput {
            success: true,
            stdout: text,
            stderr: String::new(),
            truncated,
            metadata: json!({ "found": true }),
        })
    }
}

fn default_true_refs() -> bool {
    true
}

fn default_symbol_limit() -> usize {
    100
}

fn lsp_symbol_kind_name(kind: Option<u64>) -> &'static str {
    match kind {
        Some(1) => "file",
        Some(2) => "module",
        Some(3) => "namespace",
        Some(4) => "package",
        Some(5) => "class",
        Some(6) => "method",
        Some(7) => "property",
        Some(8) => "field",
        Some(9) => "constructor",
        Some(10) => "enum",
        Some(11) => "interface",
        Some(12) => "function",
        Some(13) => "variable",
        Some(14) => "constant",
        Some(23) => "struct",
        Some(24) => "event",
        Some(26) => "type_param",
        _ => "symbol",
    }
}

fn workspace_relative(path: &Path, workspace: &Path) -> Option<PathBuf> {
    if let Ok(rel) = path.strip_prefix(workspace) {
        return Some(rel.to_path_buf());
    }
    let workspace_canon = dunce::canonicalize(workspace).ok()?;
    let path_canon = dunce::canonicalize(path).ok()?;
    path_canon
        .strip_prefix(&workspace_canon)
        .ok()
        .map(|rel| rel.to_path_buf())
}

/// Convert a zero-based LSP position into a byte offset at a char boundary.
/// Character columns are interpreted as UTF-16 code units per the LSP spec.
fn position_to_byte_offset(text: &str, line: u64, character: u64) -> Option<usize> {
    let mut offset = 0usize;
    let mut current_line = 0u64;
    for segment in text.split_inclusive('\n') {
        if current_line == line {
            let mut col = 0u64;
            let mut byte = offset;
            for ch in segment.chars() {
                if ch == '\n' {
                    break;
                }
                if col >= character {
                    return Some(byte);
                }
                col += ch.len_utf16() as u64;
                byte += ch.len_utf8();
            }
            return Some(byte.min(text.len()));
        }
        offset += segment.len();
        current_line += 1;
    }
    if current_line == line {
        return Some(text.len());
    }
    None
}

fn apply_text_edits(text: &str, edits: &[Value]) -> Result<String, ToolError> {
    let mut applied: Vec<(usize, usize, String)> = Vec::new();
    for edit in edits {
        let range = &edit["range"];
        let new_text = edit["newText"].as_str().unwrap_or_default().to_string();
        let start = position_to_byte_offset(
            text,
            range["start"]["line"].as_u64().unwrap_or(0),
            range["start"]["character"].as_u64().unwrap_or(0),
        )
        .ok_or_else(|| ToolError::Path("rename edit start position out of range".into()))?;
        let end = position_to_byte_offset(
            text,
            range["end"]["line"].as_u64().unwrap_or(0),
            range["end"]["character"].as_u64().unwrap_or(0),
        )
        .ok_or_else(|| ToolError::Path("rename edit end position out of range".into()))?;
        if start > end || end > text.len() {
            return Err(ToolError::Path("rename edit range is invalid".into()));
        }
        applied.push((start, end, new_text));
    }
    applied.sort_by(|a, b| b.0.cmp(&a.0));
    let mut result = text.to_string();
    let mut last_start = usize::MAX;
    for (start, end, new_text) in applied {
        if end > last_start {
            return Err(ToolError::Path("overlapping rename edits".into()));
        }
        result.replace_range(start..end, &new_text);
        last_start = start;
    }
    Ok(result)
}

fn collect_workspace_edits(result: &Value) -> Vec<(String, Vec<Value>)> {
    let mut out: Vec<(String, Vec<Value>)> = Vec::new();
    if let Some(changes) = result.get("changes").and_then(|value| value.as_object()) {
        for (uri, edits) in changes {
            if let Some(array) = edits.as_array() {
                out.push((uri.clone(), array.clone()));
            }
        }
    }
    if let Some(document_changes) = result
        .get("documentChanges")
        .and_then(|value| value.as_array())
    {
        for change in document_changes {
            if let (Some(uri), Some(edits)) = (
                change["textDocument"]["uri"].as_str(),
                change["edits"].as_array(),
            ) {
                out.push((uri.to_string(), edits.clone()));
            }
        }
    }
    out
}

struct FindReferences;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindReferencesArgs {
    path: PathBuf,
    line: u32,
    column: u32,
    #[serde(default = "default_true_refs")]
    include_declaration: bool,
    #[serde(default)]
    server: Option<String>,
}

#[async_trait]
impl Tool for FindReferences {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "find_references".into(),
            description: "List every reference to the symbol at a 1-based line/column in a workspace file using a real Language Server. Returns locations as path:line:column. Server selection and --allow-shell requirement match goto_definition."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "line", "column"],
                "properties": {
                    "path": { "type": "string", "minLength": 1, "description": "Workspace-relative file path" },
                    "line": { "type": "integer", "minimum": 1, "description": "1-based line number" },
                    "column": { "type": "integer", "minimum": 1, "description": "1-based column number" },
                    "include_declaration": { "type": "boolean", "description": "Include the declaration itself (default true)" },
                    "server": { "type": "string", "description": "Optional explicit language server command line" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: FindReferencesArgs = serde_json::from_value(arguments.clone())?;
        validate_relative_path(&args.path)?;
        let (command, cmd_args) = resolve_lsp_server(&args.path, &args.server)?;
        Ok(PermissionRequest::Shell {
            command: lsp_command_line(&command, &cmd_args),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: FindReferencesArgs = serde_json::from_value(arguments)?;
        let (client, absolute) = start_lsp_for(context, &args.path, &args.server).await?;
        let uri = crate::lsp::path_to_uri(&absolute);
        let mut params = lsp_position_params(&uri, args.line, args.column);
        params["context"] = json!({ "includeDeclaration": args.include_declaration });
        let response = client
            .request(
                "textDocument/references",
                params,
                std::time::Duration::from_secs(20),
            )
            .await
            .map_err(|error| {
                ToolError::Process(format!("language server request failed: {error}"))
            });
        client.shutdown().await;
        let response = response?;

        let mut locations = Vec::new();
        collect_lsp_locations(&response["result"], &mut locations);
        if locations.is_empty() {
            return Ok(ToolOutput {
                success: true,
                stdout: "No references found (the language server may still be indexing).".into(),
                stderr: String::new(),
                truncated: false,
                metadata: json!({ "references": [] }),
            });
        }
        let mut lines = Vec::new();
        let mut meta = Vec::new();
        for location in &locations {
            let uri = location["uri"].as_str().unwrap_or_default();
            let resolved = crate::lsp::uri_to_path(uri);
            let display = workspace_relative(&resolved, &context.workspace)
                .unwrap_or(resolved)
                .to_string_lossy()
                .into_owned();
            let line = location["line"].as_u64().unwrap_or(0);
            let column = location["column"].as_u64().unwrap_or(0);
            lines.push(format!("{display}:{line}:{column}"));
            meta.push(json!({ "path": display, "line": line, "column": column }));
        }
        Ok(ToolOutput {
            success: true,
            stdout: format!("{} reference(s):\n{}", meta.len(), lines.join("\n")),
            stderr: String::new(),
            truncated: false,
            metadata: json!({ "references": meta }),
        })
    }
}

struct WorkspaceSymbols;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceSymbolsArgs {
    query: String,
    path: PathBuf,
    #[serde(default)]
    server: Option<String>,
    #[serde(default = "default_symbol_limit")]
    limit: usize,
}

#[async_trait]
impl Tool for WorkspaceSymbols {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "workspace_symbols".into(),
            description: "Search project-wide symbols (functions, types, methods, ...) by name using a real Language Server. `path` selects and warms the server (a representative source file); `query` is the fuzzy symbol name. Requires --allow-shell."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query", "path"],
                "properties": {
                    "query": { "type": "string", "description": "Symbol name query (fuzzy)" },
                    "path": { "type": "string", "minLength": 1, "description": "Representative workspace-relative file that selects the language server" },
                    "server": { "type": "string", "description": "Optional explicit language server command line" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Maximum results (default 100)" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: WorkspaceSymbolsArgs = serde_json::from_value(arguments.clone())?;
        validate_relative_path(&args.path)?;
        let (command, cmd_args) = resolve_lsp_server(&args.path, &args.server)?;
        Ok(PermissionRequest::Shell {
            command: lsp_command_line(&command, &cmd_args),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: WorkspaceSymbolsArgs = serde_json::from_value(arguments)?;
        let limit = args.limit.clamp(1, 500);
        let (client, _absolute) = start_lsp_for(context, &args.path, &args.server).await?;
        let response = client
            .request(
                "workspace/symbol",
                json!({ "query": args.query }),
                std::time::Duration::from_secs(20),
            )
            .await
            .map_err(|error| {
                ToolError::Process(format!("language server request failed: {error}"))
            });
        client.shutdown().await;
        let response = response?;

        let items = match response["result"].as_array() {
            Some(items) => items,
            None => {
                return Ok(ToolOutput {
                    success: true,
                    stdout: "No symbols found (the language server may still be indexing).".into(),
                    stderr: String::new(),
                    truncated: false,
                    metadata: json!({ "symbols": [] }),
                });
            }
        };
        let mut lines = Vec::new();
        let mut meta = Vec::new();
        let truncated = items.len() > limit;
        for item in items.iter().take(limit) {
            let name = item["name"].as_str().unwrap_or("?");
            let kind = lsp_symbol_kind_name(item["kind"].as_u64());
            let location = &item["location"];
            let uri = location["uri"].as_str().unwrap_or_default();
            let resolved = crate::lsp::uri_to_path(uri);
            let display = workspace_relative(&resolved, &context.workspace)
                .unwrap_or(resolved)
                .to_string_lossy()
                .into_owned();
            let line = location["range"]["start"]["line"].as_u64();
            match line {
                Some(line) => {
                    lines.push(format!("{kind} {name} \u{2014} {display}:{}", line + 1));
                    meta.push(
                        json!({ "name": name, "kind": kind, "path": display, "line": line + 1 }),
                    );
                }
                None => {
                    lines.push(format!("{kind} {name} \u{2014} {display}"));
                    meta.push(json!({ "name": name, "kind": kind, "path": display }));
                }
            }
        }
        if lines.is_empty() {
            return Ok(ToolOutput {
                success: true,
                stdout: "No symbols found.".into(),
                stderr: String::new(),
                truncated: false,
                metadata: json!({ "symbols": [] }),
            });
        }
        Ok(ToolOutput {
            success: true,
            stdout: lines.join("\n"),
            stderr: String::new(),
            truncated,
            metadata: json!({ "symbols": meta, "limit": limit }),
        })
    }
}

struct RenameSymbol;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameArgs {
    path: PathBuf,
    line: u32,
    column: u32,
    new_name: String,
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    server: Option<String>,
}

#[async_trait]
impl Tool for RenameSymbol {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rename_symbol".into(),
            description: "Rename the symbol at a 1-based line/column across the whole project using a real Language Server, then apply the returned edits to the affected files. Only files inside the workspace are written. Set dry_run=true to preview the affected files without writing. Requires --allow-write."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "line", "column", "new_name"],
                "properties": {
                    "path": { "type": "string", "minLength": 1, "description": "Workspace-relative file path" },
                    "line": { "type": "integer", "minimum": 1, "description": "1-based line number" },
                    "column": { "type": "integer", "minimum": 1, "description": "1-based column number" },
                    "new_name": { "type": "string", "minLength": 1, "description": "New symbol name" },
                    "dry_run": { "type": "boolean", "description": "Preview affected files without writing (default false)" },
                    "server": { "type": "string", "description": "Optional explicit language server command line" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: RenameArgs = serde_json::from_value(arguments.clone())?;
        validate_relative_path(&args.path)?;
        resolve_lsp_server(&args.path, &args.server)?;
        Ok(PermissionRequest::FileWrite {
            path: default_list_path(),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: RenameArgs = serde_json::from_value(arguments)?;
        if args.new_name.trim().is_empty() {
            return Err(ToolError::Path("new_name must not be empty".into()));
        }
        let dry_run = args.dry_run;
        let (client, absolute) = start_lsp_for(context, &args.path, &args.server).await?;
        let uri = crate::lsp::path_to_uri(&absolute);
        let mut params = lsp_position_params(&uri, args.line, args.column);
        params["newName"] = json!(args.new_name);
        let response = client
            .request(
                "textDocument/rename",
                params,
                std::time::Duration::from_secs(30),
            )
            .await
            .map_err(|error| {
                ToolError::Process(format!("language server request failed: {error}"))
            });
        client.shutdown().await;
        let response = response?;

        let result = &response["result"];
        if result.is_null() {
            return Ok(ToolOutput {
                success: true,
                stdout: "Rename produced no changes (the symbol may not be renameable here)."
                    .into(),
                stderr: String::new(),
                truncated: false,
                metadata: json!({ "changed": false, "dry_run": dry_run }),
            });
        }
        let edits = collect_workspace_edits(result);
        let mut changed_files = 0usize;
        let mut total_edits = 0usize;
        let mut skipped_outside = 0usize;
        let mut summary = Vec::new();
        for (uri, file_edits) in &edits {
            let resolved = crate::lsp::uri_to_path(uri);
            let Some(relative) = workspace_relative(&resolved, &context.workspace) else {
                skipped_outside += 1;
                continue;
            };
            let bytes = std::fs::read(&resolved)?;
            let original = String::from_utf8_lossy(&bytes).into_owned();
            let updated = apply_text_edits(&original, file_edits)?;
            let display = relative.to_string_lossy().into_owned();
            summary.push(format!("{display} ({} edit(s))", file_edits.len()));
            total_edits += file_edits.len();
            if updated != original {
                changed_files += 1;
                if !dry_run {
                    let metadata = std::fs::metadata(&resolved)?;
                    atomic_replace(&resolved, updated.as_bytes(), metadata.permissions())?;
                }
            }
        }
        summary.sort();
        let header = if dry_run {
            format!(
                "[dry run] Would rename to '{}' across {changed_files} file(s):",
                args.new_name
            )
        } else {
            format!(
                "Renamed to '{}' across {changed_files} file(s):",
                args.new_name
            )
        };
        let body = if summary.is_empty() {
            "(no in-workspace files affected)".to_string()
        } else {
            summary.join("\n")
        };
        Ok(ToolOutput {
            success: true,
            stdout: format!("{header}\n{body}"),
            stderr: String::new(),
            truncated: false,
            metadata: json!({
                "changed": changed_files > 0 && !dry_run,
                "dry_run": dry_run,
                "changed_files": changed_files,
                "total_edits": total_edits,
                "skipped_outside_workspace": skipped_outside,
            }),
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

struct WebDownload;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebDownloadArgs {
    url: String,
    path: PathBuf,
}

#[async_trait]
impl Tool for WebDownload {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_download".into(),
            description: "Download a file from an HTTP/HTTPS URL and save it to a new path inside the workspace. Fails if the destination already exists or exceeds the file size limit."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["url", "path"],
                "properties": {
                    "url": { "type": "string", "minLength": 1, "description": "HTTP or HTTPS URL to download" },
                    "path": { "type": "string", "minLength": 1, "description": "Workspace-relative destination path for the downloaded file" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        // Downloading writes a file, so require the file-write capability for the
        // destination in addition to network egress.
        let args: WebDownloadArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileWrite { path: args.path })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: WebDownloadArgs = serde_json::from_value(arguments)?;
        let destination = new_file_path(&context.workspace, &args.path)?;
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
                metadata: json!({"url": args.url, "status": status.as_u16(), "changed": false}),
            });
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ToolError::Git(e.to_string()))?;
        if bytes.len() > context.max_file_bytes {
            return Err(ToolError::Limit(format!(
                "download is {} bytes, limit is {}",
                bytes.len(),
                context.max_file_bytes
            )));
        }
        let data = bytes.to_vec();
        let sha256 = sha256_hex(&data);
        let written = data.len();
        tokio::task::spawn_blocking(move || atomic_create(&destination, &data))
            .await
            .map_err(|error| ToolError::Join(error.to_string()))??;
        Ok(ToolOutput {
            success: true,
            stdout: format!("downloaded {} bytes to {}", written, args.path.display()),
            stderr: String::new(),
            truncated: false,
            metadata: json!({
                "url": args.url,
                "status": status.as_u16(),
                "path": args.path,
                "bytes": written,
                "sha256": sha256,
                "changed": true,
            }),
        })
    }
}

struct MoveFile;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveFileArgs {
    from: PathBuf,
    to: PathBuf,
}

#[async_trait]
impl Tool for MoveFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "move_file".into(),
            description: "Move or rename a regular file within the workspace. The source must exist and the destination must not already exist."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["from", "to"],
                "properties": {
                    "from": { "type": "string", "minLength": 1 },
                    "to": { "type": "string", "minLength": 1 }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: MoveFileArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileWrite { path: args.to })
    }

    fn preview(&self, arguments: &Value, _context: &ToolContext) -> Option<WritePreview> {
        let args: MoveFileArgs = serde_json::from_value(arguments.clone()).ok()?;
        let from = args.from.display().to_string();
        let to = args.to.display().to_string();
        Some(WritePreview {
            summary: format!("move {from} \u{2192} {to}"),
            diff: format!("rename\n- {from}\n+ {to}\n"),
            path: to,
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: MoveFileArgs = serde_json::from_value(arguments)?;
        let source = existing_regular_file(&context.workspace, &args.from)?;
        let destination = new_file_path(&context.workspace, &args.to)?;
        let from_display = args.from.clone();
        let to_display = args.to.clone();
        tokio::task::spawn_blocking(move || -> Result<(), ToolError> {
            std::fs::rename(&source, &destination)?;
            Ok(())
        })
        .await
        .map_err(|error| ToolError::Join(error.to_string()))??;
        Ok(ToolOutput {
            success: true,
            stdout: format!(
                "moved {} -> {}",
                from_display.display(),
                to_display.display()
            ),
            stderr: String::new(),
            truncated: false,
            metadata: json!({ "from": from_display, "to": to_display, "changed": true }),
        })
    }
}

struct DeleteFile;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteFileArgs {
    path: PathBuf,
}

#[async_trait]
impl Tool for DeleteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delete_file".into(),
            description: "Delete a regular file inside the workspace. Directories and symbolic links are refused."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "minLength": 1 }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: DeleteFileArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileWrite { path: args.path })
    }

    fn preview(&self, arguments: &Value, context: &ToolContext) -> Option<WritePreview> {
        let args: DeleteFileArgs = serde_json::from_value(arguments.clone()).ok()?;
        let display = args.path.display().to_string();
        let content = std::fs::read(context.workspace.join(&args.path))
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
        let (summary, diff) = match content {
            Some(content) => {
                let count = content.lines().count();
                (
                    format!("delete {display} (-{count} line(s))"),
                    render_unified_diff(&display, &content, ""),
                )
            }
            None => (
                format!("delete {display}"),
                format!("--- a/{display}\n+++ /dev/null\n(binary or unreadable file)\n"),
            ),
        };
        Some(WritePreview {
            summary,
            path: display,
            diff,
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DeleteFileArgs = serde_json::from_value(arguments)?;
        let target = existing_regular_file(&context.workspace, &args.path)?;
        let display = args.path.clone();
        tokio::task::spawn_blocking(move || -> Result<(), ToolError> {
            std::fs::remove_file(&target)?;
            Ok(())
        })
        .await
        .map_err(|error| ToolError::Join(error.to_string()))??;
        Ok(ToolOutput {
            success: true,
            stdout: format!("deleted {}", display.display()),
            stderr: String::new(),
            truncated: false,
            metadata: json!({ "path": display, "changed": true }),
        })
    }
}

/// Read-only recursive walk used to preview delete_directory: lists the files
/// and subdirectories that would be removed, bounded so previews stay small.
/// Never writes anything. Returns None for the workspace root or on error.
fn preview_delete_directory(workspace: &Path, rel: &Path) -> Option<(usize, usize, String)> {
    const MAX_ENTRIES: usize = 100;
    let target = existing_directory(workspace, rel).ok()?;
    let root = dunce::canonicalize(workspace).ok()?;
    if target == root {
        return None;
    }
    let mut stack = vec![target];
    let mut files = 0usize;
    let mut dirs = 0usize;
    let mut listed = 0usize;
    let mut scanned = 0usize;
    let mut diff = String::new();
    'walk: while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            scanned += 1;
            if scanned > 20000 {
                break 'walk;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            let display = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            if file_type.is_dir() {
                dirs += 1;
                if listed < MAX_ENTRIES {
                    diff.push_str(&format!("- {display}/\n"));
                    listed += 1;
                }
                stack.push(path);
            } else {
                files += 1;
                if listed < MAX_ENTRIES {
                    diff.push_str(&format!("- {display}\n"));
                    listed += 1;
                }
            }
        }
    }
    let total = files + dirs;
    if total > listed {
        diff.push_str(&format!(
            "\u{2026} ({} more entr(y/ies) not shown)\n",
            total - listed
        ));
    }
    Some((files, dirs, diff))
}

struct DeleteDirectory;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteDirectoryArgs {
    path: PathBuf,
}

#[async_trait]
impl Tool for DeleteDirectory {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delete_directory".into(),
            description: "Recursively delete a directory and all of its contents inside the workspace. Symbolic links are refused and the workspace root itself cannot be deleted."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "minLength": 1 }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: DeleteDirectoryArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileWrite { path: args.path })
    }

    fn preview(&self, arguments: &Value, context: &ToolContext) -> Option<WritePreview> {
        let args: DeleteDirectoryArgs = serde_json::from_value(arguments.clone()).ok()?;
        let display = args.path.display().to_string();
        let (files, dirs, diff) = preview_delete_directory(&context.workspace, &args.path)?;
        Some(WritePreview {
            summary: format!("delete directory {display} (-{files} file(s), -{dirs} subdir(s))"),
            path: display,
            diff,
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DeleteDirectoryArgs = serde_json::from_value(arguments)?;
        let target = existing_directory(&context.workspace, &args.path)?;
        let root = dunce::canonicalize(&context.workspace)?;
        if target == root {
            return Err(ToolError::Path(
                "refusing to delete the workspace root".into(),
            ));
        }
        let display = args.path.clone();
        tokio::task::spawn_blocking(move || -> Result<(), ToolError> {
            std::fs::remove_dir_all(&target)?;
            Ok(())
        })
        .await
        .map_err(|error| ToolError::Join(error.to_string()))??;
        Ok(ToolOutput {
            success: true,
            stdout: format!("deleted directory {}", display.display()),
            stderr: String::new(),
            truncated: false,
            metadata: json!({ "path": display.to_string_lossy(), "changed": true }),
        })
    }
}

struct CreateDirectory;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateDirectoryArgs {
    path: PathBuf,
}

#[async_trait]
impl Tool for CreateDirectory {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "create_directory".into(),
            description: "Create a new directory inside the workspace, including any missing parent directories. Fails if the path already exists."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": { "type": "string", "minLength": 1, "description": "Workspace-relative directory to create" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: CreateDirectoryArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileWrite { path: args.path })
    }

    fn preview(&self, arguments: &Value, context: &ToolContext) -> Option<WritePreview> {
        let args: CreateDirectoryArgs = serde_json::from_value(arguments.clone()).ok()?;
        let display = args.path.display().to_string();
        // Best-effort, read-only: figure out which path segments don't yet exist.
        let mut current = context.workspace.clone();
        let mut rel = PathBuf::new();
        let mut new_dirs = 0usize;
        let mut diff = String::new();
        for component in args.path.components() {
            current = current.join(component.as_os_str());
            rel = rel.join(component.as_os_str());
            if !current.exists() {
                new_dirs += 1;
                diff.push_str(&format!("+ {}/\n", rel.display()));
            }
        }
        if new_dirs == 0 {
            diff.push_str(&format!("(directory {display} already exists)\n"));
        }
        Some(WritePreview {
            summary: format!("mkdir {display} (+{new_dirs} new dir(s))"),
            path: display,
            diff,
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: CreateDirectoryArgs = serde_json::from_value(arguments)?;
        let target = new_dir_path(&context.workspace, &args.path)?;
        let display = args.path.clone();
        tokio::task::spawn_blocking(move || -> Result<(), ToolError> {
            std::fs::create_dir_all(&target)?;
            Ok(())
        })
        .await
        .map_err(|error| ToolError::Join(error.to_string()))??;
        Ok(ToolOutput {
            success: true,
            stdout: format!("Created directory {}", display.display()),
            stderr: String::new(),
            truncated: false,
            metadata: json!({ "path": display.to_string_lossy(), "changed": true }),
        })
    }
}

struct CopyFile;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CopyFileArgs {
    from: PathBuf,
    to: PathBuf,
}

#[async_trait]
impl Tool for CopyFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "copy_file".into(),
            description: "Copy a regular file to a new path inside the workspace. The source must exist and the destination must not already exist."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["from", "to"],
                "properties": {
                    "from": { "type": "string", "minLength": 1, "description": "Existing workspace-relative source file" },
                    "to": { "type": "string", "minLength": 1, "description": "New workspace-relative destination path" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: CopyFileArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileWrite { path: args.to })
    }

    fn preview(&self, arguments: &Value, context: &ToolContext) -> Option<WritePreview> {
        let args: CopyFileArgs = serde_json::from_value(arguments.clone()).ok()?;
        let from = args.from.display().to_string();
        let to = args.to.display().to_string();
        let content = std::fs::read(context.workspace.join(&args.from))
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
        let (summary, diff) = match content {
            Some(content) => {
                let count = content.lines().count();
                (
                    format!("copy {from} \u{2192} {to} (+{count} line(s))"),
                    render_unified_diff(&to, "", &content),
                )
            }
            None => (
                format!("copy {from} \u{2192} {to}"),
                format!("--- /dev/null\n+++ b/{to}\n(binary or unreadable file)\n"),
            ),
        };
        Some(WritePreview {
            summary,
            path: to,
            diff,
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: CopyFileArgs = serde_json::from_value(arguments)?;
        let source = existing_regular_file(&context.workspace, &args.from)?;
        let destination = new_file_path(&context.workspace, &args.to)?;
        let max_file_bytes = context.max_file_bytes;
        let from_display = args.from.clone();
        let to_display = args.to.clone();
        let bytes = tokio::task::spawn_blocking(move || -> Result<u64, ToolError> {
            let length = std::fs::metadata(&source)?.len();
            if length as usize > max_file_bytes {
                return Err(ToolError::Limit(format!(
                    "source is {length} bytes, above the {max_file_bytes} byte limit"
                )));
            }
            std::fs::copy(&source, &destination)?;
            Ok(length)
        })
        .await
        .map_err(|error| ToolError::Join(error.to_string()))??;
        Ok(ToolOutput {
            success: true,
            stdout: format!(
                "Copied {} -> {} ({bytes} bytes)",
                from_display.display(),
                to_display.display()
            ),
            stderr: String::new(),
            truncated: false,
            metadata: json!({
                "from": from_display.to_string_lossy(),
                "to": to_display.to_string_lossy(),
                "bytes": bytes,
                "changed": true,
            }),
        })
    }
}

/// Read-only walk that mirrors ReplaceInFiles::execute to produce per-file diffs
/// for a preview, without writing anything. Bounded in files scanned and diffs
/// emitted so previews stay small. Returns None when nothing would change.
fn preview_replace_in_files(
    workspace: &Path,
    scope_path: &Path,
    find: &str,
    replace: &str,
    regex: bool,
    max_file_bytes: usize,
) -> Option<(usize, usize, String)> {
    const MAX_PREVIEW_FILES: usize = 20;
    if find.is_empty() {
        return None;
    }
    let scope = existing_directory(workspace, scope_path).ok()?;
    let root = dunce::canonicalize(workspace).ok()?;
    let matcher = if regex {
        Some(regex::Regex::new(find).ok()?)
    } else {
        None
    };
    let mut stack = vec![scope];
    let mut files_changed = 0usize;
    let mut total_replacements = 0usize;
    let mut scanned = 0usize;
    let mut previewed_files = 0usize;
    let mut diff = String::new();
    'walk: while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let raw_name = entry.file_name();
            let name = raw_name.to_string_lossy();
            if file_type.is_dir() {
                if matches!(name.as_ref(), ".git" | "target" | "node_modules" | ".omni")
                    || name.starts_with('.')
                {
                    continue;
                }
                stack.push(entry.path());
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            scanned += 1;
            if scanned > 20000 {
                break 'walk;
            }
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.len() as usize > max_file_bytes {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(content) = String::from_utf8(bytes) else {
                continue;
            };
            let (occurrences, updated) = if let Some(re) = matcher.as_ref() {
                let count = re.find_iter(&content).count();
                if count == 0 {
                    continue;
                }
                (count, re.replace_all(&content, replace).into_owned())
            } else {
                let count = content.matches(find).count();
                if count == 0 {
                    continue;
                }
                (count, content.replace(find, replace))
            };
            files_changed += 1;
            total_replacements += occurrences;
            if previewed_files < MAX_PREVIEW_FILES {
                let display = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                diff.push_str(&render_unified_diff(&display, &content, &updated));
                previewed_files += 1;
            }
        }
    }
    if files_changed == 0 {
        return None;
    }
    if files_changed > previewed_files {
        diff.push_str(&format!(
            "\u{2026} ({} more changed file(s) not shown)\n",
            files_changed - previewed_files
        ));
    }
    Some((files_changed, total_replacements, diff))
}

struct ReplaceInFiles;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceInFilesArgs {
    find: String,
    replace: String,
    #[serde(default = "default_list_path")]
    path: PathBuf,
    #[serde(default)]
    regex: bool,
    #[serde(default)]
    dry_run: bool,
}

#[async_trait]
impl Tool for ReplaceInFiles {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "replace_in_files".into(),
            description: "Find and replace text across every UTF-8 text file under a workspace directory (defaults to the whole workspace). By default find is matched literally; set regex=true to match a regular expression, in which case replace may reference capture groups with $1 or ${name}. Skips .git, target, node_modules, .omni, hidden entries, symlinks, and files above the size limit. Reports how many files changed and how many occurrences were replaced."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["find", "replace"],
                "properties": {
                    "find": { "type": "string", "minLength": 1 },
                    "replace": { "type": "string" },
                    "path": { "type": "string", "minLength": 1, "description": "Workspace-relative directory to scope the replacement (defaults to the workspace root)" },
                    "regex": { "type": "boolean", "description": "Treat find as a regular expression; replace may use $1 / ${name} capture references (default false)" },
                    "dry_run": { "type": "boolean", "description": "Preview matches without writing any files (default false)" }
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: ReplaceInFilesArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::FileWrite { path: args.path })
    }

    fn preview(&self, arguments: &Value, context: &ToolContext) -> Option<WritePreview> {
        let args: ReplaceInFilesArgs = serde_json::from_value(arguments.clone()).ok()?;
        let (files_changed, total_replacements, diff) = preview_replace_in_files(
            &context.workspace,
            &args.path,
            &args.find,
            &args.replace,
            args.regex,
            context.max_file_bytes,
        )?;
        let scope = args.path.display().to_string();
        Some(WritePreview {
            summary: format!(
                "replace_in_files: {total_replacements} occurrence(s) in {files_changed} file(s) under {scope}"
            ),
            path: scope,
            diff,
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReplaceInFilesArgs = serde_json::from_value(arguments)?;
        if args.find.is_empty() {
            return Err(ToolError::Path("find must not be empty".into()));
        }
        let scope = existing_directory(&context.workspace, &args.path)?;
        let root = dunce::canonicalize(&context.workspace)?;
        let max_file_bytes = context.max_file_bytes;
        let matcher = if args.regex {
            Some(
                regex::Regex::new(&args.find)
                    .map_err(|error| ToolError::Path(format!("invalid regex: {error}")))?,
            )
        } else {
            None
        };
        let find = args.find.clone();
        let replace = args.replace.clone();
        let dry_run = args.dry_run;
        let scope_display = args.path.clone();
        let (files_changed, total_replacements, changed) = tokio::task::spawn_blocking(
            move || -> Result<(usize, usize, Vec<String>), ToolError> {
                let mut stack = vec![scope];
                let mut files_changed = 0usize;
                let mut total_replacements = 0usize;
                let mut changed: Vec<String> = Vec::new();
                let mut scanned = 0usize;
                'walk: while let Some(dir) = stack.pop() {
                    for entry in std::fs::read_dir(&dir)? {
                        let entry = entry?;
                        let file_type = entry.file_type()?;
                        if file_type.is_symlink() {
                            continue;
                        }
                        let raw_name = entry.file_name();
                        let name = raw_name.to_string_lossy();
                        if file_type.is_dir() {
                            if matches!(name.as_ref(), ".git" | "target" | "node_modules" | ".omni")
                                || name.starts_with('.')
                            {
                                continue;
                            }
                            stack.push(entry.path());
                            continue;
                        }
                        if !file_type.is_file() {
                            continue;
                        }
                        scanned += 1;
                        if scanned > 20000 {
                            break 'walk;
                        }
                        let path = entry.path();
                        let metadata = entry.metadata()?;
                        if metadata.len() as usize > max_file_bytes {
                            continue;
                        }
                        let bytes = std::fs::read(&path)?;
                        let Ok(content) = String::from_utf8(bytes) else {
                            continue;
                        };
                        let (occurrences, updated) = if let Some(re) = matcher.as_ref() {
                            let count = re.find_iter(&content).count();
                            if count == 0 {
                                continue;
                            }
                            (
                                count,
                                re.replace_all(&content, replace.as_str()).into_owned(),
                            )
                        } else {
                            let count = content.matches(find.as_str()).count();
                            if count == 0 {
                                continue;
                            }
                            (count, content.replace(find.as_str(), replace.as_str()))
                        };
                        if !dry_run {
                            atomic_replace(&path, updated.as_bytes(), metadata.permissions())?;
                        }
                        files_changed += 1;
                        total_replacements += occurrences;
                        let display = path
                            .strip_prefix(&root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .into_owned();
                        changed.push(display);
                    }
                }
                changed.sort();
                Ok((files_changed, total_replacements, changed))
            },
        )
        .await
        .map_err(|error| ToolError::Join(error.to_string()))??;
        let stdout = if files_changed == 0 {
            format!(
                "No occurrences of the search string under {}",
                scope_display.display()
            )
        } else if dry_run {
            format!(
                "[dry run] Would replace {total_replacements} occurrence(s) across {files_changed} file(s):\n{}",
                changed.join("\n")
            )
        } else {
            format!(
                "Replaced {total_replacements} occurrence(s) across {files_changed} file(s):\n{}",
                changed.join("\n")
            )
        };
        Ok(ToolOutput {
            success: true,
            stdout,
            stderr: String::new(),
            truncated: false,
            metadata: json!({
                "files_changed": files_changed,
                "replacements": total_replacements,
                "dry_run": dry_run,
                "changed": files_changed > 0 && !dry_run,
            }),
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

        let checks = detect_checks(directory.path(), None, None).unwrap();
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
        assert!(
            detect_checks(directory.path(), None, None)
                .unwrap()
                .is_empty()
        );
    }
}
