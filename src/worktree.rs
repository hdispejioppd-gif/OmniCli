use std::{
    ffi::{OsStr, OsString},
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    permission::{GitWorktreeOperation, PermissionRequest},
    protocol::{ToolOutput, ToolSpec},
    tools::{Tool, ToolContext, ToolError, ToolRegistry, run_bounded},
};

const METADATA_VERSION: u16 = 1;
const MAX_METADATA_BYTES: u64 = 64 * 1024;

#[derive(Clone)]
pub struct WorktreeManager {
    anchor: PathBuf,
    data_dir: PathBuf,
    max_output_bytes: usize,
    timeout: Duration,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub name: String,
    pub path: PathBuf,
    pub branch_ref: String,
    pub head: Option<String>,
    pub state: WorktreeState,
    pub dirty: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeState {
    Active,
    Incomplete,
    Conflict,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnershipRecord {
    version: u16,
    repo_id: String,
    name: String,
    branch_ref: String,
    created_from_oid: String,
    created_at_ms: i64,
}

struct RepositoryContext {
    repo_id: String,
    root: PathBuf,
    metadata_dir: PathBuf,
    hooks_dir: PathBuf,
}

#[derive(Default)]
struct GitWorktreeEntry {
    path: PathBuf,
    head: Option<String>,
    branch: Option<String>,
    locked: bool,
    prunable: bool,
}

impl WorktreeManager {
    pub fn new(
        anchor: PathBuf,
        data_dir: PathBuf,
        max_output_bytes: usize,
        timeout: Duration,
    ) -> Self {
        Self {
            anchor,
            data_dir,
            max_output_bytes,
            timeout,
        }
    }

    pub async fn create(&self, name: &str, reference: &str) -> Result<WorktreeInfo, WorktreeError> {
        validate_worktree_name(name)?;
        validate_reference(reference)?;
        let repository = self.repository_context().await?;
        self.reject_checkout_drivers().await?;
        let oid = self.resolve_commit(reference).await?;
        let branch_ref = format!("refs/heads/omni/{name}");
        let branch = format!("omni/{name}");
        let target = repository.root.join(name);
        let metadata_path = repository.metadata_dir.join(format!("{name}.json"));
        if target.exists() || metadata_path.exists() {
            return Err(WorktreeError::Exists(name.into()));
        }
        let branch_exists = self
            .run_git(
                &self.anchor,
                [
                    OsString::from("show-ref"),
                    OsString::from("--verify"),
                    OsString::from("--quiet"),
                    OsString::from(&branch_ref),
                ],
                false,
            )
            .await?;
        if branch_exists.exit_code == Some(0) {
            return Err(WorktreeError::BranchExists(branch_ref));
        }
        if branch_exists.exit_code != Some(1) {
            return Err(WorktreeError::Git(branch_exists.stderr_text()));
        }

        tokio::fs::create_dir_all(&repository.metadata_dir).await?;
        tokio::fs::create_dir_all(&repository.hooks_dir).await?;
        let record = OwnershipRecord {
            version: METADATA_VERSION,
            repo_id: repository.repo_id.clone(),
            name: name.into(),
            branch_ref: branch_ref.clone(),
            created_from_oid: oid.clone(),
            created_at_ms: now_ms()?,
        };
        reserve_metadata(metadata_path, serde_json::to_vec(&record)?).await?;

        let output = self
            .run_git(
                &self.anchor,
                [
                    OsString::from("-c"),
                    OsString::from(format!(
                        "core.hooksPath={}",
                        repository.hooks_dir.to_string_lossy()
                    )),
                    OsString::from("-c"),
                    OsString::from("worktree.guessRemote=false"),
                    OsString::from("worktree"),
                    OsString::from("add"),
                    OsString::from("--quiet"),
                    OsString::from("--no-track"),
                    OsString::from("-b"),
                    OsString::from(branch),
                    target.as_os_str().to_owned(),
                    OsString::from(oid),
                ],
                true,
            )
            .await?;
        if output.exit_code != Some(0) {
            return Err(WorktreeError::Git(output.stderr_text()));
        }
        let info = self.inspect(name).await?;
        if info.state != WorktreeState::Active {
            return Err(WorktreeError::Conflict(name.into()));
        }
        Ok(info)
    }

    pub async fn list(&self) -> Result<Vec<WorktreeInfo>, WorktreeError> {
        let repository = self.repository_context().await?;
        if !repository.metadata_dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries = tokio::fs::read_dir(&repository.metadata_dir).await?;
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("json"))
                && let Some(name) = path.file_stem().and_then(OsStr::to_str)
            {
                validate_worktree_name(name)?;
                names.push(name.to_string());
            }
        }
        names.sort();
        let mut result = Vec::with_capacity(names.len());
        for name in names {
            result.push(self.inspect_with_context(&repository, &name).await?);
        }
        Ok(result)
    }

    pub async fn inspect(&self, name: &str) -> Result<WorktreeInfo, WorktreeError> {
        validate_worktree_name(name)?;
        let repository = self.repository_context().await?;
        self.inspect_with_context(&repository, name).await
    }

    pub async fn remove(&self, name: &str) -> Result<WorktreeInfo, WorktreeError> {
        validate_worktree_name(name)?;
        let repository = self.repository_context().await?;
        let info = self.inspect_with_context(&repository, name).await?;
        if info.state != WorktreeState::Active {
            return Err(WorktreeError::Conflict(name.into()));
        }
        if info.dirty != Some(false) {
            return Err(WorktreeError::Dirty(name.into()));
        }
        let output = self
            .run_git(
                &self.anchor,
                [
                    OsString::from("worktree"),
                    OsString::from("remove"),
                    OsString::from("--"),
                    info.path.as_os_str().to_owned(),
                ],
                true,
            )
            .await?;
        if output.exit_code != Some(0) {
            return Err(WorktreeError::Git(output.stderr_text()));
        }
        tokio::fs::remove_file(repository.metadata_dir.join(format!("{name}.json"))).await?;
        Ok(WorktreeInfo {
            state: WorktreeState::Incomplete,
            dirty: None,
            ..info
        })
    }

    async fn inspect_with_context(
        &self,
        repository: &RepositoryContext,
        name: &str,
    ) -> Result<WorktreeInfo, WorktreeError> {
        let record = read_metadata(
            &repository.metadata_dir.join(format!("{name}.json")),
            &repository.repo_id,
            name,
        )
        .await?;
        let target = repository.root.join(name);
        let entries = self.git_worktree_entries().await?;
        let entry = entries
            .into_iter()
            .find(|entry| paths_equal(&entry.path, &target));
        let Some(entry) = entry else {
            return Ok(WorktreeInfo {
                name: name.into(),
                path: target,
                branch_ref: record.branch_ref,
                head: None,
                state: WorktreeState::Incomplete,
                dirty: None,
            });
        };
        let expected_branch = Some(record.branch_ref.as_str());
        if entry.branch.as_deref() != expected_branch || entry.locked || entry.prunable {
            return Ok(WorktreeInfo {
                name: name.into(),
                path: target,
                branch_ref: record.branch_ref,
                head: entry.head,
                state: WorktreeState::Conflict,
                dirty: None,
            });
        }
        let dirty = self.worktree_dirty(&target).await?;
        Ok(WorktreeInfo {
            name: name.into(),
            path: target,
            branch_ref: record.branch_ref,
            head: entry.head,
            state: WorktreeState::Active,
            dirty: Some(dirty),
        })
    }

    async fn repository_context(&self) -> Result<RepositoryContext, WorktreeError> {
        let top = self
            .git_stdout(["rev-parse", "--path-format=absolute", "--show-toplevel"])
            .await?;
        let common = self
            .git_stdout(["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .await?;
        let anchor = std::fs::canonicalize(&self.anchor)?;
        let top = std::fs::canonicalize(top.trim())?;
        if !paths_equal(&anchor, &top) {
            return Err(WorktreeError::NotRoot);
        }
        let common_dir = std::fs::canonicalize(common.trim())?;
        let identity = normalized_identity(&common_dir);
        let repo_id = hex::encode(Sha256::digest(identity.as_bytes()))[..32].to_string();
        let root = self.data_dir.join("worktrees").join("v1").join(&repo_id);
        let absolute_root = absolutize(&root)?;
        if path_within(&absolute_root, &top) || path_within(&absolute_root, &common_dir) {
            return Err(WorktreeError::UnsafeRoot(absolute_root));
        }
        Ok(RepositoryContext {
            repo_id,
            metadata_dir: absolute_root.join(".metadata"),
            hooks_dir: absolute_root.join(".hooks-empty"),
            root: absolute_root,
        })
    }

    async fn resolve_commit(&self, reference: &str) -> Result<String, WorktreeError> {
        if reference.starts_with("refs/") {
            let check = self
                .run_git(
                    &self.anchor,
                    [
                        OsString::from("check-ref-format"),
                        OsString::from(reference),
                    ],
                    false,
                )
                .await?;
            if check.exit_code != Some(0) {
                return Err(WorktreeError::InvalidReference(reference.into()));
            }
        }
        let expression = format!("{reference}^{{commit}}");
        let output = self
            .run_git(
                &self.anchor,
                [
                    OsString::from("rev-parse"),
                    OsString::from("--verify"),
                    OsString::from("--end-of-options"),
                    OsString::from(expression),
                ],
                false,
            )
            .await?;
        if output.exit_code != Some(0) {
            return Err(WorktreeError::UnresolvedReference(reference.into()));
        }
        Ok(output.stdout_text().trim().into())
    }

    async fn reject_checkout_drivers(&self) -> Result<(), WorktreeError> {
        let output = self
            .run_git(
                &self.anchor,
                [
                    OsString::from("config"),
                    OsString::from("--get-regexp"),
                    OsString::from(r"^filter\..*\.(smudge|process)$"),
                ],
                false,
            )
            .await?;
        if output.exit_code == Some(0) && !output.stdout.is_empty() {
            return Err(WorktreeError::CheckoutFilter);
        }
        if !matches!(output.exit_code, Some(0 | 1)) {
            return Err(WorktreeError::Git(output.stderr_text()));
        }
        Ok(())
    }

    async fn git_worktree_entries(&self) -> Result<Vec<GitWorktreeEntry>, WorktreeError> {
        let output = self
            .run_git(
                &self.anchor,
                [
                    OsString::from("worktree"),
                    OsString::from("list"),
                    OsString::from("--porcelain"),
                    OsString::from("-z"),
                ],
                false,
            )
            .await?;
        if output.exit_code != Some(0) {
            return Err(WorktreeError::Git(output.stderr_text()));
        }
        parse_worktree_porcelain(&output.stdout)
    }

    async fn worktree_dirty(&self, path: &Path) -> Result<bool, WorktreeError> {
        let output = self
            .run_git(
                path,
                [
                    OsString::from("status"),
                    OsString::from("--porcelain=v1"),
                    OsString::from("-z"),
                    OsString::from("--untracked-files=all"),
                    OsString::from("--ignored=matching"),
                    OsString::from("--ignore-submodules=none"),
                ],
                false,
            )
            .await?;
        if output.exit_code != Some(0) {
            return Err(WorktreeError::Git(output.stderr_text()));
        }
        Ok(!output.stdout.is_empty())
    }

    async fn git_stdout<const N: usize>(&self, args: [&str; N]) -> Result<String, WorktreeError> {
        let output = self
            .run_git(&self.anchor, args.into_iter().map(OsString::from), false)
            .await?;
        if output.exit_code != Some(0) {
            return Err(WorktreeError::Git(output.stderr_text()));
        }
        Ok(output.stdout_text())
    }

    async fn run_git<I>(
        &self,
        cwd: &Path,
        args: I,
        mutation: bool,
    ) -> Result<crate::tools::CapturedProcess, WorktreeError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let mut command = tokio::process::Command::new("git");
        command
            .current_dir(cwd)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_PREFIX")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_EXTERNAL_DIFF")
            .env_remove("PAGER")
            .stdin(Stdio::null());
        if !mutation {
            command.env("GIT_OPTIONAL_LOCKS", "0");
        } else {
            command.env_remove("GIT_OPTIONAL_LOCKS");
        }
        let output = run_bounded(command, self.max_output_bytes, self.timeout).await?;
        if output.stdout_bytes > output.stdout.len() as u64
            || output.stderr_bytes > output.stderr.len() as u64
        {
            return Err(WorktreeError::OutputLimit);
        }
        Ok(output)
    }
}

pub fn register_worktree_tools(registry: &mut ToolRegistry, manager: Arc<WorktreeManager>) {
    registry
        .register(WorktreeList {
            manager: manager.clone(),
        })
        .expect("unique built-in tool");
    registry
        .register(WorktreeInspect {
            manager: manager.clone(),
        })
        .expect("unique built-in tool");
    registry
        .register(WorktreeCreate {
            manager: manager.clone(),
        })
        .expect("unique built-in tool");
    registry
        .register(WorktreeRemove { manager })
        .expect("unique built-in tool");
}

struct WorktreeList {
    manager: Arc<WorktreeManager>,
}

#[async_trait]
impl Tool for WorktreeList {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_worktree_list".into(),
            description: "List managed isolated Git worktrees".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let _: EmptyArgs = serde_json::from_value(arguments.clone())?;
        Ok(PermissionRequest::GitWorktree {
            operation: GitWorktreeOperation::List,
            name: None,
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        _context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let _: EmptyArgs = serde_json::from_value(arguments)?;
        let worktrees = self.manager.list().await.map_err(worktree_tool_error)?;
        worktree_output(&worktrees)
    }
}

struct WorktreeInspect {
    manager: Arc<WorktreeManager>,
}

struct WorktreeCreate {
    manager: Arc<WorktreeManager>,
}

struct WorktreeRemove {
    manager: Arc<WorktreeManager>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameArgs {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArgs {
    name: String,
    #[serde(default = "default_reference")]
    reference: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

fn default_reference() -> String {
    "HEAD".into()
}

#[async_trait]
impl Tool for WorktreeInspect {
    fn spec(&self) -> ToolSpec {
        named_spec("git_worktree_inspect", "Inspect one managed Git worktree")
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: NameArgs = serde_json::from_value(arguments.clone())?;
        validate_worktree_name(&args.name).map_err(worktree_tool_error)?;
        Ok(PermissionRequest::GitWorktree {
            operation: GitWorktreeOperation::Inspect,
            name: Some(args.name),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        _context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: NameArgs = serde_json::from_value(arguments)?;
        let info = self
            .manager
            .inspect(&args.name)
            .await
            .map_err(worktree_tool_error)?;
        worktree_output(&info)
    }
}

#[async_trait]
impl Tool for WorktreeCreate {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git_worktree_create".into(),
            description: "Create a managed isolated Git worktree and omni/<name> branch".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["name"],
                "properties": {
                    "name": {"type": "string"},
                    "reference": {"type": "string", "default": "HEAD"}
                }
            }),
        }
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: CreateArgs = serde_json::from_value(arguments.clone())?;
        validate_worktree_name(&args.name).map_err(worktree_tool_error)?;
        validate_reference(&args.reference).map_err(worktree_tool_error)?;
        Ok(PermissionRequest::GitWorktree {
            operation: GitWorktreeOperation::Create,
            name: Some(args.name),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        _context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: CreateArgs = serde_json::from_value(arguments)?;
        let info = self
            .manager
            .create(&args.name, &args.reference)
            .await
            .map_err(worktree_tool_error)?;
        worktree_output(&info)
    }
}

#[async_trait]
impl Tool for WorktreeRemove {
    fn spec(&self) -> ToolSpec {
        named_spec(
            "git_worktree_remove",
            "Remove a clean managed Git worktree without force",
        )
    }

    fn permission_request(&self, arguments: &Value) -> Result<PermissionRequest, ToolError> {
        let args: NameArgs = serde_json::from_value(arguments.clone())?;
        validate_worktree_name(&args.name).map_err(worktree_tool_error)?;
        Ok(PermissionRequest::GitWorktree {
            operation: GitWorktreeOperation::Remove,
            name: Some(args.name),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        _context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: NameArgs = serde_json::from_value(arguments)?;
        let info = self
            .manager
            .remove(&args.name)
            .await
            .map_err(worktree_tool_error)?;
        worktree_output(&info)
    }
}

fn named_spec(name: &str, description: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["name"],
            "properties": {"name": {"type": "string"}}
        }),
    }
}

fn worktree_output<T: Serialize>(value: &T) -> Result<ToolOutput, ToolError> {
    let serialized = serde_json::to_string_pretty(value)?;
    Ok(ToolOutput {
        success: true,
        stdout: serialized,
        stderr: String::new(),
        truncated: false,
        metadata: serde_json::to_value(value)?,
    })
}

fn worktree_tool_error(error: WorktreeError) -> ToolError {
    ToolError::Worktree(error.to_string())
}

pub fn validate_worktree_name(name: &str) -> Result<(), WorktreeError> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 48
        || !bytes[0].is_ascii_lowercase()
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        return Err(WorktreeError::InvalidName(name.into()));
    }
    let reserved = matches!(name, "con" | "prn" | "aux" | "nul")
        || (name.len() == 4
            && matches!(&name[..3], "com" | "lpt")
            && matches!(name.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        return Err(WorktreeError::InvalidName(name.into()));
    }
    Ok(())
}

fn validate_reference(reference: &str) -> Result<(), WorktreeError> {
    let full_ref = reference.starts_with("refs/heads/")
        || reference.starts_with("refs/tags/")
        || reference.starts_with("refs/remotes/");
    let full_oid = matches!(reference.len(), 40 | 64)
        && reference.bytes().all(|byte| byte.is_ascii_hexdigit());
    if reference != "HEAD" && !full_ref && !full_oid {
        return Err(WorktreeError::InvalidReference(reference.into()));
    }
    if reference.starts_with('-') || reference.chars().any(char::is_whitespace) {
        return Err(WorktreeError::InvalidReference(reference.into()));
    }
    Ok(())
}

fn parse_worktree_porcelain(bytes: &[u8]) -> Result<Vec<GitWorktreeEntry>, WorktreeError> {
    let mut entries = Vec::new();
    let mut current = GitWorktreeEntry::default();
    let mut has_fields = false;
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            if has_fields {
                entries.push(current);
                current = GitWorktreeEntry::default();
                has_fields = false;
            }
            continue;
        }
        has_fields = true;
        let text = String::from_utf8(field.to_vec()).map_err(|_| WorktreeError::Protocol)?;
        if let Some(path) = text.strip_prefix("worktree ") {
            current.path = PathBuf::from(path);
        } else if let Some(head) = text.strip_prefix("HEAD ") {
            current.head = Some(head.into());
        } else if let Some(branch) = text.strip_prefix("branch ") {
            current.branch = Some(branch.into());
        } else if text == "locked" || text.starts_with("locked ") {
            current.locked = true;
        } else if text == "prunable" || text.starts_with("prunable ") {
            current.prunable = true;
        }
    }
    if has_fields {
        entries.push(current);
    }
    Ok(entries)
}

async fn reserve_metadata(path: PathBuf, bytes: Vec<u8>) -> Result<(), WorktreeError> {
    tokio::task::spawn_blocking(move || {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()
    })
    .await
    .map_err(|error| WorktreeError::Task(error.to_string()))??;
    Ok(())
}

async fn read_metadata(
    path: &Path,
    repo_id: &str,
    name: &str,
) -> Result<OwnershipRecord, WorktreeError> {
    let metadata = tokio::fs::metadata(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            WorktreeError::Unknown(name.into())
        } else {
            WorktreeError::Io(error)
        }
    })?;
    if metadata.len() > MAX_METADATA_BYTES {
        return Err(WorktreeError::Metadata);
    }
    let record: OwnershipRecord = serde_json::from_slice(&tokio::fs::read(path).await?)?;
    if record.version != METADATA_VERSION || record.repo_id != repo_id || record.name != name {
        return Err(WorktreeError::Metadata);
    }
    Ok(record)
}

fn absolutize(path: &Path) -> Result<PathBuf, WorktreeError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn normalized_identity(path: &Path) -> String {
    let value = path.to_string_lossy().replace("\\\\?\\", "");
    if cfg!(windows) {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    normalized_identity(&left) == normalized_identity(&right)
}

fn path_within(candidate: &Path, parent: &Path) -> bool {
    let candidate = normalized_identity(candidate);
    let parent = normalized_identity(parent);
    if candidate == parent {
        return true;
    }
    let separator = if cfg!(windows) { '\\' } else { '/' };
    candidate.starts_with(&format!("{parent}{separator}"))
}

fn now_ms() -> Result<i64, WorktreeError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| WorktreeError::Time(error.to_string()))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| WorktreeError::Time("timestamp overflow".into()))
}

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("invalid worktree name: {0}")]
    InvalidName(String),
    #[error("invalid worktree reference: {0}")]
    InvalidReference(String),
    #[error("worktree reference does not resolve to a commit: {0}")]
    UnresolvedReference(String),
    #[error("worktree already exists or is reserved: {0}")]
    Exists(String),
    #[error("worktree branch already exists: {0}")]
    BranchExists(String),
    #[error("unknown managed worktree: {0}")]
    Unknown(String),
    #[error("managed worktree is dirty: {0}")]
    Dirty(String),
    #[error("managed worktree state conflicts with Git registration: {0}")]
    Conflict(String),
    #[error("workspace must be the Git worktree root")]
    NotRoot,
    #[error("configured worktree root is unsafe: {0}")]
    UnsafeRoot(PathBuf),
    #[error("checkout filters are configured; managed worktree creation is blocked")]
    CheckoutFilter,
    #[error("invalid managed worktree metadata")]
    Metadata,
    #[error("invalid Git worktree porcelain output")]
    Protocol,
    #[error("Git worktree operation failed: {0}")]
    Git(String),
    #[error("Git worktree output exceeded the configured limit")]
    OutputLimit,
    #[error("worktree I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("worktree metadata JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("worktree task failed: {0}")]
    Task(String),
    #[error("worktree clock failed: {0}")]
    Time(String),
    #[error(transparent)]
    Tool(#[from] ToolError),
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("git starts");
        assert!(status.success(), "git command failed: {args:?}");
    }

    fn committed_repository() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        let data = directory.path().join("data");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--initial-branch=main"]);
        std::fs::write(repo.join("tracked.txt"), "initial\n").unwrap();
        git(&repo, &["add", "tracked.txt"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Omni Test",
                "-c",
                "user.email=omni@example.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        );
        (directory, repo, data)
    }

    #[test]
    fn worktree_names_and_refs_are_deliberately_narrow() {
        for valid in ["task", "task-17", "a1"] {
            validate_worktree_name(valid).unwrap();
        }
        for invalid in ["", "Task", "-task", "task_name", "../task", "con", "com1"] {
            assert!(
                validate_worktree_name(invalid).is_err(),
                "accepted {invalid}"
            );
        }
        for valid in [
            "HEAD",
            "refs/heads/main",
            "refs/tags/v1",
            "0123456789012345678901234567890123456789",
        ] {
            validate_reference(valid).unwrap();
        }
        for invalid in ["main", "HEAD~1", "--help", "refs/heads/bad name"] {
            assert!(validate_reference(invalid).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn porcelain_parser_preserves_worktree_records() {
        let input = b"worktree C:/repo\0HEAD aaa\0branch refs/heads/main\0\0worktree C:/task\0HEAD bbb\0branch refs/heads/omni/task\0locked reason\0\0";
        let entries = parse_worktree_porcelain(input).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].branch.as_deref(), Some("refs/heads/omni/task"));
        assert!(entries[1].locked);
    }

    #[test]
    fn invalid_tool_names_fail_before_permission_evaluation() {
        let manager = Arc::new(WorktreeManager::new(
            PathBuf::from("repo"),
            PathBuf::from("data"),
            1024,
            Duration::from_secs(1),
        ));
        let mut registry = ToolRegistry::default();
        register_worktree_tools(&mut registry, manager);
        let tool = registry.get("git_worktree_create").unwrap();
        assert!(
            tool.permission_request(&json!({"name": "../escape", "reference": "HEAD"}))
                .is_err()
        );
    }

    #[tokio::test]
    async fn create_refuses_dirty_removal_and_preserves_branch() {
        let (_directory, repo, data) = committed_repository();
        let manager = WorktreeManager::new(repo.clone(), data, 64 * 1024, Duration::from_secs(15));
        let created = manager.create("task-17", "HEAD").await.unwrap();
        assert_eq!(created.state, WorktreeState::Active);
        assert_eq!(created.branch_ref, "refs/heads/omni/task-17");
        assert!(created.path.is_dir());
        assert!(!created.path.starts_with(&repo));

        std::fs::write(created.path.join("untracked.txt"), "dirty").unwrap();
        assert!(matches!(
            manager.remove("task-17").await,
            Err(WorktreeError::Dirty(_))
        ));
        std::fs::remove_file(created.path.join("untracked.txt")).unwrap();
        let removed = manager.remove("task-17").await.unwrap();
        assert_eq!(removed.state, WorktreeState::Incomplete);
        assert!(!removed.path.exists());

        let status = Command::new("git")
            .args(["show-ref", "--verify", "--quiet", "refs/heads/omni/task-17"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success(), "managed branch should be preserved");
    }

    #[tokio::test]
    async fn worktree_root_inside_repository_is_rejected() {
        let (_directory, repo, _data) = committed_repository();
        let manager = WorktreeManager::new(
            repo.clone(),
            repo.join(".omni"),
            64 * 1024,
            Duration::from_secs(15),
        );
        assert!(matches!(
            manager.list().await,
            Err(WorktreeError::UnsafeRoot(_))
        ));
    }
}
