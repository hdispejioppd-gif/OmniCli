use std::{
    collections::{HashSet, VecDeque},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    Agent, ModelProvider, ModelSpec, Policy, ProviderFactory, RunRequest, ToolRegistry,
    agent::{AgentError, default_tool_context},
    events::{EventError, EventSink, RunEvent},
    permission::PermissionRequest,
    store::{NewSupervisorTask, SqliteStore, StoreError, SupervisorRunRecord},
    worktree::{WorktreeError, WorktreeManager, WorktreeState, validate_worktree_name},
};

const MAX_SUPERVISOR_BYTES: usize = 1024 * 1024;
const MAX_TASKS: usize = 64;
const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_CONCURRENCY: usize = 32;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorDocument {
    pub version: u16,
    pub tasks: Vec<SupervisorTaskDefinition>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorTaskDefinition {
    pub id: String,
    pub worktree: String,
    pub prompt: String,
    pub verify: bool,
    #[serde(default)]
    pub idempotent: bool,
}

#[derive(Clone)]
pub struct SupervisorRuntime {
    root_workspace: PathBuf,
    store: Arc<SqliteStore>,
    worktrees: Arc<WorktreeManager>,
    provider_factory: Arc<ProviderFactory>,
    policy: Policy,
    max_turns: u32,
    max_output_bytes: usize,
    max_file_bytes: usize,
    shell_timeout: Duration,
}

struct PreparedTask {
    definition: SupervisorTaskDefinition,
    workspace: PathBuf,
    session_id: String,
}

pub struct PreparedSupervisor {
    run_id: String,
    tasks: Vec<PreparedTask>,
    provider: Arc<dyn ModelProvider>,
    runtime: SupervisorRuntime,
    concurrency: usize,
    owner: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SupervisorStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupervisorTaskReport {
    pub id: String,
    pub worktree: String,
    pub session_id: String,
    pub status: SupervisorStatus,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupervisorReport {
    pub schema_version: u16,
    pub run_id: String,
    pub status: SupervisorStatus,
    pub tasks: Vec<SupervisorTaskReport>,
}

impl SupervisorReport {
    pub fn succeeded(&self) -> bool {
        self.status == SupervisorStatus::Succeeded
    }
}

impl SupervisorRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root_workspace: PathBuf,
        store: Arc<SqliteStore>,
        worktrees: Arc<WorktreeManager>,
        provider_factory: Arc<ProviderFactory>,
        policy: Policy,
        max_turns: u32,
        max_output_bytes: usize,
        max_file_bytes: usize,
        shell_timeout: Duration,
    ) -> Result<Self, SupervisorError> {
        Ok(Self {
            root_workspace: std::fs::canonicalize(root_workspace)?,
            store,
            worktrees,
            provider_factory,
            policy,
            max_turns,
            max_output_bytes,
            max_file_bytes,
            shell_timeout,
        })
    }

    pub async fn prepare_start(
        &self,
        file: PathBuf,
        concurrency: usize,
        model: ModelSpec,
    ) -> Result<PreparedSupervisor, SupervisorError> {
        if !(1..=MAX_CONCURRENCY).contains(&concurrency) {
            return Err(SupervisorError::InvalidConcurrency(concurrency));
        }
        let document = load_supervisor(&self.root_workspace, &file).await?;
        self.prepare_document(file, document, concurrency, model, None)
            .await
    }

    pub async fn prepare_retry(
        &self,
        run_id: &str,
        concurrency: usize,
        model: ModelSpec,
    ) -> Result<PreparedSupervisor, SupervisorError> {
        self.store.recover_expired_supervisors(now_ms()?)?;
        let previous = self.inspect_run(run_id)?;
        if previous.status == "running" {
            return Err(SupervisorError::RunBusy);
        }
        let file = PathBuf::from(&previous.task_file);
        let mut document = load_supervisor(&self.root_workspace, &file).await?;
        let fingerprint = document_fingerprint(&document)?;
        if fingerprint != previous.fingerprint {
            return Err(SupervisorError::ManifestChanged);
        }
        document.tasks.retain(|task| {
            previous
                .tasks
                .iter()
                .find(|stored| stored.id == task.id)
                .is_some_and(|stored| stored.status != "succeeded")
        });
        if document.tasks.is_empty() {
            return Err(SupervisorError::NothingToRetry);
        }
        for task in &document.tasks {
            let stored = previous
                .tasks
                .iter()
                .find(|stored| stored.id == task.id)
                .ok_or_else(|| SupervisorError::ManifestChanged)?;
            if !task.idempotent || !stored.idempotent {
                return Err(SupervisorError::NotIdempotent(task.id.clone()));
            }
            if hex::encode(Sha256::digest(task.prompt.as_bytes())) != stored.prompt_sha256 {
                return Err(SupervisorError::ManifestChanged);
            }
        }
        self.prepare_document(file, document, concurrency, model, Some(run_id))
            .await
    }

    async fn prepare_document(
        &self,
        file: PathBuf,
        document: SupervisorDocument,
        concurrency: usize,
        model: ModelSpec,
        parent_run_id: Option<&str>,
    ) -> Result<PreparedSupervisor, SupervisorError> {
        validate_document(&document)?;
        if document.tasks.iter().any(|task| task.verify)
            && !self.policy.decide(&PermissionRequest::Validation).allowed
        {
            return Err(SupervisorError::VerificationDenied);
        }
        let provider = self.provider_factory.build(&model)?;
        let fingerprint = document_fingerprint(&document)?;
        let mut worktree_names = HashSet::new();
        let mut workspace_keys = HashSet::new();
        let mut prepared = Vec::with_capacity(document.tasks.len());
        let mut new_tasks = Vec::with_capacity(document.tasks.len());
        for task in document.tasks {
            if !worktree_names.insert(task.worktree.clone()) {
                return Err(SupervisorError::DuplicateWorktree(task.worktree));
            }
            let info = self.worktrees.inspect(&task.worktree).await?;
            if info.state != WorktreeState::Active || info.dirty != Some(false) {
                return Err(SupervisorError::WorktreeNotClean(task.worktree));
            }
            let workspace = std::fs::canonicalize(info.path)?;
            let key = normalized_path(&workspace);
            if !workspace_keys.insert(key.clone()) {
                return Err(SupervisorError::DuplicateWorktree(task.worktree));
            }
            new_tasks.push(NewSupervisorTask {
                id: task.id.clone(),
                worktree: task.worktree.clone(),
                workspace_path: key,
                verify: task.verify,
                prompt_sha256: hex::encode(Sha256::digest(task.prompt.as_bytes())),
                idempotent: task.idempotent,
                title: task.prompt.chars().take(80).collect(),
            });
            prepared.push((task, workspace));
        }
        let owner = uuid::Uuid::now_v7().to_string();
        let (run_id, sessions) = self.store.create_supervisor_run(
            &self.root_workspace,
            &file,
            &model.selector(),
            concurrency,
            &fingerprint,
            parent_run_id,
            &owner,
            now_ms()? + 300_000,
            &new_tasks,
        )?;
        let tasks = prepared
            .into_iter()
            .zip(sessions)
            .map(|((definition, workspace), session_id)| PreparedTask {
                definition,
                workspace,
                session_id,
            })
            .collect();
        Ok(PreparedSupervisor {
            run_id,
            tasks,
            provider,
            runtime: self.clone(),
            concurrency,
            owner,
        })
    }

    pub fn list_runs(&self, limit: u32) -> Result<Vec<SupervisorRunRecord>, SupervisorError> {
        self.store.recover_expired_supervisors(now_ms()?)?;
        Ok(self
            .store
            .list_supervisor_runs(&self.root_workspace, limit)?)
    }

    pub fn inspect_run(&self, id: &str) -> Result<SupervisorRunRecord, SupervisorError> {
        self.store.recover_expired_supervisors(now_ms()?)?;
        self.store
            .load_supervisor_run(id)?
            .ok_or_else(|| SupervisorError::RunNotFound(id.into()))
    }
}

impl PreparedSupervisor {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub async fn execute(
        self,
        cancellation: CancellationToken,
    ) -> Result<SupervisorReport, SupervisorError> {
        let count = self.tasks.len();
        let mut pending = self.tasks.into_iter().enumerate().collect::<VecDeque<_>>();
        let mut running = FuturesUnordered::new();
        let mut reports = vec![None; count];
        let mut cancelled = false;
        let mut heartbeat = tokio::time::interval(Duration::from_secs(60));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            while running.len() < self.concurrency && !cancelled {
                let Some((index, task)) = pending.pop_front() else {
                    break;
                };
                self.runtime.store.set_supervisor_task_status(
                    &self.run_id,
                    &task.definition.id,
                    "running",
                    None,
                )?;
                let runtime = self.runtime.clone();
                let provider = self.provider.clone();
                let token = cancellation.child_token();
                let run_id = self.run_id.clone();
                running.push(async move {
                    let report = run_task(runtime, provider, task, token).await;
                    (index, run_id, report)
                });
            }
            if running.is_empty() {
                break;
            }
            tokio::select! {
                _ = heartbeat.tick(), if !cancelled => {
                    if let Err(error) = self.runtime.store.renew_supervisor_lease(
                        &self.run_id,
                        &self.owner,
                        now_ms()? + 300_000,
                    ) {
                        cancellation.cancel();
                        while running.next().await.is_some() {}
                        return Err(error.into());
                    }
                }
                _ = cancellation.cancelled(), if !cancelled => {
                    cancelled = true;
                    for (index, task) in pending.drain(..) {
                        self.runtime.store.set_supervisor_task_status(
                            &self.run_id,
                            &task.definition.id,
                            "cancelled",
                            None,
                        )?;
                        reports[index] = Some(SupervisorTaskReport {
                            id: task.definition.id,
                            worktree: task.definition.worktree,
                            session_id: task.session_id,
                            status: SupervisorStatus::Cancelled,
                            error: None,
                        });
                    }
                }
                Some((index, run_id, report)) = running.next() => {
                    self.runtime.store.set_supervisor_task_status(
                        &run_id,
                        &report.id,
                        supervisor_status_name(&report.status),
                        report.error.as_deref(),
                    )?;
                    reports[index] = Some(report);
                }
            }
        }
        let tasks = reports
            .into_iter()
            .map(|report| report.expect("all supervisor tasks reached a terminal state"))
            .collect::<Vec<_>>();
        let status = if cancellation.is_cancelled() {
            SupervisorStatus::Cancelled
        } else if tasks
            .iter()
            .all(|task| task.status == SupervisorStatus::Succeeded)
        {
            SupervisorStatus::Succeeded
        } else {
            SupervisorStatus::Failed
        };
        self.runtime
            .store
            .finish_supervisor_run(&self.run_id, supervisor_status_name(&status))?;
        Ok(SupervisorReport {
            schema_version: 1,
            run_id: self.run_id,
            status,
            tasks,
        })
    }
}

async fn run_task(
    runtime: SupervisorRuntime,
    provider: Arc<dyn ModelProvider>,
    task: PreparedTask,
    cancellation: CancellationToken,
) -> SupervisorTaskReport {
    let policy = runtime.policy.for_workspace(task.workspace.clone());
    let agent = Agent::new(
        provider,
        ToolRegistry::standard(),
        policy,
        runtime.store,
        default_tool_context(
            task.workspace,
            runtime.max_output_bytes,
            runtime.max_file_bytes,
            runtime.shell_timeout.as_secs(),
        ),
        runtime.max_turns,
    );
    let result = agent
        .run(
            RunRequest {
                prompt: task.definition.prompt,
                session_id: Some(task.session_id.clone()),
                verify: task.definition.verify,
                system_prompt: None,
            },
            Arc::new(DiscardSink),
            cancellation.clone(),
        )
        .await;
    let (status, error) = match result {
        Ok(_) => (SupervisorStatus::Succeeded, None),
        Err(_) if cancellation.is_cancelled() => (SupervisorStatus::Cancelled, None),
        Err(error) => (SupervisorStatus::Failed, Some(error.to_string())),
    };
    SupervisorTaskReport {
        id: task.definition.id,
        worktree: task.definition.worktree,
        session_id: task.session_id,
        status,
        error,
    }
}

struct DiscardSink;

#[async_trait]
impl EventSink for DiscardSink {
    async fn emit(&self, _event: &RunEvent) -> Result<(), EventError> {
        Ok(())
    }
}

async fn load_supervisor(
    workspace: &Path,
    file: &Path,
) -> Result<SupervisorDocument, SupervisorError> {
    validate_relative(file)?;
    let root = tokio::fs::canonicalize(workspace).await?;
    let path = tokio::fs::canonicalize(root.join(file)).await?;
    if !path.starts_with(&root) {
        return Err(SupervisorError::Path);
    }
    let metadata = tokio::fs::metadata(&path).await?;
    if metadata.len() > MAX_SUPERVISOR_BYTES as u64 {
        return Err(SupervisorError::TooLarge);
    }
    let raw = tokio::fs::read_to_string(path).await?;
    if raw.lines().any(|line| {
        let line = line.split('#').next().unwrap_or("");
        line.contains("<<:")
            || line
                .split_whitespace()
                .any(|token| token.starts_with(['&', '*', '!']))
    }) {
        return Err(SupervisorError::UnsafeYaml);
    }
    Ok(serde_yaml_ng::from_str(&raw)?)
}

fn validate_document(document: &SupervisorDocument) -> Result<(), SupervisorError> {
    if document.version != 1 {
        return Err(SupervisorError::Version(document.version));
    }
    if document.tasks.is_empty() || document.tasks.len() > MAX_TASKS {
        return Err(SupervisorError::TaskCount);
    }
    let mut ids = HashSet::new();
    let mut worktrees = HashSet::new();
    for task in &document.tasks {
        if !valid_id(&task.id) {
            return Err(SupervisorError::InvalidTaskId(task.id.clone()));
        }
        if !ids.insert(task.id.clone()) {
            return Err(SupervisorError::DuplicateTaskId(task.id.clone()));
        }
        validate_worktree_name(&task.worktree)?;
        if !worktrees.insert(task.worktree.clone()) {
            return Err(SupervisorError::DuplicateWorktree(task.worktree.clone()));
        }
        if task.prompt.trim().is_empty()
            || task.prompt.len() > MAX_PROMPT_BYTES
            || task.prompt.contains('\0')
        {
            return Err(SupervisorError::InvalidPrompt(task.id.clone()));
        }
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && id.len() <= 64
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_relative(path: &Path) -> Result<(), SupervisorError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(SupervisorError::Path);
    }
    Ok(())
}

fn normalized_path(path: &Path) -> String {
    let value = path.to_string_lossy().replace("\\\\?\\", "");
    if cfg!(windows) {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn document_fingerprint(document: &SupervisorDocument) -> Result<String, SupervisorError> {
    let mut normalized = document.clone();
    normalized.tasks.sort_by(|a, b| a.id.cmp(&b.id));
    let bytes = serde_json::to_vec(&normalized)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn now_ms() -> Result<i64, StoreError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(StoreError::Clock)?;
    i64::try_from(duration.as_millis()).map_err(|_| StoreError::TimeOverflow)
}

fn supervisor_status_name(status: &SupervisorStatus) -> &'static str {
    match status {
        SupervisorStatus::Succeeded => "succeeded",
        SupervisorStatus::Failed => "failed",
        SupervisorStatus::Cancelled => "cancelled",
    }
}

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error("invalid supervisor concurrency: {0}")]
    InvalidConcurrency(usize),
    #[error("unsupported supervisor version: {0}")]
    Version(u16),
    #[error("supervisor task count must be between 1 and {MAX_TASKS}")]
    TaskCount,
    #[error("invalid supervisor task id: {0}")]
    InvalidTaskId(String),
    #[error("duplicate supervisor task id: {0}")]
    DuplicateTaskId(String),
    #[error("duplicate supervisor worktree: {0}")]
    DuplicateWorktree(String),
    #[error("invalid supervisor prompt for task: {0}")]
    InvalidPrompt(String),
    #[error("supervisor task file path is invalid")]
    Path,
    #[error("supervisor task file is too large")]
    TooLarge,
    #[error("unsafe YAML feature in supervisor task file")]
    UnsafeYaml,
    #[error("supervisor verification requires --verify")]
    VerificationDenied,
    #[error("managed worktree is not active and clean: {0}")]
    WorktreeNotClean(String),
    #[error("supervisor run not found: {0}")]
    RunNotFound(String),
    #[error("supervisor run is still active")]
    RunBusy,
    #[error("supervisor task file changed since the original run")]
    ManifestChanged,
    #[error("supervisor run has no failed tasks to retry")]
    NothingToRetry,
    #[error("supervisor task is not marked idempotent: {0}")]
    NotIdempotent(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Provider(#[from] crate::provider::ProviderError),
    #[error(transparent)]
    Agent(#[from] AgentError),
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo)
                .status()
                .unwrap()
                .success()
        );
    }

    async fn fixture() -> (
        tempfile::TempDir,
        PathBuf,
        Arc<SqliteStore>,
        Arc<WorktreeManager>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        let data = directory.path().join("data");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--initial-branch=main"]);
        std::fs::write(repo.join("tracked.txt"), "payload").unwrap();
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
        let manager = Arc::new(WorktreeManager::new(
            repo.clone(),
            data.clone(),
            64 * 1024,
            Duration::from_secs(15),
        ));
        manager.create("first", "HEAD").await.unwrap();
        manager.create("second", "HEAD").await.unwrap();
        let store = Arc::new(SqliteStore::open(&data.join("state.db")).unwrap());
        (directory, repo, store, manager)
    }

    #[tokio::test]
    async fn supervisor_runs_isolated_tasks_and_keeps_declaration_order() {
        let (_directory, repo, store, manager) = fixture().await;
        std::fs::write(
            repo.join("tasks.yml"),
            "version: 1\ntasks:\n  - id: good\n    worktree: first\n    prompt: read tracked.txt\n    verify: false\n  - id: bad\n    worktree: second\n    prompt: read missing.txt\n    verify: false\n",
        )
        .unwrap();
        let runtime = SupervisorRuntime::new(
            repo,
            store.clone(),
            manager,
            Arc::new(ProviderFactory::new(None)),
            Policy::new(PathBuf::from("unused"), false, false, false),
            4,
            64 * 1024,
            1024 * 1024,
            Duration::from_secs(15),
        )
        .unwrap();
        let report = runtime
            .prepare_start(PathBuf::from("tasks.yml"), 2, ModelSpec::Fake)
            .await
            .unwrap()
            .execute(CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report.status, SupervisorStatus::Failed);
        assert_eq!(report.tasks[0].id, "good");
        assert_eq!(report.tasks[0].status, SupervisorStatus::Succeeded);
        assert_eq!(report.tasks[1].id, "bad");
        assert_eq!(report.tasks[1].status, SupervisorStatus::Failed);
        assert_ne!(report.tasks[0].session_id, report.tasks[1].session_id);
        assert!(
            store
                .load_messages(&report.tasks[0].session_id)
                .unwrap()
                .iter()
                .any(|message| message.content.contains("tracked.txt"))
        );
        let persisted = runtime.inspect_run(&report.run_id).unwrap();
        assert_eq!(persisted.tasks.len(), 2);
    }

    #[tokio::test]
    async fn duplicate_worktrees_are_rejected_before_journal_creation() {
        let (_directory, repo, store, manager) = fixture().await;
        std::fs::write(
            repo.join("tasks.yml"),
            "version: 1\ntasks:\n  - id: one\n    worktree: first\n    prompt: one\n    verify: false\n  - id: two\n    worktree: first\n    prompt: two\n    verify: false\n",
        )
        .unwrap();
        let runtime = SupervisorRuntime::new(
            repo,
            store,
            manager,
            Arc::new(ProviderFactory::new(None)),
            Policy::new(PathBuf::from("unused"), false, false, false),
            2,
            1024,
            1024,
            Duration::from_secs(15),
        )
        .unwrap();
        assert!(matches!(
            runtime
                .prepare_start(PathBuf::from("tasks.yml"), 2, ModelSpec::Fake)
                .await,
            Err(SupervisorError::DuplicateWorktree(_))
        ));
        assert!(runtime.list_runs(10).unwrap().is_empty());
    }
}
