use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cap_std::{ambient_authority, fs::Dir};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    permission::Policy,
    protocol::ToolOutput,
    store::{SqliteStore, StoreError, WorkflowRunRecord, WorkflowRunSummary},
    tools::{ToolContext, ToolRegistry},
};

pub const WORKFLOW_SCHEMA_VERSION: u16 = 2;
pub const MIN_WORKFLOW_SCHEMA_VERSION: u16 = 1;
pub const MAX_WORKFLOW_BYTES: usize = 1024 * 1024;
pub const MAX_WORKFLOW_STEPS: usize = 256;
pub const MAX_WORKFLOW_CONCURRENCY: usize = 32;
pub const MAX_ARTIFACTS_PER_STEP: usize = 32;
pub const MAX_WORKFLOW_ARTIFACTS: usize = 1024;
pub const MAX_ARTIFACT_PATH_BYTES: usize = 4096;
const WORKFLOW_LEASE_MS: i64 = 300_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDocument {
    pub version: u16,
    pub steps: Vec<StepDefinition>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StepDefinition {
    pub id: String,
    pub tool: String,
    #[serde(default = "empty_object")]
    pub arguments: Value,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub retry: RetryPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetryPolicy {
    pub max_attempts: u8,
    pub delay_ms: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            delay_ms: 0,
        }
    }
}

fn empty_object() -> Value {
    json!({})
}

#[derive(Clone, Debug)]
pub struct ValidatedWorkflow {
    steps: Vec<ValidatedStep>,
}

#[derive(Clone, Debug)]
struct ValidatedStep {
    id: String,
    tool: String,
    arguments: Value,
    dependencies: Vec<usize>,
    retry: RetryPolicy,
    artifacts: Vec<String>,
}

pub async fn load_workflow(
    workspace: &Path,
    path: &Path,
) -> Result<WorkflowDocument, WorkflowError> {
    validate_relative_path(path)?;
    let root = tokio::fs::canonicalize(workspace)
        .await
        .map_err(|source| WorkflowError::Read {
            path: workspace.to_path_buf(),
            source,
        })?;
    let candidate = root.join(path);
    let canonical = tokio::fs::canonicalize(&candidate)
        .await
        .map_err(|source| WorkflowError::Read {
            path: candidate.clone(),
            source,
        })?;
    if !canonical.starts_with(&root) {
        return Err(WorkflowError::Path(
            "workflow file resolves outside the workspace".into(),
        ));
    }
    let metadata = tokio::fs::metadata(&canonical)
        .await
        .map_err(|source| WorkflowError::Read {
            path: canonical.clone(),
            source,
        })?;
    if metadata.len() > MAX_WORKFLOW_BYTES as u64 {
        return Err(WorkflowError::TooLarge {
            bytes: metadata.len(),
            limit: MAX_WORKFLOW_BYTES,
        });
    }
    let raw = tokio::fs::read_to_string(&canonical)
        .await
        .map_err(|source| WorkflowError::Read {
            path: canonical,
            source,
        })?;
    reject_unsafe_yaml_features(&raw)?;
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw)?;
    validate_yaml_value(&value, 0)?;
    let document: WorkflowDocument = serde_yaml_ng::from_value(value)?;
    validate_structure(&document)?;
    Ok(document)
}

pub fn validate_workflow(
    document: WorkflowDocument,
    tools: &ToolRegistry,
) -> Result<ValidatedWorkflow, WorkflowError> {
    validate_structure(&document)?;
    let ids = document
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut steps = Vec::with_capacity(document.steps.len());
    for step in document.steps {
        if tools.get(&step.tool).is_none() {
            return Err(WorkflowError::UnknownTool {
                step: step.id,
                tool: step.tool,
            });
        }
        let dependencies = step
            .needs
            .iter()
            .map(|dependency| ids[dependency])
            .collect();
        steps.push(ValidatedStep {
            id: step.id,
            tool: step.tool,
            arguments: step.arguments,
            dependencies,
            retry: step.retry,
            artifacts: step.artifacts,
        });
    }
    Ok(ValidatedWorkflow { steps })
}

fn validate_structure(document: &WorkflowDocument) -> Result<(), WorkflowError> {
    if !(MIN_WORKFLOW_SCHEMA_VERSION..=WORKFLOW_SCHEMA_VERSION).contains(&document.version) {
        return Err(WorkflowError::UnsupportedVersion(document.version));
    }
    if document.steps.is_empty() {
        return Err(WorkflowError::Empty);
    }
    if document.steps.len() > MAX_WORKFLOW_STEPS {
        return Err(WorkflowError::TooManySteps(document.steps.len()));
    }
    let mut ids = HashSet::new();
    let mut artifact_count = 0_usize;
    for step in &document.steps {
        if !valid_id(&step.id) {
            return Err(WorkflowError::InvalidStepId(step.id.clone()));
        }
        if !ids.insert(step.id.clone()) {
            return Err(WorkflowError::DuplicateStepId(step.id.clone()));
        }
        if step.tool.is_empty() || step.tool.len() > 64 {
            return Err(WorkflowError::InvalidToolName {
                step: step.id.clone(),
            });
        }
        if !step.arguments.is_object() {
            return Err(WorkflowError::InvalidArgumentsShape(step.id.clone()));
        }
        if !(1..=10).contains(&step.retry.max_attempts) || step.retry.delay_ms > 60_000 {
            return Err(WorkflowError::InvalidRetryPolicy(step.id.clone()));
        }
        if document.version == 1 && !step.artifacts.is_empty() {
            return Err(WorkflowError::ArtifactsRequireVersion2(step.id.clone()));
        }
        if step.artifacts.len() > MAX_ARTIFACTS_PER_STEP {
            return Err(WorkflowError::TooManyArtifacts(step.id.clone()));
        }
        artifact_count = artifact_count
            .checked_add(step.artifacts.len())
            .ok_or(WorkflowError::TooManyWorkflowArtifacts)?;
        if artifact_count > MAX_WORKFLOW_ARTIFACTS {
            return Err(WorkflowError::TooManyWorkflowArtifacts);
        }
        let mut artifact_paths = HashSet::new();
        for artifact in &step.artifacts {
            validate_artifact_path(artifact).map_err(|reason| {
                WorkflowError::InvalidArtifactPath {
                    step: step.id.clone(),
                    path: artifact.clone(),
                    reason,
                }
            })?;
            let key = if cfg!(windows) {
                artifact.to_ascii_lowercase()
            } else {
                artifact.clone()
            };
            if !artifact_paths.insert(key) {
                return Err(WorkflowError::DuplicateArtifact {
                    step: step.id.clone(),
                    path: artifact.clone(),
                });
            }
        }
    }
    let positions = document
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut indegree = vec![0_usize; document.steps.len()];
    let mut dependents = vec![Vec::new(); document.steps.len()];
    for (index, step) in document.steps.iter().enumerate() {
        let mut dependencies = HashSet::new();
        for dependency in &step.needs {
            if dependency == &step.id {
                return Err(WorkflowError::SelfDependency(step.id.clone()));
            }
            if !dependencies.insert(dependency) {
                return Err(WorkflowError::DuplicateDependency {
                    step: step.id.clone(),
                    dependency: dependency.clone(),
                });
            }
            let Some(&dependency_index) = positions.get(dependency.as_str()) else {
                return Err(WorkflowError::UnknownDependency {
                    step: step.id.clone(),
                    dependency: dependency.clone(),
                });
            };
            indegree[index] += 1;
            dependents[dependency_index].push(index);
        }
    }
    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect::<VecDeque<_>>();
    let mut visited = 0;
    while let Some(index) = ready.pop_front() {
        visited += 1;
        for &dependent in &dependents[index] {
            indegree[dependent] -= 1;
            if indegree[dependent] == 0 {
                ready.push_back(dependent);
            }
        }
    }
    if visited != document.steps.len() {
        return Err(WorkflowError::Cycle);
    }
    Ok(())
}

pub fn workflow_fingerprint(document: &WorkflowDocument) -> Result<String, WorkflowError> {
    let mut normalized = document.clone();
    for step in &mut normalized.steps {
        step.needs.sort();
        step.artifacts.sort();
    }
    let bytes = serde_json::to_vec(&normalized)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

impl ValidatedWorkflow {
    pub fn step_ids(&self) -> Vec<String> {
        self.steps.iter().map(|step| step.id.clone()).collect()
    }

    fn metadata(&self) -> Vec<(String, String, Vec<String>, Vec<String>)> {
        self.steps
            .iter()
            .map(|step| {
                (
                    step.id.clone(),
                    step.tool.clone(),
                    step.dependencies
                        .iter()
                        .map(|index| self.steps[*index].id.clone())
                        .collect(),
                    step.artifacts.clone(),
                )
            })
            .collect()
    }
}

#[derive(Clone)]
pub struct WorkflowRuntime {
    workspace: PathBuf,
    canonical_workspace: PathBuf,
    store: Arc<SqliteStore>,
    tools: ToolRegistry,
    policy: Policy,
    context: ToolContext,
}

pub struct PreparedWorkflow {
    run_id: String,
    runner: WorkflowRunner,
    workflow: ValidatedWorkflow,
    lease: WorkflowLease,
}

struct WorkflowLease {
    store: Arc<SqliteStore>,
    run_id: String,
    owner: String,
}

impl PreparedWorkflow {
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub async fn execute(
        self,
        cancellation: CancellationToken,
    ) -> Result<WorkflowReport, WorkflowError> {
        let mut execution = Box::pin(self.runner.run(self.workflow, cancellation.clone()));
        let mut heartbeat = tokio::time::interval(Duration::from_secs(60));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                result = &mut execution => return result,
                _ = heartbeat.tick() => {
                    if let Err(error) = self.lease.store.renew_workflow_lease(
                        &self.lease.run_id,
                        &self.lease.owner,
                        lease_deadline_ms()?,
                    ) {
                        cancellation.cancel();
                        let _ = execution.await;
                        return Err(error.into());
                    }
                }
            }
        }
    }
}

impl WorkflowRuntime {
    pub fn new(
        workspace: PathBuf,
        store: Arc<SqliteStore>,
        tools: ToolRegistry,
        policy: Policy,
        context: ToolContext,
    ) -> Result<Self, WorkflowError> {
        let canonical_workspace =
            std::fs::canonicalize(&workspace).map_err(|source| WorkflowError::Read {
                path: workspace.clone(),
                source,
            })?;
        Ok(Self {
            workspace,
            canonical_workspace,
            store,
            tools,
            policy,
            context,
        })
    }

    pub fn list_runs(&self, limit: u32) -> Result<Vec<WorkflowRunSummary>, WorkflowError> {
        Ok(self
            .store
            .list_workflow_runs(&self.canonical_workspace, limit)?)
    }

    pub fn inspect_run(&self, run_id: &str) -> Result<WorkflowRunRecord, WorkflowError> {
        self.store
            .load_workflow_run(run_id)?
            .ok_or_else(|| WorkflowError::RunNotFound(run_id.into()))
    }

    pub async fn prepare_start(
        &self,
        file: PathBuf,
        concurrency: usize,
    ) -> Result<PreparedWorkflow, WorkflowError> {
        validate_concurrency(concurrency)?;
        let document = load_workflow(&self.workspace, &file).await?;
        let fingerprint = workflow_fingerprint(&document)?;
        let workflow = validate_workflow(document, &self.tools)?;
        let owner = uuid::Uuid::now_v7().to_string();
        let run_id = self.store.create_workflow_run(
            &self.canonical_workspace,
            &file,
            &fingerprint,
            &workflow.step_ids(),
            &owner,
            lease_deadline_ms()?,
        )?;
        for (step_id, tool, needs, artifacts) in workflow.metadata() {
            self.store
                .set_workflow_step_metadata(&run_id, &step_id, &tool, &needs, &artifacts, &owner)?;
        }
        let runner = WorkflowRunner::new(
            self.tools.clone(),
            self.policy.clone(),
            self.context.clone(),
            concurrency,
        )?
        .with_journal(
            self.store.clone(),
            run_id.clone(),
            owner.clone(),
            Vec::new(),
        );
        Ok(PreparedWorkflow {
            run_id: run_id.clone(),
            runner,
            workflow,
            lease: WorkflowLease {
                store: self.store.clone(),
                run_id: run_id.clone(),
                owner,
            },
        })
    }

    pub async fn prepare_resume(
        &self,
        run_id: &str,
        concurrency: usize,
    ) -> Result<PreparedWorkflow, WorkflowError> {
        validate_concurrency(concurrency)?;
        let record = self.inspect_run(run_id)?;
        let record_workspace = std::fs::canonicalize(&record.workspace_path).map_err(|source| {
            WorkflowError::Read {
                path: PathBuf::from(&record.workspace_path),
                source,
            }
        })?;
        if record_workspace != self.canonical_workspace {
            return Err(WorkflowError::WorkspaceChanged);
        }
        let file = PathBuf::from(&record.workflow_path);
        let document = load_workflow(&self.workspace, &file).await?;
        if workflow_fingerprint(&document)? != record.fingerprint {
            return Err(WorkflowError::WorkflowModified);
        }
        let workflow = validate_workflow(document, &self.tools)?;
        let stored_ids = record
            .steps
            .iter()
            .map(|step| step.step_id.clone())
            .collect::<Vec<_>>();
        if workflow.step_ids() != stored_ids {
            return Err(WorkflowError::CorruptJournal(
                "stored step order does not match workflow".into(),
            ));
        }
        let previous = record
            .steps
            .iter()
            .filter_map(|step| step.report_json.as_ref())
            .map(|value| serde_json::from_str(value))
            .collect::<Result<Vec<_>, _>>()?;
        let owner = uuid::Uuid::now_v7().to_string();
        let now = unix_time_ms()?;
        if !self
            .store
            .claim_workflow_run(run_id, &owner, now, now + WORKFLOW_LEASE_MS)?
        {
            return Err(WorkflowError::RunBusy);
        }
        let runner = WorkflowRunner::new(
            self.tools.clone(),
            self.policy.clone(),
            self.context.clone(),
            concurrency,
        )?
        .with_journal(self.store.clone(), run_id.into(), owner.clone(), previous);
        Ok(PreparedWorkflow {
            run_id: run_id.into(),
            runner,
            workflow,
            lease: WorkflowLease {
                store: self.store.clone(),
                run_id: run_id.into(),
                owner,
            },
        })
    }
}

fn validate_concurrency(concurrency: usize) -> Result<(), WorkflowError> {
    if !(1..=MAX_WORKFLOW_CONCURRENCY).contains(&concurrency) {
        return Err(WorkflowError::InvalidConcurrency(concurrency));
    }
    Ok(())
}

#[derive(Clone)]
pub struct WorkflowRunner {
    tools: ToolRegistry,
    policy: Policy,
    context: ToolContext,
    concurrency: usize,
    journal: Option<WorkflowJournal>,
}

#[derive(Clone)]
struct WorkflowJournal {
    store: Arc<SqliteStore>,
    run_id: String,
    owner: String,
    previous: HashMap<String, StepReport>,
}

impl WorkflowRunner {
    pub fn new(
        tools: ToolRegistry,
        policy: Policy,
        context: ToolContext,
        concurrency: usize,
    ) -> Result<Self, WorkflowError> {
        if !(1..=MAX_WORKFLOW_CONCURRENCY).contains(&concurrency) {
            return Err(WorkflowError::InvalidConcurrency(concurrency));
        }
        Ok(Self {
            tools,
            policy,
            context,
            concurrency,
            journal: None,
        })
    }

    pub fn with_journal(
        mut self,
        store: Arc<SqliteStore>,
        run_id: String,
        owner: String,
        previous: Vec<StepReport>,
    ) -> Self {
        self.journal = Some(WorkflowJournal {
            store,
            run_id,
            owner,
            previous: previous
                .into_iter()
                .map(|report| (report.id.clone(), report))
                .collect(),
        });
        self
    }

    pub async fn run(
        &self,
        workflow: ValidatedWorkflow,
        cancellation: CancellationToken,
    ) -> Result<WorkflowReport, WorkflowError> {
        let run_id = self
            .journal
            .as_ref()
            .map(|journal| journal.run_id.clone())
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let count = workflow.steps.len();
        let mut states = vec![RuntimeState::Pending; count];
        let mut reports = vec![None; count];
        let mut attempts = vec![0_u32; count];
        let mut journaled = vec![false; count];
        if let Some(journal) = &self.journal {
            for report in journal.previous.values() {
                let Some(index) = workflow.steps.iter().position(|step| step.id == report.id)
                else {
                    return Err(WorkflowError::CorruptJournal(format!(
                        "stored step {} is not in the workflow",
                        report.id
                    )));
                };
                attempts[index] = report.attempts;
                if report.status == StepStatus::Succeeded {
                    states[index] = RuntimeState::Done;
                    reports[index] = Some(report.clone());
                    journaled[index] = true;
                }
            }
        }
        let mut ready = VecDeque::new();
        for (index, step) in workflow.steps.iter().enumerate() {
            if states[index] == RuntimeState::Pending && step.dependencies.is_empty() {
                states[index] = RuntimeState::Ready;
                ready.push_back(index);
            }
        }
        let mut running = FuturesUnordered::new();
        loop {
            settle_dependencies(
                &workflow.steps,
                &mut states,
                &mut reports,
                &attempts,
                &mut ready,
            );
            self.persist_new_reports(&reports, &mut journaled)?;
            while running.len() < self.concurrency && !cancellation.is_cancelled() {
                let Some(index) = ready.pop_front() else {
                    break;
                };
                if states[index] != RuntimeState::Ready {
                    continue;
                }
                states[index] = RuntimeState::Running;
                let step = workflow.steps[index].clone();
                let tools = self.tools.clone();
                let policy = self.policy.clone();
                let context = self.context.clone();
                let cancellation = cancellation.clone();
                let journal = self.journal.clone();
                let previous_attempts = attempts[index];
                running.push(async move {
                    (
                        index,
                        execute_step(
                            step,
                            tools,
                            policy,
                            context,
                            cancellation,
                            journal,
                            previous_attempts,
                        )
                        .await,
                    )
                });
            }
            if running.is_empty() {
                break;
            }
            if let Some((index, report)) = running.next().await {
                let report = report?;
                self.persist_report(&report)?;
                journaled[index] = true;
                attempts[index] = report.attempts;
                states[index] = RuntimeState::Done;
                reports[index] = Some(report);
            }
        }
        if cancellation.is_cancelled() {
            for (index, state) in states.iter_mut().enumerate() {
                if *state != RuntimeState::Done {
                    *state = RuntimeState::Done;
                    reports[index] = Some(StepReport {
                        id: workflow.steps[index].id.clone(),
                        tool: workflow.steps[index].tool.clone(),
                        status: StepStatus::Cancelled,
                        attempts: attempts[index],
                        output: None,
                        blocked_by: Vec::new(),
                        artifacts: Vec::new(),
                        error: Some(WorkflowFailure {
                            code: "cancelled".into(),
                            message: "workflow was cancelled".into(),
                        }),
                    });
                }
            }
            self.persist_new_reports(&reports, &mut journaled)?;
        }
        let steps = reports
            .into_iter()
            .enumerate()
            .map(|(index, report)| {
                report.unwrap_or_else(|| StepReport {
                    id: workflow.steps[index].id.clone(),
                    tool: workflow.steps[index].tool.clone(),
                    status: StepStatus::Skipped,
                    attempts: attempts[index],
                    output: None,
                    blocked_by: Vec::new(),
                    artifacts: Vec::new(),
                    error: Some(WorkflowFailure {
                        code: "unscheduled".into(),
                        message: "step could not be scheduled".into(),
                    }),
                })
            })
            .collect::<Vec<_>>();
        let status = if steps
            .iter()
            .any(|step| step.status == StepStatus::Cancelled)
        {
            WorkflowStatus::Cancelled
        } else if steps
            .iter()
            .all(|step| step.status == StepStatus::Succeeded)
        {
            WorkflowStatus::Succeeded
        } else {
            WorkflowStatus::Failed
        };
        let report = WorkflowReport {
            schema_version: 3,
            run_id: run_id.clone(),
            status,
            steps,
        };
        if let Some(journal) = &self.journal {
            journal.store.finish_workflow_run(
                &run_id,
                workflow_status_name(&report.status),
                &journal.owner,
            )?;
        }
        Ok(report)
    }

    fn persist_new_reports(
        &self,
        reports: &[Option<StepReport>],
        journaled: &mut [bool],
    ) -> Result<(), WorkflowError> {
        for (index, report) in reports.iter().enumerate() {
            if !journaled[index]
                && let Some(report) = report
            {
                self.persist_report(report)?;
                journaled[index] = true;
            }
        }
        Ok(())
    }

    fn persist_report(&self, report: &StepReport) -> Result<(), WorkflowError> {
        if let Some(journal) = &self.journal {
            journal.store.renew_workflow_lease(
                &journal.run_id,
                &journal.owner,
                lease_deadline_ms()?,
            )?;
            journal.store.complete_workflow_step(
                &journal.run_id,
                &report.id,
                step_status_name(&report.status),
                &serde_json::to_string(report)?,
                &journal.owner,
            )?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeState {
    Pending,
    Ready,
    Running,
    Done,
}

fn settle_dependencies(
    steps: &[ValidatedStep],
    states: &mut [RuntimeState],
    reports: &mut [Option<StepReport>],
    attempts: &[u32],
    ready: &mut VecDeque<usize>,
) {
    loop {
        let mut changed = false;
        for (index, step) in steps.iter().enumerate() {
            if states[index] != RuntimeState::Pending
                || !step
                    .dependencies
                    .iter()
                    .all(|dependency| states[*dependency] == RuntimeState::Done)
            {
                continue;
            }
            let blocked_by = step
                .dependencies
                .iter()
                .filter_map(|dependency| {
                    reports[*dependency]
                        .as_ref()
                        .filter(|report| report.status != StepStatus::Succeeded)
                        .map(|report| report.id.clone())
                })
                .collect::<Vec<_>>();
            if blocked_by.is_empty() {
                states[index] = RuntimeState::Ready;
                ready.push_back(index);
            } else {
                states[index] = RuntimeState::Done;
                reports[index] = Some(StepReport {
                    id: step.id.clone(),
                    tool: step.tool.clone(),
                    status: StepStatus::Skipped,
                    attempts: attempts[index],
                    output: None,
                    blocked_by,
                    artifacts: Vec::new(),
                    error: Some(WorkflowFailure {
                        code: "dependency_failed".into(),
                        message: "one or more dependencies did not succeed".into(),
                    }),
                });
            }
            changed = true;
        }
        if !changed {
            break;
        }
    }
}

async fn execute_step(
    step: ValidatedStep,
    tools: ToolRegistry,
    policy: Policy,
    context: ToolContext,
    cancellation: CancellationToken,
    journal: Option<WorkflowJournal>,
    previous_attempts: u32,
) -> Result<StepReport, WorkflowError> {
    let mut cumulative_attempts = previous_attempts;
    for local_attempt in 1..=step.retry.max_attempts {
        if cancellation.is_cancelled() {
            return Ok(cancelled_report(&step, cumulative_attempts));
        }
        cumulative_attempts = if let Some(journal) = &journal {
            journal.store.renew_workflow_lease(
                &journal.run_id,
                &journal.owner,
                lease_deadline_ms()?,
            )?;
            journal
                .store
                .begin_workflow_step(&journal.run_id, &step.id, &journal.owner)?
        } else {
            previous_attempts + u32::from(local_attempt)
        };
        let report = execute_attempt(&step, &tools, &policy, &context, cumulative_attempts).await;
        let retryable = report
            .error
            .as_ref()
            .is_some_and(|error| matches!(error.code.as_str(), "tool_failed" | "tool_error"));
        if report.status == StepStatus::Succeeded
            || !retryable
            || local_attempt == step.retry.max_attempts
        {
            return Ok(report);
        }
        if step.retry.delay_ms > 0 {
            tokio::select! {
                _ = cancellation.cancelled() => return Ok(cancelled_report(&step, cumulative_attempts)),
                _ = tokio::time::sleep(Duration::from_millis(u64::from(step.retry.delay_ms))) => {}
            }
        }
    }
    unreachable!("retry policy always has at least one attempt")
}

async fn execute_attempt(
    step: &ValidatedStep,
    tools: &ToolRegistry,
    policy: &Policy,
    context: &ToolContext,
    attempts: u32,
) -> StepReport {
    let Some(tool) = tools.get(&step.tool) else {
        return failed_report(
            step,
            "unknown_tool",
            "tool disappeared from registry",
            None,
            attempts,
        );
    };
    let permission = match tool.permission_request(&step.arguments) {
        Ok(permission) => permission,
        Err(error) => {
            return failed_report(
                step,
                "invalid_arguments",
                &error.to_string(),
                None,
                attempts,
            );
        }
    };
    let decision = policy.decide(&permission);
    if !decision.allowed {
        return failed_report(step, "permission_denied", &decision.reason, None, attempts);
    }
    match tool.execute(step.arguments.clone(), context).await {
        Ok(output) if output.success => {
            match capture_artifacts(context.workspace.clone(), step.artifacts.clone()).await {
                Ok(artifacts) => StepReport {
                    id: step.id.clone(),
                    tool: step.tool.clone(),
                    status: StepStatus::Succeeded,
                    attempts,
                    output: Some(output),
                    blocked_by: Vec::new(),
                    artifacts,
                    error: None,
                },
                Err(error) => {
                    failed_report(step, error.code, &error.message, Some(output), attempts)
                }
            }
        }
        Ok(output) => {
            let message = if output.stderr.is_empty() {
                "tool returned an unsuccessful result".into()
            } else {
                output.stderr.clone()
            };
            failed_report(step, "tool_failed", &message, Some(output), attempts)
        }
        Err(error) => failed_report(step, "tool_error", &error.to_string(), None, attempts),
    }
}

fn failed_report(
    step: &ValidatedStep,
    code: &str,
    message: &str,
    output: Option<ToolOutput>,
    attempts: u32,
) -> StepReport {
    StepReport {
        id: step.id.clone(),
        tool: step.tool.clone(),
        status: StepStatus::Failed,
        attempts,
        output,
        blocked_by: Vec::new(),
        artifacts: Vec::new(),
        error: Some(WorkflowFailure {
            code: code.into(),
            message: message.into(),
        }),
    }
}

fn cancelled_report(step: &ValidatedStep, attempts: u32) -> StepReport {
    StepReport {
        id: step.id.clone(),
        tool: step.tool.clone(),
        status: StepStatus::Cancelled,
        attempts,
        output: None,
        blocked_by: Vec::new(),
        artifacts: Vec::new(),
        error: Some(WorkflowFailure {
            code: "cancelled".into(),
            message: "workflow was cancelled".into(),
        }),
    }
}

struct ArtifactCaptureFailure {
    code: &'static str,
    message: String,
}

async fn capture_artifacts(
    workspace: PathBuf,
    artifacts: Vec<String>,
) -> Result<Vec<ArtifactMetadata>, ArtifactCaptureFailure> {
    if artifacts.is_empty() {
        return Ok(Vec::new());
    }
    tokio::task::spawn_blocking(move || capture_artifacts_blocking(&workspace, &artifacts))
        .await
        .map_err(|error| ArtifactCaptureFailure {
            code: "artifact_io",
            message: format!("artifact worker failed: {error}"),
        })?
}

fn capture_artifacts_blocking(
    workspace: &Path,
    artifacts: &[String],
) -> Result<Vec<ArtifactMetadata>, ArtifactCaptureFailure> {
    let root = Dir::open_ambient_dir(workspace, ambient_authority()).map_err(|error| {
        ArtifactCaptureFailure {
            code: "artifact_io",
            message: format!("failed to open workspace for artifact capture: {error}"),
        }
    })?;
    let mut captured = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        validate_artifact_path(artifact).map_err(|reason| ArtifactCaptureFailure {
            code: "artifact_invalid",
            message: format!("invalid artifact {artifact}: {reason}"),
        })?;
        let relative = Path::new(artifact);
        let mut partial = PathBuf::new();
        for component in relative.components() {
            if let Component::Normal(part) = component {
                partial.push(part);
                let metadata =
                    root.symlink_metadata(&partial)
                        .map_err(|error| ArtifactCaptureFailure {
                            code: if error.kind() == std::io::ErrorKind::NotFound {
                                "artifact_missing"
                            } else {
                                "artifact_io"
                            },
                            message: format!("failed to inspect artifact {artifact}: {error}"),
                        })?;
                if metadata.file_type().is_symlink() {
                    return Err(ArtifactCaptureFailure {
                        code: "artifact_invalid",
                        message: format!("artifact path contains a symbolic link: {artifact}"),
                    });
                }
            }
        }
        let mut file = root
            .open(relative)
            .map_err(|error| ArtifactCaptureFailure {
                code: if error.kind() == std::io::ErrorKind::NotFound {
                    "artifact_missing"
                } else {
                    "artifact_io"
                },
                message: format!("failed to open artifact {artifact}: {error}"),
            })?;
        let metadata = file.metadata().map_err(|error| ArtifactCaptureFailure {
            code: "artifact_io",
            message: format!("failed to read artifact metadata {artifact}: {error}"),
        })?;
        if !metadata.is_file() {
            return Err(ArtifactCaptureFailure {
                code: "artifact_invalid",
                message: format!("artifact is not a regular file: {artifact}"),
            });
        }
        let mut hasher = Sha256::new();
        let mut size_bytes = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| ArtifactCaptureFailure {
                    code: "artifact_io",
                    message: format!("failed to read artifact {artifact}: {error}"),
                })?;
            if count == 0 {
                break;
            }
            size_bytes =
                size_bytes
                    .checked_add(count as u64)
                    .ok_or_else(|| ArtifactCaptureFailure {
                        code: "artifact_io",
                        message: format!("artifact size overflow: {artifact}"),
                    })?;
            hasher.update(&buffer[..count]);
        }
        captured.push(ArtifactMetadata {
            path: artifact.clone(),
            size_bytes,
            sha256: hex::encode(hasher.finalize()),
        });
    }
    Ok(captured)
}

fn lease_deadline_ms() -> Result<i64, WorkflowError> {
    Ok(unix_time_ms()? + WORKFLOW_LEASE_MS)
}

fn unix_time_ms() -> Result<i64, WorkflowError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| WorkflowError::Time(error.to_string()))?;
    i64::try_from(now.as_millis()).map_err(|_| WorkflowError::Time("timestamp overflow".into()))
}

fn step_status_name(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Succeeded => "succeeded",
        StepStatus::Failed => "failed",
        StepStatus::Skipped => "skipped",
        StepStatus::Cancelled => "cancelled",
    }
}

fn workflow_status_name(status: &WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Succeeded => "succeeded",
        WorkflowStatus::Failed => "failed",
        WorkflowStatus::Cancelled => "cancelled",
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStatus {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Succeeded,
    Failed,
    Skipped,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowReport {
    pub schema_version: u16,
    pub run_id: String,
    pub status: WorkflowStatus,
    pub steps: Vec<StepReport>,
}

impl WorkflowReport {
    pub fn succeeded(&self) -> bool {
        self.status == WorkflowStatus::Succeeded
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StepReport {
    pub id: String,
    pub tool: String,
    pub status: StepStatus,
    pub attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<ToolOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<WorkflowFailure>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ArtifactMetadata {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowFailure {
    pub code: String,
    pub message: String,
}

fn reject_unsafe_yaml_features(raw: &str) -> Result<(), WorkflowError> {
    if raw.lines().any(|line| {
        let content = line.split('#').next().unwrap_or("");
        content.contains("<<:")
            || content.split_whitespace().any(|token| {
                token.starts_with('&') || token.starts_with('*') || token.starts_with('!')
            })
    }) {
        return Err(WorkflowError::UnsupportedYamlFeature);
    }
    Ok(())
}

fn validate_yaml_value(value: &serde_yaml_ng::Value, depth: usize) -> Result<(), WorkflowError> {
    if depth > 32 {
        return Err(WorkflowError::TooDeep);
    }
    match value {
        serde_yaml_ng::Value::Sequence(values) => {
            for value in values {
                validate_yaml_value(value, depth + 1)?;
            }
        }
        serde_yaml_ng::Value::Mapping(mapping) => {
            for (key, value) in mapping {
                if !matches!(key, serde_yaml_ng::Value::String(_)) {
                    return Err(WorkflowError::NonStringKey);
                }
                validate_yaml_value(value, depth + 1)?;
            }
        }
        serde_yaml_ng::Value::Tagged(_) => return Err(WorkflowError::UnsupportedYamlFeature),
        _ => {}
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<(), WorkflowError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(WorkflowError::Path(
            "workflow path must be relative and inside the workspace".into(),
        ));
    }
    Ok(())
}

fn validate_artifact_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("path is empty".into());
    }
    if path.len() > MAX_ARTIFACT_PATH_BYTES {
        return Err(format!("path exceeds {MAX_ARTIFACT_PATH_BYTES} bytes"));
    }
    if path.contains('\0') {
        return Err("path contains a NUL byte".into());
    }
    if path
        .split(['/', '\\'])
        .any(|component| matches!(component, "." | ".."))
    {
        return Err("dot and parent components are not allowed".into());
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Err("path is absolute".into());
    }
    let mut normal_components = 0_usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => normal_components += 1,
            Component::CurDir => return Err("current-directory components are not allowed".into()),
            Component::ParentDir => {
                return Err("parent-directory components are not allowed".into());
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err("rooted or prefixed paths are not allowed".into());
            }
        }
    }
    if normal_components == 0 {
        return Err("path has no file component".into());
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 64 {
        return false;
    }
    let mut bytes = id.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("failed to read workflow {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("workflow is {bytes} bytes, limit is {limit}")]
    TooLarge { bytes: u64, limit: usize },
    #[error("workflow path rejected: {0}")]
    Path(String),
    #[error("invalid workflow YAML: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
    #[error("YAML tags, anchors, aliases, and merge keys are not supported")]
    UnsupportedYamlFeature,
    #[error("workflow YAML nesting exceeds 32 levels")]
    TooDeep,
    #[error("workflow mapping keys must be strings")]
    NonStringKey,
    #[error("unsupported workflow version: {0}")]
    UnsupportedVersion(u16),
    #[error("workflow must contain at least one step")]
    Empty,
    #[error("workflow contains too many steps: {0}")]
    TooManySteps(usize),
    #[error("invalid workflow step id: {0}")]
    InvalidStepId(String),
    #[error("duplicate workflow step id: {0}")]
    DuplicateStepId(String),
    #[error("step {step} has an invalid tool name")]
    InvalidToolName { step: String },
    #[error("step {0} arguments must be an object")]
    InvalidArgumentsShape(String),
    #[error("step {0} cannot depend on itself")]
    SelfDependency(String),
    #[error("step {step} repeats dependency {dependency}")]
    DuplicateDependency { step: String, dependency: String },
    #[error("step {step} has unknown dependency {dependency}")]
    UnknownDependency { step: String, dependency: String },
    #[error("workflow dependency graph contains a cycle")]
    Cycle,
    #[error("step {step} references unknown tool {tool}")]
    UnknownTool { step: String, tool: String },
    #[error("workflow concurrency must be between 1 and {MAX_WORKFLOW_CONCURRENCY}, got {0}")]
    InvalidConcurrency(usize),
    #[error("invalid retry policy for step {0}")]
    InvalidRetryPolicy(String),
    #[error("step {0} must use workflow version 2 to declare artifacts")]
    ArtifactsRequireVersion2(String),
    #[error("step {0} declares more than {MAX_ARTIFACTS_PER_STEP} artifacts")]
    TooManyArtifacts(String),
    #[error("workflow declares more than {MAX_WORKFLOW_ARTIFACTS} artifacts")]
    TooManyWorkflowArtifacts,
    #[error("step {step} has invalid artifact path {path}: {reason}")]
    InvalidArtifactPath {
        step: String,
        path: String,
        reason: String,
    },
    #[error("step {step} repeats artifact path {path}")]
    DuplicateArtifact { step: String, path: String },
    #[error("workflow journal is corrupt: {0}")]
    CorruptJournal(String),
    #[error("workflow persistence failed: {0}")]
    Store(#[from] StoreError),
    #[error("workflow report serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("workflow clock failed: {0}")]
    Time(String),
    #[error("workflow run not found: {0}")]
    RunNotFound(String),
    #[error("workflow changed since the run was created")]
    WorkflowModified,
    #[error("workflow run belongs to a different workspace")]
    WorkspaceChanged,
    #[error("workflow run is currently owned by another process")]
    RunBusy,
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;

    use super::*;
    use crate::{
        permission::{GitReadOperation, PermissionRequest},
        protocol::ToolSpec,
        tools::{Tool, ToolError},
    };

    struct TestTool {
        name: &'static str,
        succeeds: bool,
        delay: Duration,
        active: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    struct FlakyTool {
        attempts: Arc<AtomicUsize>,
        succeed_on: usize,
    }

    #[async_trait]
    impl Tool for FlakyTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "flaky".into(),
                description: "fails before a configured attempt".into(),
                input_schema: json!({"type": "object"}),
            }
        }

        fn permission_request(&self, _arguments: &Value) -> Result<PermissionRequest, ToolError> {
            Ok(PermissionRequest::GitRead {
                operation: GitReadOperation::Status,
            })
        }

        async fn execute(
            &self,
            _arguments: Value,
            _context: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(ToolOutput {
                success: attempt >= self.succeed_on,
                stdout: String::new(),
                stderr: if attempt >= self.succeed_on {
                    String::new()
                } else {
                    "transient".into()
                },
                truncated: false,
                metadata: json!({"attempt": attempt}),
            })
        }
    }

    #[async_trait]
    impl Tool for TestTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "test tool".into(),
                input_schema: json!({"type": "object"}),
            }
        }

        fn permission_request(&self, _arguments: &Value) -> Result<PermissionRequest, ToolError> {
            Ok(PermissionRequest::GitRead {
                operation: GitReadOperation::Status,
            })
        }

        async fn execute(
            &self,
            _arguments: Value,
            _context: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(ToolOutput {
                success: self.succeeds,
                stdout: self.name.into(),
                stderr: if self.succeeds { "" } else { "failed" }.into(),
                truncated: false,
                metadata: json!({}),
            })
        }
    }

    fn runner(
        directory: &tempfile::TempDir,
        registry: ToolRegistry,
        concurrency: usize,
    ) -> WorkflowRunner {
        WorkflowRunner::new(
            registry,
            Policy::new(directory.path().to_path_buf(), false, false, false),
            ToolContext {
                workspace: directory.path().to_path_buf(),
                max_output_bytes: 1024,
                max_file_bytes: 1024,
                shell_timeout: Duration::from_secs(5),
            },
            concurrency,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn parser_rejects_cycles_before_execution() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("cycle.yml"),
            "version: 1\nsteps:\n  - id: a\n    tool: read_file\n    needs: [b]\n  - id: b\n    tool: read_file\n    needs: [a]\n",
        )
        .unwrap();
        let error = load_workflow(directory.path(), Path::new("cycle.yml"))
            .await
            .unwrap_err();
        assert!(matches!(error, WorkflowError::Cycle));
    }

    #[tokio::test]
    async fn independent_steps_run_concurrently() {
        let directory = tempfile::tempdir().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::default();
        for name in ["first", "second"] {
            registry
                .register(TestTool {
                    name,
                    succeeds: true,
                    delay: Duration::from_millis(50),
                    active: active.clone(),
                    peak: peak.clone(),
                })
                .unwrap();
        }
        let workflow = validate_workflow(
            WorkflowDocument {
                version: 1,
                steps: vec![
                    StepDefinition {
                        id: "first".into(),
                        tool: "first".into(),
                        arguments: json!({}),
                        needs: vec![],
                        retry: RetryPolicy::default(),
                        artifacts: Vec::new(),
                    },
                    StepDefinition {
                        id: "second".into(),
                        tool: "second".into(),
                        arguments: json!({}),
                        needs: vec![],
                        retry: RetryPolicy::default(),
                        artifacts: Vec::new(),
                    },
                ],
            },
            &registry,
        )
        .unwrap();
        let report = runner(&directory, registry, 2)
            .run(workflow, CancellationToken::new())
            .await
            .unwrap();
        assert!(report.succeeded());
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failure_skips_descendants_but_not_independent_branches() {
        let directory = tempfile::tempdir().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::default();
        for (name, succeeds) in [("fail", false), ("pass", true)] {
            registry
                .register(TestTool {
                    name,
                    succeeds,
                    delay: Duration::from_millis(1),
                    active: active.clone(),
                    peak: peak.clone(),
                })
                .unwrap();
        }
        let workflow = validate_workflow(
            WorkflowDocument {
                version: 1,
                steps: vec![
                    StepDefinition {
                        id: "root".into(),
                        tool: "fail".into(),
                        arguments: json!({}),
                        needs: vec![],
                        retry: RetryPolicy::default(),
                        artifacts: Vec::new(),
                    },
                    StepDefinition {
                        id: "child".into(),
                        tool: "pass".into(),
                        arguments: json!({}),
                        needs: vec!["root".into()],
                        retry: RetryPolicy::default(),
                        artifacts: Vec::new(),
                    },
                    StepDefinition {
                        id: "independent".into(),
                        tool: "pass".into(),
                        arguments: json!({}),
                        needs: vec![],
                        retry: RetryPolicy::default(),
                        artifacts: Vec::new(),
                    },
                ],
            },
            &registry,
        )
        .unwrap();
        let report = runner(&directory, registry, 2)
            .run(workflow, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report.status, WorkflowStatus::Failed);
        assert_eq!(report.steps[0].status, StepStatus::Failed);
        assert_eq!(report.steps[1].status, StepStatus::Skipped);
        assert_eq!(report.steps[1].blocked_by, ["root"]);
        assert_eq!(report.steps[2].status, StepStatus::Succeeded);
    }

    #[tokio::test]
    async fn retry_policy_succeeds_at_the_configured_bound() {
        let directory = tempfile::tempdir().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::default();
        registry
            .register(FlakyTool {
                attempts: attempts.clone(),
                succeed_on: 3,
            })
            .unwrap();
        let workflow = validate_workflow(
            WorkflowDocument {
                version: 1,
                steps: vec![StepDefinition {
                    id: "flaky".into(),
                    tool: "flaky".into(),
                    arguments: json!({}),
                    needs: vec![],
                    retry: RetryPolicy {
                        max_attempts: 3,
                        delay_ms: 1,
                    },
                    artifacts: Vec::new(),
                }],
            },
            &registry,
        )
        .unwrap();
        let report = runner(&directory, registry, 1)
            .run(workflow, CancellationToken::new())
            .await
            .unwrap();
        assert!(report.succeeded());
        assert_eq!(report.steps[0].attempts, 3);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn fingerprint_normalizes_dependency_order_but_not_semantics() {
        let parse = |needs: Vec<&str>, path: &str| WorkflowDocument {
            version: 1,
            steps: vec![
                StepDefinition {
                    id: "a".into(),
                    tool: "read_file".into(),
                    arguments: json!({"path": "a"}),
                    needs: vec![],
                    retry: RetryPolicy::default(),
                    artifacts: Vec::new(),
                },
                StepDefinition {
                    id: "b".into(),
                    tool: "read_file".into(),
                    arguments: json!({"path": "b"}),
                    needs: vec![],
                    retry: RetryPolicy::default(),
                    artifacts: Vec::new(),
                },
                StepDefinition {
                    id: "final".into(),
                    tool: "read_file".into(),
                    arguments: json!({"path": path}),
                    needs: needs.into_iter().map(String::from).collect(),
                    retry: RetryPolicy::default(),
                    artifacts: Vec::new(),
                },
            ],
        };
        let first = workflow_fingerprint(&parse(vec!["a", "b"], "same")).unwrap();
        let reordered = workflow_fingerprint(&parse(vec!["b", "a"], "same")).unwrap();
        let changed = workflow_fingerprint(&parse(vec!["a", "b"], "changed")).unwrap();
        assert_eq!(first, reordered);
        assert_ne!(first, changed);
    }

    #[tokio::test]
    async fn runtime_prepares_journals_and_lists_workflows() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("input.txt"), "payload").unwrap();
        std::fs::write(
            directory.path().join("workflow.yml"),
            "version: 1\nsteps:\n  - id: read\n    tool: read_file\n    arguments:\n      path: input.txt\n",
        )
        .unwrap();
        let store = Arc::new(SqliteStore::open(&directory.path().join("state.db")).unwrap());
        let runtime = WorkflowRuntime::new(
            directory.path().to_path_buf(),
            store,
            ToolRegistry::standard(),
            Policy::new(directory.path().to_path_buf(), false, false, false),
            ToolContext {
                workspace: directory.path().to_path_buf(),
                max_output_bytes: 1024,
                max_file_bytes: 1024,
                shell_timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let prepared = runtime
            .prepare_start(PathBuf::from("workflow.yml"), 1)
            .await
            .unwrap();
        let run_id = prepared.run_id().to_string();
        let pending = runtime.inspect_run(&run_id).unwrap();
        assert_eq!(pending.steps[0].tool.as_deref(), Some("read_file"));
        assert!(pending.steps[0].needs.is_empty());
        let report = prepared.execute(CancellationToken::new()).await.unwrap();
        assert!(report.succeeded());
        let runs = runtime.list_runs(10).unwrap();
        assert_eq!(runs[0].id, run_id);
        assert_eq!(runs[0].status, "succeeded");
    }

    #[tokio::test]
    async fn artifact_contract_hashes_declared_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("artifact.bin"), b"artifact payload").unwrap();
        std::fs::write(
            directory.path().join("workflow.yml"),
            "version: 2\nsteps:\n  - id: capture\n    tool: read_file\n    arguments:\n      path: artifact.bin\n    artifacts:\n      - artifact.bin\n",
        )
        .unwrap();
        let store = Arc::new(SqliteStore::open(&directory.path().join("state.db")).unwrap());
        let runtime = WorkflowRuntime::new(
            directory.path().to_path_buf(),
            store,
            ToolRegistry::standard(),
            Policy::new(directory.path().to_path_buf(), false, false, false),
            ToolContext {
                workspace: directory.path().to_path_buf(),
                max_output_bytes: 1024,
                max_file_bytes: 1024,
                shell_timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let report = runtime
            .prepare_start(PathBuf::from("workflow.yml"), 1)
            .await
            .unwrap()
            .execute(CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report.schema_version, 3);
        assert!(report.succeeded());
        assert_eq!(report.steps[0].artifacts[0].path, "artifact.bin");
        assert_eq!(report.steps[0].artifacts[0].size_bytes, 16);
        assert_eq!(
            report.steps[0].artifacts[0].sha256,
            hex::encode(Sha256::digest(b"artifact payload"))
        );
    }

    #[tokio::test]
    async fn missing_artifact_fails_after_successful_tool_execution() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("input.txt"), "input").unwrap();
        std::fs::write(
            directory.path().join("workflow.yml"),
            "version: 2\nsteps:\n  - id: capture\n    tool: read_file\n    arguments:\n      path: input.txt\n    artifacts:\n      - missing.bin\n",
        )
        .unwrap();
        let store = Arc::new(SqliteStore::open(&directory.path().join("state.db")).unwrap());
        let runtime = WorkflowRuntime::new(
            directory.path().to_path_buf(),
            store,
            ToolRegistry::standard(),
            Policy::new(directory.path().to_path_buf(), false, false, false),
            ToolContext {
                workspace: directory.path().to_path_buf(),
                max_output_bytes: 1024,
                max_file_bytes: 1024,
                shell_timeout: Duration::from_secs(5),
            },
        )
        .unwrap();
        let report = runtime
            .prepare_start(PathBuf::from("workflow.yml"), 1)
            .await
            .unwrap()
            .execute(CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(report.status, WorkflowStatus::Failed);
        assert_eq!(
            report.steps[0].error.as_ref().unwrap().code,
            "artifact_missing"
        );
        assert!(report.steps[0].output.as_ref().unwrap().success);
        assert!(report.steps[0].artifacts.is_empty());
    }

    #[tokio::test]
    async fn artifact_paths_require_v2_and_cannot_escape_workspace() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("v1.yml"),
            "version: 1\nsteps:\n  - id: bad\n    tool: read_file\n    arguments: {path: file}\n    artifacts: [file]\n",
        )
        .unwrap();
        assert!(matches!(
            load_workflow(directory.path(), Path::new("v1.yml")).await,
            Err(WorkflowError::ArtifactsRequireVersion2(_))
        ));
        std::fs::write(
            directory.path().join("escape.yml"),
            "version: 2\nsteps:\n  - id: bad\n    tool: read_file\n    arguments: {path: file}\n    artifacts: [../secret]\n",
        )
        .unwrap();
        assert!(matches!(
            load_workflow(directory.path(), Path::new("escape.yml")).await,
            Err(WorkflowError::InvalidArtifactPath { .. })
        ));
    }
}
