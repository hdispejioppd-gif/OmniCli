use std::{
    collections::HashMap,
    io::{self, IsTerminal, Stdout},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use unicode_segmentation::UnicodeSegmentation;

use crate::theme;

use crate::{
    Agent, AuthorizationRequest, ModelSpec, PermissionAuthorizer, PermissionDecision, Policy,
    PolicyEvaluation, ProviderFactory, RunEvent, RunEventKind, RunRequest, StepReport,
    SupervisorReport, SupervisorRuntime, Usage, WorkflowReport, WorkflowRuntime,
    agent::RunOutcome,
    events::{EventError, EventSink},
    protocol::{Role, ToolOutput},
    store::{
        SessionSummary, SqliteStore, StoredSession, SupervisorRunRecord, WorkflowRunRecord,
        WorkflowRunSummary,
    },
};

pub struct TuiOptions {
    pub session_id: Option<String>,
    pub verify: bool,
    pub full_access: bool,
    pub initial_model: ModelSpec,
    pub available_models: Vec<ModelSpec>,
    pub data_dir: std::path::PathBuf,
}

pub struct PermissionPrompt {
    request: AuthorizationRequest,
    reason: String,
    reply: oneshot::Sender<bool>,
}

struct InteractiveAuthorizer {
    policy: Policy,
    sender: mpsc::Sender<PermissionPrompt>,
}

#[async_trait]
impl PermissionAuthorizer for InteractiveAuthorizer {
    async fn authorize(&self, request: &AuthorizationRequest) -> PermissionDecision {
        match self.policy.evaluate(&request.permission) {
            PolicyEvaluation::Resolved(decision) => decision,
            PolicyEvaluation::RequiresApproval { reason } => {
                let (reply, receiver) = oneshot::channel();
                let prompt = PermissionPrompt {
                    request: request.clone(),
                    reason: reason.clone(),
                    reply,
                };
                if self.sender.send(prompt).await.is_err() {
                    return PermissionDecision {
                        allowed: false,
                        reason: "interactive permission channel closed".into(),
                    };
                }
                match receiver.await {
                    Ok(true) => PermissionDecision {
                        allowed: true,
                        reason: "approved interactively for this call".into(),
                    },
                    Ok(false) => PermissionDecision {
                        allowed: false,
                        reason: reason.clone(),
                    },
                    Err(_) => PermissionDecision {
                        allowed: false,
                        reason: "interactive permission prompt was abandoned".into(),
                    },
                }
            }
        }
    }
}

pub fn permission_bridge(
    policy: Policy,
) -> (
    Arc<dyn PermissionAuthorizer>,
    mpsc::Receiver<PermissionPrompt>,
) {
    let (sender, receiver) = mpsc::channel(1);
    (Arc::new(InteractiveAuthorizer { policy, sender }), receiver)
}

enum BackendEvent {
    Run(RunEvent),
    Complete(Result<RunOutcome, String>),
    WorkflowStarted(String),
    WorkflowSnapshot(Result<WorkflowRunRecord, String>),
    WorkflowComplete(Result<WorkflowReport, String>),
    SupervisorStarted(String),
    SupervisorSnapshot(Result<SupervisorRunRecord, String>),
    SupervisorComplete(Result<SupervisorReport, String>),
}

struct ChannelSink {
    sender: mpsc::Sender<BackendEvent>,
}

#[async_trait]
impl EventSink for ChannelSink {
    async fn emit(&self, event: &RunEvent) -> Result<(), EventError> {
        self.sender
            .send(BackendEvent::Run(event.clone()))
            .await
            .map_err(|_| EventError::Sink("TUI event channel closed".into()))
    }
}

const MAX_PROMPT_BYTES: usize = 64 * 1024;

#[derive(Default)]
struct Editor {
    text: String,
    cursor: usize,
}

impl Editor {
    fn insert(&mut self, text: &str) {
        let normalized = normalize_input(text);
        let remaining = MAX_PROMPT_BYTES.saturating_sub(self.text.len());
        let mut end = normalized.len().min(remaining);
        while end > 0 && !normalized.is_char_boundary(end) {
            end -= 1;
        }
        self.text.insert_str(self.cursor, &normalized[..end]);
        self.cursor += end;
    }

    fn move_left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
    }

    fn move_right(&mut self) {
        self.cursor = self.text[self.cursor..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(self.text.len(), |(index, _)| self.cursor + index);
    }

    fn backspace(&mut self) {
        let previous = self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
        if previous < self.cursor {
            self.text.replace_range(previous..self.cursor, "");
            self.cursor = previous;
        }
    }

    fn delete(&mut self) {
        let next = self.text[self.cursor..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(self.text.len(), |(index, _)| self.cursor + index);
        if next > self.cursor {
            self.text.replace_range(self.cursor..next, "");
        }
    }

    fn home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
    }

    fn end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index);
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    fn display_with_cursor(&self) -> String {
        let mut display = self.text.clone();
        display.insert(self.cursor, '|');
        display
    }

    fn move_word_left(&mut self) {
        let mut chars: Vec<(usize, char)> = self.text[..self.cursor].char_indices().collect();
        while let Some(&(index, character)) = chars.last() {
            if character.is_alphanumeric() || character == '_' {
                break;
            }
            self.cursor = index;
            chars.pop();
        }
        while let Some(&(index, character)) = chars.last() {
            if !(character.is_alphanumeric() || character == '_') {
                break;
            }
            self.cursor = index;
            chars.pop();
        }
    }

    fn move_word_right(&mut self) {
        let start = self.cursor;
        let mut seen_word = false;
        for (index, character) in self.text[start..].char_indices() {
            let is_word = character.is_alphanumeric() || character == '_';
            if seen_word && !is_word {
                self.cursor = start + index;
                return;
            }
            if is_word {
                seen_word = true;
            }
        }
        self.cursor = self.text.len();
    }

    fn delete_word_back(&mut self) {
        let end = self.cursor;
        self.move_word_left();
        if self.cursor < end {
            self.text.replace_range(self.cursor..end, "");
        }
    }
}

fn normalize_input(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolStatus {
    Requested,
    Awaiting,
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

struct ToolItem {
    name: String,
    arguments: String,
    status: ToolStatus,
    summary: String,
}

#[derive(Default)]
struct ToolTimeline {
    order: Vec<String>,
    items: HashMap<String, ToolItem>,
}

impl ToolTimeline {
    fn requested(&mut self, id: String, name: String, arguments: String) {
        if !self.items.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.items.insert(
            id.clone(),
            ToolItem {
                name,
                arguments,
                status: ToolStatus::Requested,
                summary: String::new(),
            },
        );
    }

    fn set_status(&mut self, id: &str, status: ToolStatus) {
        if let Some(item) = self.items.get_mut(id) {
            item.status = status;
        }
    }

    fn last_pending_mut(&mut self) -> Option<&mut ToolItem> {
        let id = self
            .order
            .iter()
            .rev()
            .find(|id| {
                self.items.get(*id).is_some_and(|item| {
                    matches!(item.status, ToolStatus::Requested | ToolStatus::Awaiting)
                })
            })?
            .clone();
        self.items.get_mut(&id)
    }

    fn finished(&mut self, id: String, output: ToolOutput) {
        if !self.items.contains_key(&id) {
            self.requested(id.clone(), "unknown".into(), String::new());
        }
        if let Some(item) = self.items.get_mut(&id) {
            item.status = if output.success {
                ToolStatus::Succeeded
            } else {
                ToolStatus::Failed
            };
            item.summary = sanitize(
                if output.success {
                    &output.stdout
                } else {
                    &output.stderr
                },
                256,
            );
        }
    }
}

struct SessionPicker {
    sessions: Vec<SessionSummary>,
    selected: usize,
    error: Option<String>,
}

struct ModelPicker {
    models: Vec<ModelSpec>,
    selected: usize,
    error: Option<String>,
}

struct WorkflowDashboard {
    runs: Vec<WorkflowRunSummary>,
    selected: usize,
    selected_step: usize,
    focus: WorkflowFocus,
    detail_scroll: u16,
    snapshot: Option<WorkflowRunRecord>,
    active_run_id: Option<String>,
    error: Option<String>,
}

struct SupervisorDashboard {
    runs: Vec<SupervisorRunRecord>,
    selected: usize,
    snapshot: Option<SupervisorRunRecord>,
    active_run_id: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkflowFocus {
    Runs,
    Steps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveKind {
    Agent,
    Workflow,
    Supervisor,
}

struct App {
    provider: String,
    active_model: ModelSpec,
    available_models: Vec<ModelSpec>,
    session_id: Option<String>,
    verify: bool,
    editor: Editor,
    transcript: Vec<String>,
    streaming: String,
    status: String,
    running: bool,
    exit_requested: bool,
    scroll: u16,
    follow_bottom: bool,
    permission: Option<PermissionPrompt>,
    cancellation: Option<CancellationToken>,
    tools: ToolTimeline,
    sessions: Option<SessionPicker>,
    workflows: Option<WorkflowDashboard>,
    active_kind: Option<ActiveKind>,
    models: Option<ModelPicker>,
    supervisors: Option<SupervisorDashboard>,
    usage: Usage,
    run_started: Option<std::time::Instant>,
    stream_started: Option<std::time::Instant>,
    history: Vec<String>,
    history_index: Option<usize>,
    show_help: bool,
    full_access: bool,
    queued: Vec<String>,
    last_prompt: Option<String>,
    data_dir: std::path::PathBuf,
    palette_selected: usize,
    palette_scroll: usize,
}

impl App {
    fn new(options: TuiOptions) -> Self {
        Self {
            provider: options.initial_model.selector(),
            active_model: options.initial_model,
            available_models: deduplicate_models(options.available_models),
            session_id: options.session_id,
            verify: options.verify,
            editor: Editor::default(),
            transcript: vec!["✦ omni ready — describe a task below to begin.".into()],
            streaming: String::new(),
            status: "idle".into(),
            running: false,
            exit_requested: false,
            scroll: u16::MAX,
            permission: None,
            cancellation: None,
            tools: ToolTimeline::default(),
            sessions: None,
            workflows: None,
            active_kind: None,
            models: None,
            supervisors: None,
            usage: Usage::default(),
            run_started: None,
            stream_started: None,
            history: Vec::new(),
            history_index: None,
            show_help: false,
            full_access: options.full_access,
            queued: Vec::new(),
            last_prompt: None,
            data_dir: options.data_dir,
            palette_selected: 0,
            palette_scroll: 0,
            follow_bottom: true,
        }
    }

    fn palette_visible(&self) -> bool {
        let trimmed = self.editor.text.trim_start();
        trimmed.starts_with('/')
            && !crate::tui_palette::filter_commands(&self.editor.text).is_empty()
    }

    fn handle_run_event(&mut self, event: RunEvent) {
        self.session_id = Some(event.session_id);
        match event.kind {
            RunEventKind::RunStarted => self.status = "running".into(),
            RunEventKind::SystemMessage { .. } => {}
            RunEventKind::UserMessage { message } => {
                self.transcript
                    .push(format!("YOU  {}", sanitize(&message.content, 4096)));
            }
            RunEventKind::ModelTextDelta { text } => {
                if self.streaming.is_empty() {
                    self.stream_started = Some(std::time::Instant::now());
                }
                self.streaming.push_str(&sanitize(&text, 4096))
            }
            RunEventKind::ToolCallRequested { call } => {
                self.flush_streaming();
                let name = sanitize(&call.name, 64);
                self.transcript.push(format!("TOOL {name}"));
                self.status = format!("⚙ {name}");
                self.tools.requested(
                    call.id,
                    call.name,
                    sanitize(&call.arguments.to_string(), 512),
                );
            }
            RunEventKind::DiffPreview { summary, diff, .. } => {
                self.flush_streaming();
                self.transcript
                    .push(format!("DIFF {}", sanitize(&summary, 256)));
                for line in sanitize(&diff, 8192).lines() {
                    self.transcript.push(line.to_string());
                }
            }
            RunEventKind::PermissionResolved { allowed, reason } => {
                if let Some(item) = self.tools.last_pending_mut() {
                    item.status = if allowed {
                        ToolStatus::Running
                    } else {
                        ToolStatus::Failed
                    };
                    item.summary = sanitize(&reason, 256);
                }
            }
            RunEventKind::ToolFinished { call_id, output } => {
                self.tools.finished(call_id, output);
                if self.running {
                    self.status = "running".into();
                }
            }
            RunEventKind::AssistantMessage { .. } => self.flush_streaming(),
            RunEventKind::Usage { usage } => {
                self.usage.input_tokens += usage.input_tokens;
                self.usage.output_tokens += usage.output_tokens;
                self.usage.total_tokens += usage.total_tokens;
            }
            RunEventKind::RunFinished => self.status = "complete".into(),
            RunEventKind::Failed { code, message } => {
                self.transcript
                    .push(format!("ERROR {code}: {}", sanitize(&message, 512)));
            }
        }
        self.follow_bottom = true;
    }

    fn flush_streaming(&mut self) {
        if !self.streaming.is_empty() {
            self.transcript
                .push(format!("OMNI {}", self.streaming.trim_end()));
            self.streaming.clear();
            self.stream_started = None;
        }
    }

    fn complete(&mut self, result: Result<RunOutcome, String>) {
        if let Some(prompt) = self.permission.take() {
            let _ = prompt.reply.send(false);
        }
        self.flush_streaming();
        self.running = false;
        self.active_kind = None;
        self.cancellation = None;
        self.follow_bottom = true;
        match result {
            Ok(outcome) => {
                self.session_id = Some(outcome.session_id);
                if let Some(started) = self.run_started.take() {
                    let secs = started.elapsed().as_secs();
                    self.transcript
                        .push(format!("✓ done in {:02}:{:02}", secs / 60, secs % 60));
                }
                self.status = "idle".into();
            }
            Err(error) => {
                self.status = "error".into();
                self.transcript
                    .push(format!("ERROR {}", sanitize(&error, 1024)));
            }
        }
    }

    fn reset_session(&mut self) {
        self.session_id = None;
        self.editor.clear();
        self.transcript = vec!["New session. Enter a task to begin.".into()];
        self.streaming.clear();
        self.tools = ToolTimeline::default();
        self.sessions = None;
        self.workflows = None;
        self.models = None;
        self.supervisors = None;
        self.status = "idle".into();
    }

    fn load_session(&mut self, session: StoredSession) {
        self.session_id = Some(session.summary.id);
        self.editor.clear();
        self.transcript.clear();
        self.streaming.clear();
        self.tools = ToolTimeline::default();
        for message in session.messages {
            match message.role {
                Role::System if !message.content.is_empty() => self
                    .transcript
                    .push(format!("SYSTEM {}", sanitize(&message.content, 4096))),
                Role::User => self
                    .transcript
                    .push(format!("YOU  {}", sanitize(&message.content, 4096))),
                Role::Assistant => {
                    if !message.content.is_empty() {
                        self.transcript
                            .push(format!("OMNI {}", sanitize(&message.content, 4096)));
                    }
                    for call in message.tool_calls {
                        self.tools.requested(
                            call.id.clone(),
                            call.name,
                            sanitize(&call.arguments.to_string(), 512),
                        );
                        self.tools.set_status(&call.id, ToolStatus::Interrupted);
                    }
                }
                Role::Tool => {
                    if let Some(id) = message.tool_call_id
                        && let Ok(output) = serde_json::from_str::<ToolOutput>(&message.content)
                    {
                        self.tools.finished(id, output);
                    }
                }
                Role::System => {}
            }
        }
        if self.transcript.is_empty() {
            self.transcript
                .push("Session has no visible messages.".into());
        }
        self.sessions = None;
        self.workflows = None;
        self.models = None;
        self.supervisors = None;
        self.status = "idle".into();
        self.follow_bottom = true;
    }

    fn workflow_started(&mut self, run_id: String) {
        self.running = true;
        self.run_started = Some(std::time::Instant::now());
        self.active_kind = Some(ActiveKind::Workflow);
        self.status = "workflow running".into();
        if let Some(dashboard) = &mut self.workflows {
            dashboard.active_run_id = Some(run_id);
        }
    }

    fn workflow_snapshot(&mut self, result: Result<WorkflowRunRecord, String>) {
        if let Some(dashboard) = &mut self.workflows {
            match result {
                Ok(snapshot) => {
                    let selected_step_id = dashboard.snapshot.as_ref().and_then(|previous| {
                        previous
                            .steps
                            .get(dashboard.selected_step)
                            .map(|step| step.step_id.clone())
                    });
                    if let Some(run) = dashboard.runs.iter_mut().find(|run| run.id == snapshot.id) {
                        run.status = snapshot.status.clone();
                        run.updated_at_ms = snapshot.updated_at_ms;
                        run.lease_expires_at_ms = snapshot.lease_expires_at_ms;
                    } else {
                        dashboard.runs.insert(
                            0,
                            WorkflowRunSummary {
                                id: snapshot.id.clone(),
                                workflow_path: snapshot.workflow_path.clone(),
                                status: snapshot.status.clone(),
                                lease_expires_at_ms: snapshot.lease_expires_at_ms,
                                created_at_ms: snapshot.created_at_ms,
                                updated_at_ms: snapshot.updated_at_ms,
                            },
                        );
                        dashboard.selected = 0;
                    }
                    dashboard.selected_step = selected_step_id
                        .and_then(|id| snapshot.steps.iter().position(|step| step.step_id == id))
                        .unwrap_or(0)
                        .min(snapshot.steps.len().saturating_sub(1));
                    dashboard.snapshot = Some(snapshot);
                    dashboard.error = None;
                }
                Err(error) => dashboard.error = Some(error),
            }
        }
    }

    fn workflow_complete(&mut self, result: Result<WorkflowReport, String>) {
        self.running = false;
        self.active_kind = None;
        self.cancellation = None;
        if let Some(dashboard) = &mut self.workflows {
            dashboard.active_run_id = None;
        }
        match result {
            Ok(report) => {
                self.status = format!("workflow {:?}", report.status).to_ascii_lowercase();
                self.transcript
                    .push(format!("WORKFLOW {} {:?}", report.run_id, report.status));
            }
            Err(error) => {
                self.status = "workflow error".into();
                self.transcript
                    .push(format!("WORKFLOW ERROR {}", sanitize(&error, 1024)));
            }
        }
    }

    fn supervisor_snapshot(&mut self, result: Result<SupervisorRunRecord, String>) {
        if let Some(dashboard) = &mut self.supervisors {
            match result {
                Ok(snapshot) => {
                    if let Some(run) = dashboard.runs.iter_mut().find(|run| run.id == snapshot.id) {
                        *run = snapshot.clone();
                    } else {
                        dashboard.runs.insert(0, snapshot.clone());
                        dashboard.selected = 0;
                    }
                    dashboard.snapshot = Some(snapshot);
                    dashboard.error = None;
                }
                Err(error) => dashboard.error = Some(error),
            }
        }
    }

    fn supervisor_complete(&mut self, result: Result<SupervisorReport, String>) {
        self.running = false;
        self.active_kind = None;
        self.cancellation = None;
        if let Some(dashboard) = &mut self.supervisors {
            dashboard.active_run_id = None;
        }
        match result {
            Ok(report) => {
                self.status = format!("supervisor {:?}", report.status).to_ascii_lowercase();
                self.transcript
                    .push(format!("SUPERVISOR {} {:?}", report.run_id, report.status));
            }
            Err(error) => self
                .transcript
                .push(format!("SUPERVISOR ERROR {}", sanitize(&error, 1024))),
        }
    }
}

pub async fn run_tui(
    agent: Arc<Agent>,
    store: Arc<SqliteStore>,
    workflows: Arc<WorkflowRuntime>,
    supervisors: Arc<SupervisorRuntime>,
    provider_factory: Arc<ProviderFactory>,
    permission_receiver: mpsc::Receiver<PermissionPrompt>,
    options: TuiOptions,
) -> Result<(), TuiError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(TuiError::NotTerminal);
    }
    let mut terminal = init_terminal()?;
    let mut guard = RestoreGuard { armed: true };
    let result = event_loop(
        &mut terminal,
        agent,
        store,
        workflows,
        supervisors,
        provider_factory,
        permission_receiver,
        options,
    )
    .await;
    drop(terminal);
    restore_terminal()?;
    guard.armed = false;
    result
}

#[allow(clippy::too_many_arguments)]
async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    agent: Arc<Agent>,
    store: Arc<SqliteStore>,
    workflows: Arc<WorkflowRuntime>,
    supervisors: Arc<SupervisorRuntime>,
    provider_factory: Arc<ProviderFactory>,
    mut permission_receiver: mpsc::Receiver<PermissionPrompt>,
    options: TuiOptions,
) -> Result<(), TuiError> {
    let mut app = App::new(options);
    if app.full_access {
        app.transcript
            .push("⚠ full access — all permissions are granted and auto-approved.".into());
    }
    if let Some(session_id) = app.session_id.clone() {
        match store.load_session(&session_id) {
            Ok(Some(session)) => app.load_session(session),
            Ok(None) => app.status = "session not found".into(),
            Err(error) => app.status = format!("session error: {error}"),
        }
    }
    let (backend_sender, mut backend_receiver) = mpsc::channel(256);
    let mut events = EventStream::new();
    let mut redraw = tokio::time::interval(Duration::from_millis(33));
    redraw.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        terminal.draw(|frame| render(frame, &app))?;
        if app.exit_requested && !app.running {
            break;
        }
        tokio::select! {
                event = events.next() => {
                    match event {
                        Some(Ok(Event::Mouse(mouse))) => match mouse.kind {
                        MouseEventKind::ScrollUp => {
                            app.scroll = app.scroll.saturating_sub(3);
                            app.follow_bottom = false;
                        }
                        MouseEventKind::ScrollDown => {
                            app.scroll = app.scroll.saturating_add(3);
                        }
                            _ => {}
                        },
                        Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                            handle_key(
                                key,
                                &mut app,
                                &agent,
                                &store,
                                &workflows,
                                &supervisors,
                                &provider_factory,
                                &backend_sender,
                            );
                        }
                        Some(Ok(Event::Paste(text))) if app.permission.is_none()
                            && app.sessions.is_none()
                            && app.models.is_none() => {
                            app.editor.insert(&text);
                        }
                        Some(Ok(Event::Resize(_, _))) => {}
                        Some(Err(error)) => return Err(error.into()),
                        None => return Err(TuiError::InputClosed),
                        _ => {}
                    }
                }
                event = backend_receiver.recv() => {
                    match event {
                        Some(BackendEvent::Run(event)) => app.handle_run_event(event),
                        Some(BackendEvent::Complete(result)) => {
                            app.complete(result);
                            if !app.running && !app.queued.is_empty() {
                                let next = app.queued.remove(0);
                                start_agent_run(&mut app, &agent, &backend_sender, next);
                            }
                        }
                        Some(BackendEvent::WorkflowStarted(run_id)) => app.workflow_started(run_id),
                        Some(BackendEvent::WorkflowSnapshot(result)) => app.workflow_snapshot(result),
                        Some(BackendEvent::WorkflowComplete(result)) => app.workflow_complete(result),
                        Some(BackendEvent::SupervisorStarted(run_id)) => {
                            app.running = true;
        app.run_started = Some(std::time::Instant::now());
                            app.active_kind = Some(ActiveKind::Supervisor);
                            if let Some(dashboard) = &mut app.supervisors {
                                dashboard.active_run_id = Some(run_id);
                            }
                        }
                        Some(BackendEvent::SupervisorSnapshot(result)) => app.supervisor_snapshot(result),
                        Some(BackendEvent::SupervisorComplete(result)) => app.supervisor_complete(result),
                        None => return Err(TuiError::BackendClosed),
                    }
                }
                prompt = permission_receiver.recv(), if app.permission.is_none() => {
                    if let Some(prompt) = prompt {
                        if app.full_access {
                            app.transcript.push(format!(
                                "⚠ auto-approved: {} — {}",
                                prompt.request.tool,
                                sanitize(&prompt.reason, 256)
                            ));
                            let _ = prompt.reply.send(true);
                        } else {
                            app.status = "permission".into();
                            app.tools.set_status(&prompt.request.call_id, ToolStatus::Awaiting);
                            app.permission = Some(prompt);
                        }
                    }
                }
                _ = redraw.tick() => {}
            }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_key(
    key: KeyEvent,
    app: &mut App,
    agent: &Arc<Agent>,
    store: &Arc<SqliteStore>,
    workflows: &Arc<WorkflowRuntime>,
    supervisors: &Arc<SupervisorRuntime>,
    provider_factory: &Arc<ProviderFactory>,
    sender: &mpsc::Sender<BackendEvent>,
) {
    if key.code == KeyCode::F(1) {
        app.show_help = !app.show_help;
        return;
    }
    if app.show_help && key.code == KeyCode::Esc {
        app.show_help = false;
        return;
    }
    if let Some(prompt) = app.permission.take() {
        match key.code {
            KeyCode::Char('y' | 'Y') => {
                let _ = prompt.reply.send(true);
                app.status = "running".into();
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                let _ = prompt.reply.send(false);
                app.status = "running".into();
            }
            KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let _ = prompt.reply.send(false);
                app.exit_requested = true;
                if let Some(cancellation) = &app.cancellation {
                    cancellation.cancel();
                }
            }
            _ => app.permission = Some(prompt),
        }
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('q' | 'c'))
    {
        if app.running {
            if let Some(cancellation) = &app.cancellation {
                cancellation.cancel();
            }
            app.status = "cancelling".into();
            if key.code == KeyCode::Char('q') {
                app.exit_requested = true;
            }
        } else {
            app.exit_requested = true;
        }
        return;
    }
    if let Some(picker) = &mut app.models {
        let mut selected = None;
        match key.code {
            KeyCode::Esc => app.models = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => {
                picker.selected = (picker.selected + 1).min(picker.models.len().saturating_sub(1));
            }
            KeyCode::Home => picker.selected = 0,
            KeyCode::End => picker.selected = picker.models.len().saturating_sub(1),
            KeyCode::Enter => selected = picker.models.get(picker.selected).cloned(),
            _ => {}
        }
        if let Some(spec) = selected {
            switch_model(app, agent, provider_factory, spec);
        }
        return;
    }
    if let Some(dashboard) = &mut app.supervisors {
        match key.code {
            KeyCode::Esc => app.supervisors = None,
            KeyCode::Up => {
                dashboard.selected = dashboard.selected.saturating_sub(1);
                inspect_selected_supervisor(dashboard, supervisors);
            }
            KeyCode::Down => {
                dashboard.selected =
                    (dashboard.selected + 1).min(dashboard.runs.len().saturating_sub(1));
                inspect_selected_supervisor(dashboard, supervisors);
            }
            KeyCode::Enter if !app.running => {
                if let Some(session) = dashboard
                    .snapshot
                    .as_ref()
                    .and_then(|run| run.tasks.first())
                    .map(|task| task.session_id.clone())
                    && let Ok(Some(session)) = store.load_session(&session)
                {
                    app.load_session(session);
                }
            }
            KeyCode::Char('c') if app.active_kind == Some(ActiveKind::Supervisor) => {
                if let Some(token) = &app.cancellation {
                    token.cancel();
                }
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                refresh_supervisor_dashboard(dashboard, supervisors)
            }
            _ => {}
        }
        return;
    }
    if let Some(dashboard) = &mut app.workflows {
        match key.code {
            KeyCode::Esc | KeyCode::Char('w')
                if key.code == KeyCode::Esc || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if dashboard.focus == WorkflowFocus::Steps && key.code == KeyCode::Esc {
                    dashboard.focus = WorkflowFocus::Runs;
                } else {
                    app.workflows = None;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if dashboard.focus == WorkflowFocus::Runs {
                    dashboard.selected = dashboard.selected.saturating_sub(1);
                    dashboard.selected_step = 0;
                    dashboard.detail_scroll = 0;
                    inspect_selected_workflow(dashboard, workflows);
                } else {
                    dashboard.selected_step = dashboard.selected_step.saturating_sub(1);
                    dashboard.detail_scroll = 0;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if dashboard.focus == WorkflowFocus::Runs {
                    dashboard.selected =
                        (dashboard.selected + 1).min(dashboard.runs.len().saturating_sub(1));
                    dashboard.selected_step = 0;
                    dashboard.detail_scroll = 0;
                    inspect_selected_workflow(dashboard, workflows);
                } else {
                    let last = dashboard
                        .snapshot
                        .as_ref()
                        .map_or(0, |snapshot| snapshot.steps.len().saturating_sub(1));
                    dashboard.selected_step = (dashboard.selected_step + 1).min(last);
                    dashboard.detail_scroll = 0;
                }
            }
            KeyCode::Tab => {
                dashboard.focus = if dashboard.focus == WorkflowFocus::Runs {
                    WorkflowFocus::Steps
                } else {
                    WorkflowFocus::Runs
                };
            }
            KeyCode::Right | KeyCode::Enter => dashboard.focus = WorkflowFocus::Steps,
            KeyCode::Left => dashboard.focus = WorkflowFocus::Runs,
            KeyCode::PageUp => dashboard.detail_scroll = dashboard.detail_scroll.saturating_sub(8),
            KeyCode::PageDown => {
                dashboard.detail_scroll = dashboard.detail_scroll.saturating_add(8)
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                refresh_workflow_dashboard(dashboard, workflows);
            }
            KeyCode::Char('r') if !app.running => {
                if let Some(run_id) = dashboard
                    .runs
                    .get(dashboard.selected)
                    .map(|run| run.id.clone())
                {
                    start_workflow_resume(app, workflows, sender, run_id);
                }
            }
            KeyCode::Char('n') if !app.running => {
                app.workflows = None;
                app.editor.clear();
                app.editor.insert("/workflow run ");
                app.status = "enter workflow path".into();
            }
            KeyCode::Char('c') if app.active_kind == Some(ActiveKind::Workflow) => {
                if let Some(cancellation) = &app.cancellation {
                    cancellation.cancel();
                }
                app.status = "workflow cancelling".into();
            }
            _ => {}
        }
        return;
    }
    if let Some(picker) = &mut app.sessions {
        match key.code {
            KeyCode::Esc => app.sessions = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => {
                picker.selected =
                    (picker.selected + 1).min(picker.sessions.len().saturating_sub(1));
            }
            KeyCode::Home => picker.selected = 0,
            KeyCode::End => picker.selected = picker.sessions.len().saturating_sub(1),
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.reset_session();
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                match store.list_sessions(100) {
                    Ok(sessions) => {
                        picker.sessions = sessions;
                        picker.selected = 0;
                        picker.error = None;
                    }
                    Err(error) => picker.error = Some(error.to_string()),
                }
            }
            KeyCode::Enter => {
                let id = picker
                    .sessions
                    .get(picker.selected)
                    .map(|session| session.id.clone());
                if let Some(id) = id {
                    match store.load_session(&id) {
                        Ok(Some(session)) => app.load_session(session),
                        Ok(None) => picker.error = Some("session no longer exists".into()),
                        Err(error) => picker.error = Some(error.to_string()),
                    }
                }
            }
            _ => {}
        }
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('w') {
        let mut dashboard = WorkflowDashboard {
            runs: Vec::new(),
            selected: 0,
            selected_step: 0,
            focus: WorkflowFocus::Runs,
            detail_scroll: 0,
            snapshot: None,
            active_run_id: None,
            error: None,
        };
        refresh_workflow_dashboard(&mut dashboard, workflows);
        app.workflows = Some(dashboard);
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('t') {
        let mut dashboard = SupervisorDashboard {
            runs: Vec::new(),
            selected: 0,
            snapshot: None,
            active_run_id: None,
            error: None,
        };
        refresh_supervisor_dashboard(&mut dashboard, supervisors);
        app.supervisors = Some(dashboard);
        return;
    }
    if !app.running
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('p')
    {
        open_model_picker(app);
        return;
    }
    if !app.running
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('l')
    {
        app.sessions = Some(match store.list_sessions(100) {
            Ok(sessions) => SessionPicker {
                sessions,
                selected: 0,
                error: None,
            },
            Err(error) => SessionPicker {
                sessions: Vec::new(),
                selected: 0,
                error: Some(error.to_string()),
            },
        });
        return;
    }
    if !app.running
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('n')
    {
        app.reset_session();
        return;
    }
    if !app.running
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && key.code == KeyCode::Char('s')
    {
        start_run(app, agent, workflows, supervisors, provider_factory, sender);
        return;
    }
    match key.code {
        KeyCode::Enter if !app.running && key.modifiers.contains(KeyModifiers::ALT) => {
            app.editor.insert("\n");
        }
        KeyCode::Enter if !app.running => {
            start_run(app, agent, workflows, supervisors, provider_factory, sender)
        }
        KeyCode::Enter if app.running && key.modifiers.contains(KeyModifiers::ALT) => {
            app.editor.insert("\n");
        }
        KeyCode::Enter if app.running => {
            let text = app.editor.take();
            let text = text.trim().to_string();
            if !text.is_empty() {
                app.transcript.push(format!("QUEUED {text}"));
                app.queued.push(text);
                app.status = format!("queued {} message(s)", app.queued.len());
            }
        }
        KeyCode::Esc if app.running => {
            if let Some(cancellation) = &app.cancellation {
                cancellation.cancel();
            }
            app.status = "cancelling".into();
        }
        KeyCode::Esc if app.editor.text.is_empty() => app.exit_requested = true,
        KeyCode::Esc => app.editor.clear(),
        KeyCode::Backspace
            if !app.running
                && (key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT)) =>
        {
            app.editor.delete_word_back();
        }
        KeyCode::Backspace => {
            app.editor.backspace();
            app.palette_selected = 0;
        }
        KeyCode::Delete => app.editor.delete(),
        KeyCode::Left
            if !app.running
                && (key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT)) =>
        {
            app.editor.move_word_left();
        }
        KeyCode::Left => app.editor.move_left(),
        KeyCode::Right
            if !app.running
                && (key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::ALT)) =>
        {
            app.editor.move_word_right();
        }
        KeyCode::Right => app.editor.move_right(),
        KeyCode::Home if !app.running => app.editor.home(),
        KeyCode::End if !app.running && key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.editor.cursor = app.editor.text.len();
        }
        KeyCode::End if !app.running => app.editor.end(),
        KeyCode::Tab if !app.running => app.editor.insert("\t"),
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.editor.home();
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.editor.end();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.editor.clear();
        }
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.editor.text.truncate(app.editor.cursor);
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.editor.insert(&character.to_string());
            app.palette_selected = 0;
        }
        KeyCode::Up if !app.running && app.palette_visible() => {
            if app.palette_selected > 0 {
                app.palette_selected -= 1;
            }
            let count = crate::tui_palette::filter_commands(&app.editor.text).len();
            crate::tui_palette::ensure_visible(
                app.palette_selected,
                &mut app.palette_scroll,
                count,
            );
        }
        KeyCode::Down if !app.running && app.palette_visible() => {
            let count = crate::tui_palette::filter_commands(&app.editor.text).len();
            if app.palette_selected + 1 < count {
                app.palette_selected += 1;
            }
            crate::tui_palette::ensure_visible(
                app.palette_selected,
                &mut app.palette_scroll,
                count,
            );
        }
        KeyCode::Up if !app.running && !app.history.is_empty() => {
            let next_index = match app.history_index {
                Some(0) => 0,
                Some(index) => index - 1,
                None => app.history.len() - 1,
            };
            app.history_index = Some(next_index);
            app.editor.text = app.history[next_index].clone();
            app.editor.cursor = app.editor.text.len();
        }
        KeyCode::Down if !app.running && app.history_index.is_some() => {
            let current = app.history_index.unwrap_or(0);
            if current + 1 >= app.history.len() {
                app.history_index = None;
                app.editor.clear();
            } else {
                app.history_index = Some(current + 1);
                app.editor.text = app.history[current + 1].clone();
                app.editor.cursor = app.editor.text.len();
            }
        }
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_sub(8);
            app.follow_bottom = false;
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_add(8);
        }
        KeyCode::End => {
            app.scroll = 0;
            app.follow_bottom = true;
        }
        _ => {}
    }
}

fn start_run(
    app: &mut App,
    agent: &Arc<Agent>,
    workflows: &Arc<WorkflowRuntime>,
    supervisors: &Arc<SupervisorRuntime>,
    provider_factory: &Arc<ProviderFactory>,
    sender: &mpsc::Sender<BackendEvent>,
) {
    if app.running || app.editor.text.trim().is_empty() {
        return;
    }
    let prompt = app.editor.take();
    if app.history.last() != Some(&prompt) {
        app.history.push(prompt.clone());
    }
    app.history_index = None;
    app.follow_bottom = true;
    match parse_tui_command(prompt) {
        TuiCommand::Workflows => {
            let mut dashboard = WorkflowDashboard {
                runs: Vec::new(),
                selected: 0,
                selected_step: 0,
                focus: WorkflowFocus::Runs,
                detail_scroll: 0,
                snapshot: None,
                active_run_id: None,
                error: None,
            };
            refresh_workflow_dashboard(&mut dashboard, workflows);
            app.workflows = Some(dashboard);
        }
        TuiCommand::WorkflowRun(path) => {
            start_workflow_file(app, workflows, sender, path);
        }
        TuiCommand::WorkflowResume(run_id) => {
            start_workflow_resume(app, workflows, sender, run_id);
        }
        TuiCommand::Models => open_model_picker(app),
        TuiCommand::Model(selector) => {
            let model = app
                .available_models
                .iter()
                .find(|model| model.selector() == selector)
                .cloned()
                .or_else(|| {
                    app.available_models
                        .iter()
                        .find(|model| model.selector().contains(selector.as_str()))
                        .cloned()
                });
            if let Some(model) = model {
                switch_model(app, agent, provider_factory, model);
            } else {
                app.status = format!("unknown model: {}", sanitize(&selector, 128));
            }
        }
        TuiCommand::Supervisors => {
            let mut dashboard = SupervisorDashboard {
                runs: Vec::new(),
                selected: 0,
                snapshot: None,
                active_run_id: None,
                error: None,
            };
            refresh_supervisor_dashboard(&mut dashboard, supervisors);
            app.supervisors = Some(dashboard);
        }
        TuiCommand::SupervisorRun(path) => {
            start_supervisor(app, supervisors, sender, path);
        }
        TuiCommand::Help => {
            app.show_help = true;
        }
        TuiCommand::Export => {
            let name = format!(
                "omni-session-{}.md",
                app.session_id.as_deref().unwrap_or("new")
            );
            let mut body = String::from("# omni session transcript\n\n");
            for line in &app.transcript {
                if let Some(content) = line.strip_prefix("OMNI ") {
                    body.push_str("**omni**\n\n");
                    body.push_str(content);
                } else if let Some(content) = line.strip_prefix("YOU  ") {
                    body.push_str("**you**\n\n");
                    body.push_str(content);
                } else if let Some(content) = line.strip_prefix("TOOL ") {
                    body.push_str(&format!("- ⚙ `{content}`"));
                } else {
                    body.push_str(line);
                }
                body.push_str("\n\n");
            }
            app.status = match std::fs::write(&name, body) {
                Ok(()) => format!("exported to {name}"),
                Err(error) => format!("export failed: {error}"),
            };
        }
        TuiCommand::Clear => {
            app.reset_session();
            app.status = "new session".into();
        }
        TuiCommand::Yolo => {
            app.full_access = !app.full_access;
            if app.full_access {
                app.transcript.push(
                    "⚠ full access ON — every tool call is auto-approved (/yolo to turn off)"
                        .into(),
                );
                app.status = "full access on".into();
            } else {
                app.transcript
                    .push("○ full access OFF — permission prompts are back".into());
                app.status = "full access off".into();
            }
        }
        TuiCommand::Retry => match app.last_prompt.clone() {
            Some(prompt) => start_agent_run(app, agent, sender, prompt),
            None => app.status = "nothing to retry yet".into(),
        },
        TuiCommand::Invalid(message) => {
            app.status = message.into();
        }
        TuiCommand::Agent(prompt) => start_agent_run(app, agent, sender, prompt),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum TuiCommand {
    Workflows,
    WorkflowRun(std::path::PathBuf),
    WorkflowResume(String),
    Models,
    Model(String),
    Supervisors,
    SupervisorRun(PathBuf),
    Help,
    Export,
    Clear,
    Yolo,
    Retry,
    Invalid(&'static str),
    Agent(String),
}

fn parse_tui_command(prompt: String) -> TuiCommand {
    let trimmed = prompt.trim();
    if trimmed == "/workflows" {
        return TuiCommand::Workflows;
    }
    if matches!(trimmed, "/models" | "/model") {
        return TuiCommand::Models;
    }
    if trimmed == "/supervisors" {
        return TuiCommand::Supervisors;
    }
    if matches!(trimmed, "/help" | "/keys") {
        return TuiCommand::Help;
    }
    if trimmed == "/export" {
        return TuiCommand::Export;
    }
    if matches!(trimmed, "/clear" | "/new") {
        return TuiCommand::Clear;
    }
    if matches!(trimmed, "/yolo" | "/full-access") {
        return TuiCommand::Yolo;
    }
    if trimmed == "/retry" {
        return TuiCommand::Retry;
    }
    if let Some(path) = trimmed.strip_prefix("/supervisor run ") {
        return if path.trim().is_empty() {
            TuiCommand::Invalid("supervisor task file is required")
        } else {
            TuiCommand::SupervisorRun(path.trim().into())
        };
    }
    if let Some(selector) = trimmed.strip_prefix("/model ") {
        return if selector.trim().is_empty() {
            TuiCommand::Invalid("model selector is required")
        } else {
            TuiCommand::Model(selector.trim().into())
        };
    }
    if let Some(path) = trimmed.strip_prefix("/workflow run") {
        return if path.trim().is_empty() {
            TuiCommand::Invalid("workflow path is required")
        } else {
            TuiCommand::WorkflowRun(path.trim().into())
        };
    }
    if let Some(run_id) = trimmed.strip_prefix("/workflow resume") {
        return if run_id.trim().is_empty() {
            TuiCommand::Invalid("workflow run id is required")
        } else {
            TuiCommand::WorkflowResume(run_id.trim().into())
        };
    }
    TuiCommand::Agent(prompt)
}

fn deduplicate_models(models: Vec<ModelSpec>) -> Vec<ModelSpec> {
    let mut selectors = std::collections::HashSet::new();
    models
        .into_iter()
        .filter(|model| selectors.insert(model.selector()))
        .collect()
}

fn open_model_picker(app: &mut App) {
    let selected = app
        .available_models
        .iter()
        .position(|model| model == &app.active_model)
        .unwrap_or(0);
    app.models = Some(ModelPicker {
        models: app.available_models.clone(),
        selected,
        error: None,
    });
}

fn switch_model(app: &mut App, agent: &Agent, factory: &ProviderFactory, spec: ModelSpec) {
    if app.running {
        app.status = "model cannot be switched while busy".into();
        return;
    }
    if spec == app.active_model {
        app.models = None;
        app.status = "idle".into();
        return;
    }
    match agent.try_switch_provider(|| factory.build(&spec)) {
        Ok(()) => {
            app.provider = spec.selector();
            app.active_model = spec;
            app.models = None;
            app.status = "model switched".into();
            app.transcript.push(format!("MODEL {}", app.provider));
            if std::fs::create_dir_all(&app.data_dir).is_ok() {
                let _ = std::fs::write(app.data_dir.join("last_model"), &app.provider);
            }
        }
        Err(error) => {
            app.status = "model switch failed".into();
            if let Some(picker) = &mut app.models {
                picker.error = Some(sanitize(&error.to_string(), 512));
            } else {
                app.transcript
                    .push(format!("MODEL ERROR {}", sanitize(&error.to_string(), 512)));
            }
        }
    }
}

fn start_agent_run(
    app: &mut App,
    agent: &Arc<Agent>,
    sender: &mpsc::Sender<BackendEvent>,
    prompt: String,
) {
    let cancellation = CancellationToken::new();
    app.cancellation = Some(cancellation.clone());
    app.running = true;
    app.run_started = Some(std::time::Instant::now());
    app.active_kind = Some(ActiveKind::Agent);
    app.status = "running".into();
    app.last_prompt = Some(prompt.clone());
    app.scroll = 0;
    app.follow_bottom = true;
    let request = RunRequest {
        prompt,
        session_id: app.session_id.clone(),
        verify: app.verify,
        system_prompt: Some(crate::agent::DEFAULT_AGENT_SYSTEM_PROMPT.to_string()),
    };
    let agent = agent.clone();
    let sender = sender.clone();
    tokio::spawn(async move {
        let sink = Arc::new(ChannelSink {
            sender: sender.clone(),
        });
        let result = agent
            .run(request, sink, cancellation)
            .await
            .map_err(|error| error.to_string());
        let _ = sender.send(BackendEvent::Complete(result)).await;
    });
}

fn refresh_workflow_dashboard(dashboard: &mut WorkflowDashboard, runtime: &WorkflowRuntime) {
    let selected_id = dashboard
        .runs
        .get(dashboard.selected)
        .map(|run| run.id.clone());
    match runtime.list_runs(100) {
        Ok(runs) => {
            dashboard.runs = runs;
            dashboard.selected = selected_id
                .and_then(|id| dashboard.runs.iter().position(|run| run.id == id))
                .unwrap_or(0)
                .min(dashboard.runs.len().saturating_sub(1));
            dashboard.error = None;
            inspect_selected_workflow(dashboard, runtime);
        }
        Err(error) => dashboard.error = Some(error.to_string()),
    }
}

fn inspect_selected_workflow(dashboard: &mut WorkflowDashboard, runtime: &WorkflowRuntime) {
    let Some(run_id) = dashboard
        .runs
        .get(dashboard.selected)
        .map(|run| run.id.clone())
    else {
        dashboard.snapshot = None;
        return;
    };
    match runtime.inspect_run(&run_id) {
        Ok(snapshot) => {
            dashboard.snapshot = Some(snapshot);
            dashboard.error = None;
        }
        Err(error) => dashboard.error = Some(error.to_string()),
    }
}

fn ensure_workflow_dashboard(app: &mut App, runtime: &WorkflowRuntime) {
    if app.workflows.is_none() {
        let mut dashboard = WorkflowDashboard {
            runs: Vec::new(),
            selected: 0,
            selected_step: 0,
            focus: WorkflowFocus::Runs,
            detail_scroll: 0,
            snapshot: None,
            active_run_id: None,
            error: None,
        };
        refresh_workflow_dashboard(&mut dashboard, runtime);
        app.workflows = Some(dashboard);
    }
}

fn start_workflow_file(
    app: &mut App,
    runtime: &Arc<WorkflowRuntime>,
    sender: &mpsc::Sender<BackendEvent>,
    file: std::path::PathBuf,
) {
    ensure_workflow_dashboard(app, runtime);
    let cancellation = CancellationToken::new();
    app.cancellation = Some(cancellation.clone());
    app.running = true;
    app.run_started = Some(std::time::Instant::now());
    app.active_kind = Some(ActiveKind::Workflow);
    app.status = "workflow preparing".into();
    let runtime = runtime.clone();
    let sender = sender.clone();
    tokio::spawn(async move {
        match runtime.prepare_start(file, 4).await {
            Ok(prepared) => {
                execute_prepared_workflow(runtime, prepared, cancellation, sender).await;
            }
            Err(error) => {
                let _ = sender
                    .send(BackendEvent::WorkflowComplete(Err(error.to_string())))
                    .await;
            }
        }
    });
}

fn start_workflow_resume(
    app: &mut App,
    runtime: &Arc<WorkflowRuntime>,
    sender: &mpsc::Sender<BackendEvent>,
    run_id: String,
) {
    ensure_workflow_dashboard(app, runtime);
    let cancellation = CancellationToken::new();
    app.cancellation = Some(cancellation.clone());
    app.running = true;
    app.run_started = Some(std::time::Instant::now());
    app.active_kind = Some(ActiveKind::Workflow);
    app.status = "workflow preparing".into();
    let runtime = runtime.clone();
    let sender = sender.clone();
    tokio::spawn(async move {
        match runtime.prepare_resume(&run_id, 4).await {
            Ok(prepared) => {
                execute_prepared_workflow(runtime, prepared, cancellation, sender).await;
            }
            Err(error) => {
                let _ = sender
                    .send(BackendEvent::WorkflowComplete(Err(error.to_string())))
                    .await;
            }
        }
    });
}

async fn execute_prepared_workflow(
    runtime: Arc<WorkflowRuntime>,
    prepared: crate::PreparedWorkflow,
    cancellation: CancellationToken,
    sender: mpsc::Sender<BackendEvent>,
) {
    let run_id = prepared.run_id().to_string();
    let _ = sender
        .send(BackendEvent::WorkflowStarted(run_id.clone()))
        .await;
    let mut execution = Box::pin(prepared.execute(cancellation));
    let mut poll = tokio::time::interval(Duration::from_millis(250));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut execution => {
                let _ = sender.send(BackendEvent::WorkflowSnapshot(
                    runtime.inspect_run(&run_id).map_err(|error| error.to_string())
                )).await;
                let _ = sender.send(BackendEvent::WorkflowComplete(
                    result.map_err(|error| error.to_string())
                )).await;
                break;
            }
            _ = poll.tick() => {
                let snapshot = runtime.inspect_run(&run_id).map_err(|error| error.to_string());
                if sender.send(BackendEvent::WorkflowSnapshot(snapshot)).await.is_err() {
                    break;
                }
            }
        }
    }
}

fn refresh_supervisor_dashboard(dashboard: &mut SupervisorDashboard, runtime: &SupervisorRuntime) {
    match runtime.list_runs(100) {
        Ok(runs) => {
            dashboard.runs = runs;
            dashboard.selected = dashboard
                .selected
                .min(dashboard.runs.len().saturating_sub(1));
            dashboard.error = None;
            inspect_selected_supervisor(dashboard, runtime);
        }
        Err(error) => dashboard.error = Some(error.to_string()),
    }
}

fn inspect_selected_supervisor(dashboard: &mut SupervisorDashboard, runtime: &SupervisorRuntime) {
    let Some(id) = dashboard
        .runs
        .get(dashboard.selected)
        .map(|run| run.id.clone())
    else {
        dashboard.snapshot = None;
        return;
    };
    match runtime.inspect_run(&id) {
        Ok(run) => dashboard.snapshot = Some(run),
        Err(error) => dashboard.error = Some(error.to_string()),
    }
}

fn start_supervisor(
    app: &mut App,
    runtime: &Arc<SupervisorRuntime>,
    sender: &mpsc::Sender<BackendEvent>,
    file: PathBuf,
) {
    if app.supervisors.is_none() {
        let mut dashboard = SupervisorDashboard {
            runs: Vec::new(),
            selected: 0,
            snapshot: None,
            active_run_id: None,
            error: None,
        };
        refresh_supervisor_dashboard(&mut dashboard, runtime);
        app.supervisors = Some(dashboard);
    }
    let cancellation = CancellationToken::new();
    app.cancellation = Some(cancellation.clone());
    app.running = true;
    app.run_started = Some(std::time::Instant::now());
    app.active_kind = Some(ActiveKind::Supervisor);
    app.status = "supervisor preparing".into();
    let runtime = runtime.clone();
    let sender = sender.clone();
    let model = app.active_model.clone();
    tokio::spawn(async move {
        match runtime.prepare_start(file, 4, model).await {
            Ok(prepared) => {
                let run_id = prepared.run_id().to_string();
                let _ = sender
                    .send(BackendEvent::SupervisorStarted(run_id.clone()))
                    .await;
                let mut execution = Box::pin(prepared.execute(cancellation));
                let mut poll = tokio::time::interval(Duration::from_millis(250));
                loop {
                    tokio::select! {
                        result = &mut execution => {
                            let _ = sender.send(BackendEvent::SupervisorSnapshot(
                                runtime.inspect_run(&run_id).map_err(|error| error.to_string())
                            )).await;
                            let _ = sender.send(BackendEvent::SupervisorComplete(
                                result.map_err(|error| error.to_string())
                            )).await;
                            break;
                        }
                        _ = poll.tick() => {
                            if sender.send(BackendEvent::SupervisorSnapshot(
                                runtime.inspect_run(&run_id).map_err(|error| error.to_string())
                            )).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            Err(error) => {
                let _ = sender
                    .send(BackendEvent::SupervisorComplete(Err(error.to_string())))
                    .await;
            }
        }
    });
}

fn render(frame: &mut Frame<'_>, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(5),
            Constraint::Length(1),
        ])
        .split(area);
    let session = app.session_id.as_deref().unwrap_or("new");
    let session = if area.width < 80 {
        session.chars().take(8).collect::<String>()
    } else {
        session.into()
    };
    let status_style = match app.status.as_str() {
        status if status.contains("error") => Style::default().fg(theme::ERROR),
        status if status.contains("running") => Style::default().fg(theme::WARNING),
        "complete" => Style::default().fg(theme::SUCCESS),
        _ => Style::default().fg(theme::TEXT_MUTED),
    };
    let status_dot = match app.status.as_str() {
        status if status.contains("error") => "✖",
        status if status.contains("running") => spinner_glyph(),
        "complete" => "●",
        _ => "○",
    };
    let mut header_spans = vec![
        Span::styled(
            concat!(" omni v", env!("CARGO_PKG_VERSION"), " "),
            Style::default()
                .fg(theme::BG)
                .bg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            app.provider.clone(),
            Style::default()
                .fg(theme::ACCENT_ALT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  │  ", theme::dim()),
        Span::styled("session ", theme::dim()),
        Span::styled(session, theme::muted()),
        Span::styled("  │  ", theme::dim()),
        Span::styled(format!("{status_dot} "), status_style),
        Span::styled(
            app.status.clone(),
            status_style.add_modifier(Modifier::BOLD),
        ),
    ];
    if app.full_access {
        header_spans.push(Span::styled("  │  ", theme::dim()));
        header_spans.push(Span::styled(
            " ⚡ FULL ACCESS ",
            Style::default()
                .fg(theme::BG)
                .bg(theme::WARNING)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.usage.total_tokens > 0 {
        header_spans.push(Span::styled("  │  ", theme::dim()));
        header_spans.push(Span::styled(
            format!(
                "tokens {} in · {} out · {} total",
                app.usage.input_tokens, app.usage.output_tokens, app.usage.total_tokens
            ),
            theme::dim(),
        ));
    }
    if app.running
        && let Some(started) = app.run_started
    {
        let secs = started.elapsed().as_secs();
        header_spans.push(Span::styled("  │  ", theme::dim()));
        header_spans.push(Span::styled(
            format!("⏱ {:02}:{:02}", secs / 60, secs % 60),
            theme::warning(),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(header_spans)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::BORDER)),
        ),
        chunks[0],
    );
    let mut lines = Vec::new();
    if !app.running && app.transcript.len() <= 1 {
        lines.extend(crate::tui_banner::welcome_lines_at(
            area.width,
            banner_phase(),
        ));
        lines.push(Line::from(vec![
            Span::styled("  model ", theme::dim()),
            Span::styled(app.provider.clone(), theme::accent_bold()),
            Span::styled("   ·   full access ", theme::dim()),
            Span::styled(
                if app.full_access { "on" } else { "off" },
                if app.full_access {
                    theme::warning()
                } else {
                    theme::muted()
                },
            ),
            Span::styled("   ·   /yolo toggles", theme::dim()),
        ]));
        lines.push(Line::default());
    }
    for line in &app.transcript {
        if let Some(content) = line.strip_prefix("OMNI ") {
            lines.push(transcript_label("omni", theme::ACCENT));
            lines.extend(crate::tui_markdown::render_markdown(content, area.width));
            lines.push(Line::default());
        } else if let Some(content) = line.strip_prefix("YOU  ") {
            if !lines.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  ····",
                    Style::default().fg(theme::TEXT_DIM),
                )));
                lines.push(Line::default());
            }
            lines.push(transcript_label("you", theme::ACCENT_ALT));
            lines.push(Line::from(Span::styled(
                content.to_string(),
                Style::default().fg(theme::TEXT),
            )));
            lines.push(Line::default());
        } else if let Some(content) = line.strip_prefix("TOOL ") {
            lines.push(Line::from(vec![
                Span::styled("  ⚙ ", theme::dim()),
                Span::styled(content.to_string(), theme::muted()),
            ]));
        } else if let Some(content) = line.strip_prefix("✦ ") {
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled("  ◆ ", theme::accent_bold()),
                Span::styled(
                    "omni",
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" — provider-neutral agentic runtime", theme::muted()),
            ]));
            lines.push(Line::from(Span::styled(
                format!("    {content}"),
                theme::dim(),
            )));
            lines.push(Line::from(vec![
                Span::styled("    tip ", theme::warning()),
                Span::styled(
                    "try \"read README.md\" or press ^P to switch models",
                    theme::dim(),
                ),
            ]));
            lines.push(Line::default());
        } else if let Some(content) = line.strip_prefix("QUEUED ") {
            lines.push(Line::from(vec![
                Span::styled("  ▸ queued ", theme::warning()),
                Span::styled(content.to_string(), theme::muted()),
            ]));
        } else if let Some(content) = line.strip_prefix("ERROR ") {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "  ✖ error",
                Style::default()
                    .fg(theme::ERROR)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                format!("    {content}"),
                theme::error(),
            )));
            lines.push(Line::from(vec![
                Span::styled("    hint ", theme::warning()),
                Span::styled(
                    "run `omni doctor`, /model to switch provider, /clear for a fresh session",
                    theme::dim(),
                ),
            ]));
            lines.push(Line::default());
        } else if line.contains("ERROR") {
            lines.push(Line::from(Span::styled(line.clone(), theme::error())));
        } else if line.starts_with('⚠') {
            lines.push(Line::from(Span::styled(line.clone(), theme::warning())));
        } else {
            lines.push(Line::from(Span::styled(line.clone(), theme::dim())));
        }
    }
    if !app.streaming.is_empty() {
        lines.push(transcript_label(
            "omni",
            fade_color(theme::ACCENT, app.stream_started),
        ));
        lines.extend(crate::tui_markdown::render_markdown(
            &app.streaming,
            area.width,
        ));
        lines.push(Line::from(Span::styled(
            if blink_on() { "▍" } else { " " },
            theme::accent(),
        )));
    }
    if app.running && app.streaming.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {} ", spinner_glyph()),
                Style::default()
                    .fg(spinner_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(app.status.clone(), theme::muted()),
            Span::styled(thinking_dots(), theme::dim()),
        ]));
        lines.push(progress_bar_line(area.width));
    }
    let (conversation_area, tool_area) = if area.width >= 96 && !app.tools.order.is_empty() {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(chunks[1]);
        (body[0], Some(body[1]))
    } else if !app.tools.order.is_empty() && chunks[1].height >= 10 {
        let body = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(6)])
            .split(chunks[1]);
        (body[0], Some(body[1]))
    } else {
        (chunks[1], None)
    };
    let visible_height = conversation_area.height.saturating_sub(2);
    let max_scroll = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_sub(visible_height);
    let effective_scroll = if app.follow_bottom {
        max_scroll
    } else {
        app.scroll.min(max_scroll)
    };
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(theme::panel("Conversation"))
            .wrap(Wrap { trim: false })
            .scroll((effective_scroll, 0)),
        conversation_area,
    );
    if max_scroll > 0 {
        let mut scrollbar_state =
            ScrollbarState::new(max_scroll as usize).position(effective_scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .thumb_symbol("┃")
                .track_style(Style::default().fg(theme::BORDER))
                .thumb_style(Style::default().fg(theme::ACCENT)),
            conversation_area.inner(Margin::new(0, 1)),
            &mut scrollbar_state,
        );
    }
    if let Some(tool_area) = tool_area {
        let tool_lines = app
            .tools
            .order
            .iter()
            .filter_map(|id| app.tools.items.get(id))
            .map(|item| {
                let (glyph, glyph_style) = tool_status_glyph(item.status);
                let name_style = if matches!(item.status, ToolStatus::Running) {
                    Style::default()
                        .fg(spinner_color())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(theme::TEXT)
                        .add_modifier(Modifier::BOLD)
                };
                Line::from(vec![
                    Span::styled(format!("{glyph} "), glyph_style),
                    Span::styled(format!("{:<9} ", item.name), name_style),
                    Span::styled(
                        format!(
                            "{} {}",
                            sanitize(&item.arguments, 80),
                            sanitize(&item.summary, 80)
                        ),
                        theme::dim(),
                    ),
                ])
            })
            .collect::<Vec<_>>();
        let active = app
            .tools
            .items
            .values()
            .filter(|item| {
                matches!(
                    item.status,
                    ToolStatus::Requested | ToolStatus::Awaiting | ToolStatus::Running
                )
            })
            .count();
        let tools_title = if active > 0 {
            format!("Tools · {active} active")
        } else {
            format!("Tools · {}", app.tools.order.len())
        };
        let tools_block = if active > 0 {
            theme::panel_border(&tools_title, spinner_color())
        } else {
            theme::panel(&tools_title)
        };
        frame.render_widget(
            Paragraph::new(tool_lines)
                .block(tools_block)
                .wrap(Wrap { trim: true }),
            tool_area,
        );
    }
    let prompt_paragraph = if app.editor.text.is_empty() && !app.running {
        Paragraph::new(Line::from(vec![
            Span::styled("▏", theme::accent()),
            Span::styled(
                "Describe a task… (Alt+↵ for a new line)",
                theme::dim().add_modifier(Modifier::ITALIC),
            ),
        ]))
    } else if app.editor.text.is_empty() && app.running {
        Paragraph::new(Line::from(vec![
            Span::styled("▏", theme::accent()),
            Span::styled(
                "Type your next message — ↵ queues it while omni works",
                theme::dim().add_modifier(Modifier::ITALIC),
            ),
        ]))
    } else {
        Paragraph::new(app.editor.display_with_cursor())
    };
    frame.render_widget(
        prompt_paragraph.wrap(Wrap { trim: false }).block({
            let title = if app.editor.text.is_empty() {
                "Prompt".to_string()
            } else {
                format!("Prompt — {} chars", app.editor.text.chars().count())
            };
            if app.running {
                theme::panel(&title)
            } else {
                theme::panel_accent(&title)
            }
        }),
        chunks[2],
    );
    frame.render_widget(
        Paragraph::new(Line::from(key_hints(&[
            ("↵", "send · queue"),
            ("↑", "history"),
            ("^P", "models"),
            ("^L", "sessions"),
            ("^W", "workflows"),
            ("^T", "tasks"),
            ("F1", "help"),
        ])))
        .alignment(Alignment::Center),
        chunks[3],
    );
    if let Some(dashboard) = &app.supervisors {
        let modal = centered_rect(90, 75, area);
        frame.render_widget(Clear, modal);
        let panels = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(modal);
        let runs = if dashboard.runs.is_empty() {
            vec![Line::styled("No supervisor runs", theme::dim())]
        } else {
            dashboard
                .runs
                .iter()
                .enumerate()
                .map(|(index, run)| {
                    let line = format!(
                        "{} {} {:<10} {}",
                        if index == dashboard.selected {
                            ">"
                        } else {
                            " "
                        },
                        run.id.chars().take(8).collect::<String>(),
                        run.status,
                        sanitize(&run.task_file, 36)
                    );
                    if index == dashboard.selected {
                        Line::styled(line, theme::selection())
                    } else {
                        Line::from(line)
                    }
                })
                .collect()
        };
        frame.render_widget(
            Paragraph::new(runs).block(theme::panel_accent("Agent batches")),
            panels[0],
        );
        let mut tasks = dashboard
            .snapshot
            .as_ref()
            .map(|run| {
                let mut lines = vec![Line::styled(
                    format!("{}  model={}", run.id, run.model),
                    Style::default().add_modifier(Modifier::BOLD),
                )];
                lines.extend(run.tasks.iter().map(|task| {
                    Line::from(format!(
                        "{:<10} {:<16} {:<12} session={}{}",
                        task.status,
                        task.id,
                        task.worktree,
                        task.session_id.chars().take(8).collect::<String>(),
                        task.error
                            .as_ref()
                            .map(|error| format!("  {}", sanitize(error, 120)))
                            .unwrap_or_default()
                    ))
                }));
                lines
            })
            .unwrap_or_else(|| {
                vec![Line::styled(
                    "Select a batch to inspect tasks",
                    theme::dim(),
                )]
            });
        if let Some(error) = &dashboard.error {
            tasks.push(Line::styled(sanitize(error, 512), theme::error()));
        }
        tasks.push(Line::styled(
            "Up/Down select  Enter session  c cancel  Ctrl+R refresh  Esc close",
            theme::dim(),
        ));
        frame.render_widget(
            Paragraph::new(tasks)
                .block(theme::panel_accent("Parallel tasks"))
                .wrap(Wrap { trim: false }),
            panels[1],
        );
    }
    if let Some(dashboard) = &app.workflows {
        let modal = centered_rect(92, 82, area);
        frame.render_widget(Clear, modal);
        let panels = if modal.width >= 80 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(43), Constraint::Percentage(57)])
                .split(modal)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
                .split(modal)
        };
        let mut run_lines = dashboard
            .runs
            .iter()
            .enumerate()
            .map(|(index, run)| {
                let live = dashboard.active_run_id.as_deref() == Some(run.id.as_str());
                let line = format!(
                    "{} {} {:<10} {}{}",
                    if index == dashboard.selected {
                        ">"
                    } else {
                        " "
                    },
                    run.id.chars().take(8).collect::<String>(),
                    run.status,
                    sanitize(&run.workflow_path, 42),
                    if live { " LIVE" } else { "" },
                );
                if index == dashboard.selected {
                    Line::styled(line, theme::selection())
                } else {
                    Line::from(line)
                }
            })
            .collect::<Vec<_>>();
        if run_lines.is_empty() {
            run_lines.push(Line::styled("No persisted workflow runs", theme::dim()));
        }
        frame.render_widget(
            Paragraph::new(run_lines)
                .block(theme::panel_accent("Workflow runs"))
                .wrap(Wrap { trim: true }),
            panels[0],
        );
        let mut step_lines = dashboard
            .snapshot
            .as_ref()
            .map(|snapshot| {
                let mut lines = vec![Line::styled(
                    format!("{}  {}", snapshot.id, sanitize(&snapshot.workflow_path, 80)),
                    Style::default().add_modifier(Modifier::BOLD),
                )];
                lines.extend(snapshot.steps.iter().enumerate().map(|(index, step)| {
                    let needs = if step.needs.is_empty() {
                        "root".into()
                    } else {
                        format!("needs={}", step.needs.join(","))
                    };
                    let line = format!(
                        "{:<10} {:<18} {:<14} attempts={} {}",
                        step.status,
                        step.step_id,
                        step.tool.as_deref().unwrap_or("unknown"),
                        step.attempts,
                        needs,
                    );
                    if index == dashboard.selected_step {
                        Line::styled(
                            line,
                            if dashboard.focus == WorkflowFocus::Steps {
                                theme::selection()
                            } else {
                                theme::accent()
                            },
                        )
                    } else {
                        Line::from(line)
                    }
                }));
                if let Some(step) = snapshot.steps.get(dashboard.selected_step) {
                    lines.push(Line::from(""));
                    lines.extend(workflow_step_detail_lines(step));
                }
                lines
            })
            .unwrap_or_else(|| {
                vec![Line::styled(
                    "Select a run to inspect its DAG",
                    theme::dim(),
                )]
            });
        if let Some(error) = &dashboard.error {
            step_lines.push(Line::styled(sanitize(error, 512), theme::error()));
        }
        step_lines.push(Line::styled(
            "Up/Down select  r resume  n run file  c cancel  Ctrl+R refresh  Esc close",
            theme::dim(),
        ));
        frame.render_widget(
            Paragraph::new(step_lines)
                .block(theme::panel_accent("Live DAG"))
                .wrap(Wrap { trim: false })
                .scroll((dashboard.detail_scroll, 0)),
            panels[1],
        );
    }
    if let Some(picker) = &app.sessions {
        let modal = centered_rect(80, 70, area);
        frame.render_widget(Clear, modal);
        let mut session_lines = picker
            .sessions
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let line = format!(
                    "{} {}  {}",
                    if index == picker.selected { ">" } else { " " },
                    session.id.chars().take(8).collect::<String>(),
                    sanitize(&session.title, 80)
                );
                if index == picker.selected {
                    Line::styled(line, theme::selection())
                } else {
                    Line::from(line)
                }
            })
            .collect::<Vec<_>>();
        if let Some(error) = &picker.error {
            session_lines.push(Line::styled(sanitize(error, 256), theme::error()));
        }
        if session_lines.is_empty() {
            session_lines.push(Line::styled("No saved sessions", theme::dim()));
        }
        frame.render_widget(
            Paragraph::new(session_lines)
                .block(theme::panel_accent("Sessions"))
                .wrap(Wrap { trim: true }),
            modal,
        );
    }
    if let Some(picker) = &app.models {
        let modal = centered_rect(65, 45, area);
        frame.render_widget(Clear, modal);
        let mut lines = picker
            .models
            .iter()
            .enumerate()
            .map(|(index, model)| {
                let selector = model.selector();
                let active = model == &app.active_model;
                let line = format!(
                    "{} {}{}",
                    if index == picker.selected { ">" } else { " " },
                    selector,
                    if active { "  [active]" } else { "" },
                );
                if index == picker.selected {
                    Line::styled(line, theme::selection())
                } else {
                    Line::from(line)
                }
            })
            .collect::<Vec<_>>();
        if let Some(error) = &picker.error {
            lines.push(Line::from(""));
            lines.push(Line::styled(sanitize(error, 512), theme::error()));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(theme::panel_accent("Models"))
                .wrap(Wrap { trim: true }),
            modal,
        );
    }
    if let Some(prompt) = &app.permission {
        let modal = centered_rect(75, 40, area);
        frame.render_widget(Clear, modal);
        let arguments = sanitize(&prompt.request.arguments.to_string(), 1024);
        frame.render_widget(
            Paragraph::new(format!(
                "Tool: {}\nReason: {}\nArguments: {}\n\nAllow once? [y/n]",
                prompt.request.tool,
                sanitize(&prompt.reason, 256),
                arguments
            ))
            .wrap(Wrap { trim: false })
            .block(theme::panel_warning("Permission required")),
            modal,
        );
    }
    if !app.running && app.permission.is_none() && app.editor.text.trim_start().starts_with('/') {
        crate::tui_palette::render_palette(
            frame,
            chunks[2],
            &app.editor.text,
            app.palette_selected,
            app.palette_scroll,
        );
    }
    if app.show_help {
        crate::tui_palette::render_help(frame, area);
    }
}

fn workflow_step_detail_lines(step: &crate::store::StoredWorkflowStep) -> Vec<Line<'static>> {
    let mut lines = vec![Line::styled(
        format!("Step {} details", step.step_id),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if !step.artifacts.is_empty() {
        lines.push(Line::from("Declared artifacts:"));
        lines.extend(
            step.artifacts
                .iter()
                .map(|path| Line::from(format!("  {}", sanitize(path, 512)))),
        );
    }
    let Some(report_json) = &step.report_json else {
        lines.push(Line::from("Output is not available yet."));
        return lines;
    };
    let report = match serde_json::from_str::<StepReport>(report_json) {
        Ok(report) => report,
        Err(error) => {
            lines.push(Line::styled(
                format!(
                    "Stored report is invalid: {}",
                    sanitize(&error.to_string(), 512)
                ),
                theme::error(),
            ));
            return lines;
        }
    };
    if let Some(error) = report.error {
        lines.push(Line::styled(
            format!("{}: {}", error.code, sanitize(&error.message, 2048)),
            theme::error(),
        ));
    }
    if !report.artifacts.is_empty() {
        lines.push(Line::from("Captured artifacts:"));
        for artifact in report.artifacts {
            lines.push(Line::from(format!(
                "  {}  bytes={}  sha256={}",
                sanitize(&artifact.path, 512),
                artifact.size_bytes,
                artifact.sha256,
            )));
        }
    }
    if let Some(output) = report.output {
        if !output.stdout.is_empty() {
            lines.push(Line::from("stdout:"));
            lines.push(Line::from(sanitize(&output.stdout, 16 * 1024)));
        }
        if !output.stderr.is_empty() {
            lines.push(Line::from("stderr:"));
            lines.push(Line::styled(
                sanitize(&output.stderr, 16 * 1024),
                theme::error(),
            ));
        }
        lines.push(Line::from(format!(
            "metadata: {}{}",
            sanitize(&output.metadata.to_string(), 8 * 1024),
            if output.truncated { " (truncated)" } else { "" },
        )));
    }
    lines
}

fn tool_status_glyph(status: ToolStatus) -> (&'static str, Style) {
    match status {
        ToolStatus::Requested => ("◌", Style::default().fg(theme::TEXT_DIM)),
        ToolStatus::Awaiting => ("◔", Style::default().fg(theme::WARNING)),
        ToolStatus::Running => (
            spinner_glyph(),
            Style::default()
                .fg(spinner_color())
                .add_modifier(Modifier::BOLD),
        ),
        ToolStatus::Succeeded => ("●", Style::default().fg(theme::SUCCESS)),
        ToolStatus::Failed => ("✖", Style::default().fg(theme::ERROR)),
        ToolStatus::Interrupted => ("◒", Style::default().fg(theme::WARNING)),
    }
}

fn transcript_label(name: &str, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled("▍ ", Style::default().fg(color)),
        Span::styled(
            name.to_string(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ])
}

fn key_hints(pairs: &[(&str, &str)]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, (key, label)) in pairs.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ·  ", theme::dim()));
        }
        spans.push(Span::styled((*key).to_string(), theme::hint_key()));
        spans.push(Span::styled(format!(" {label}"), theme::hint_label()));
    }
    spans
}

/// Whether the streaming cursor should be visible in the current blink phase.
fn blink_on() -> bool {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    (millis / 530).is_multiple_of(2)
}

/// Braille spinner frame derived from wall-clock time, so it animates on
/// every redraw tick without extra state.
fn spinner_glyph() -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    FRAMES[((millis / 80) as usize) % FRAMES.len()]
}

/// Wall-clock phase in 0.0..=1.0 that scrolls the brand gradient across the
/// logo and other "alive" accents once every ~4 seconds.
fn banner_phase() -> f32 {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    (millis % 4000) as f32 / 4000.0
}

/// Gradient-cycled color for the running spinner, so activity feels lively.
fn spinner_color() -> ratatui::style::Color {
    theme::gradient(banner_phase())
}

/// Indeterminate gradient progress bar that sweeps left-to-right. It does not
/// track real progress — it exists to make the running state feel premium and
/// alive, using the same wall-clock phase as the logo and spinner.
fn progress_bar_line(width: u16) -> Line<'static> {
    let cells = (width.saturating_sub(6)).clamp(12, 48) as usize;
    let phase = banner_phase();
    let mut spans = Vec::with_capacity(cells + 1);
    spans.push(Span::raw("  "));
    for index in 0..cells {
        let position = index as f32 / cells as f32;
        // Distance behind the moving head (wrapped) controls the glyph weight.
        let trail = (phase - position).rem_euclid(1.0);
        let glyph = if trail < 0.16 {
            "█"
        } else if trail < 0.32 {
            "▓"
        } else if trail < 0.5 {
            "▒"
        } else {
            "░"
        };
        let color = theme::gradient((position + phase).rem_euclid(1.0));
        spans.push(Span::styled(glyph, Style::default().fg(color)));
    }
    Line::from(spans)
}

/// Animated ellipsis for the live working indicator.
fn thinking_dots() -> &'static str {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    match (millis / 400) % 4 {
        0 => "",
        1 => ".",
        2 => "..",
        _ => "...",
    }
}

/// Fade a color in from the dim tone over ~450ms so freshly streamed assistant
/// text eases into view instead of popping in abruptly.
fn fade_color(target: Color, since: Option<std::time::Instant>) -> Color {
    let progress = since
        .map(|start| start.elapsed().as_millis() as f32 / 450.0)
        .unwrap_or(1.0);
    theme::lerp(theme::TEXT_DIM, target, progress)
}

fn centered_rect(horizontal: u16, vertical: u16, area: Rect) -> Rect {
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - vertical) / 2),
            Constraint::Percentage(vertical),
            Constraint::Percentage((100 - vertical) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - horizontal) / 2),
            Constraint::Percentage(horizontal),
            Constraint::Percentage((100 - horizontal) / 2),
        ])
        .split(vertical_chunks[1])[1]
}

fn sanitize(value: &str, limit: usize) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if output.len() >= limit {
            output.push_str("...");
            break;
        }
        match character {
            '\n' | '\r' | '\t' => output.push(character),
            character if character.is_control() => output.push(' '),
            character => output.push(character),
        }
    }
    output
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>, TuiError> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        default_hook(info);
    }));
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
        Hide
    ) {
        let _ = disable_raw_mode();
        return Err(error.into());
    }
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal() -> Result<(), TuiError> {
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        Show,
        LeaveAlternateScreen
    )?;
    Ok(())
}

struct RestoreGuard {
    armed: bool,
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = restore_terminal();
        }
    }
}

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("TUI requires interactive stdin and stdout")]
    NotTerminal,
    #[error("terminal input stream closed")]
    InputClosed,
    #[error("TUI backend channel closed")]
    BackendClosed,
    #[error("terminal I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArtifactMetadata,
        permission::PermissionRequest,
        protocol::{Message, ToolCall},
        store::StoredWorkflowStep,
        workflow::{StepStatus, WorkflowFailure},
    };

    #[test]
    fn sanitizer_removes_terminal_controls() {
        assert_eq!(sanitize("safe\u{1b}[31mred", 100), "safe [31mred");
    }

    #[tokio::test]
    async fn hard_path_denials_never_reach_prompt_channel() {
        let policy = Policy::new("workspace".into(), false, false, false);
        let (authorizer, mut receiver) = permission_bridge(policy);
        let request = AuthorizationRequest {
            call_id: "1".into(),
            tool: "read_file".into(),
            arguments: serde_json::json!({"path": "../secret"}),
            permission: PermissionRequest::FileRead {
                path: "../secret".into(),
            },
        };
        let decision = authorizer.authorize(&request).await;
        assert!(!decision.allowed);
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn promptable_permissions_resolve_once() {
        let policy = Policy::new("workspace".into(), false, false, false);
        let (authorizer, mut receiver) = permission_bridge(policy);
        let request = AuthorizationRequest {
            call_id: "1".into(),
            tool: "shell".into(),
            arguments: serde_json::json!({"command": "echo test"}),
            permission: PermissionRequest::Shell {
                command: "echo test".into(),
            },
        };
        let task = tokio::spawn(async move { authorizer.authorize(&request).await });
        let prompt = receiver.recv().await.unwrap();
        prompt.reply.send(true).unwrap();
        assert!(task.await.unwrap().allowed);
    }

    #[test]
    fn editor_moves_and_deletes_complete_graphemes() {
        let mut editor = Editor::default();
        editor.insert("e\u{301}👨‍👩‍👧‍👦x");
        editor.move_left();
        editor.backspace();
        assert_eq!(editor.text, "e\u{301}x");
        editor.backspace();
        assert_eq!(editor.text, "x");
    }

    #[test]
    fn paste_normalizes_newlines_and_strips_controls() {
        let mut editor = Editor::default();
        editor.insert("first\r\nsecond\rthird\u{1b}[31m");
        assert_eq!(editor.text, "first\nsecond\nthird[31m");
        assert_eq!(editor.cursor, editor.text.len());
    }

    #[test]
    fn timeline_updates_tools_by_call_id() {
        let mut timeline = ToolTimeline::default();
        timeline.requested("one".into(), "read_file".into(), "a".into());
        timeline.requested("two".into(), "git_status".into(), "{}".into());
        timeline.finished(
            "one".into(),
            ToolOutput {
                success: true,
                stdout: "done".into(),
                stderr: String::new(),
                truncated: false,
                metadata: serde_json::json!({}),
            },
        );
        assert_eq!(timeline.items["one"].status, ToolStatus::Succeeded);
        assert_eq!(timeline.items["two"].status, ToolStatus::Requested);
    }

    #[test]
    fn historical_session_reconstructs_conversation_and_tools() {
        let call = ToolCall {
            id: "call".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "README.md"}),
        };
        let output = ToolOutput {
            success: true,
            stdout: "content".into(),
            stderr: String::new(),
            truncated: false,
            metadata: serde_json::json!({}),
        };
        let mut app = App::new(TuiOptions {
            data_dir: std::path::PathBuf::new(),
            session_id: None,
            verify: false,
            full_access: false,
            initial_model: ModelSpec::Fake,
            available_models: vec![ModelSpec::Fake],
        });
        app.load_session(StoredSession {
            summary: SessionSummary {
                id: "session".into(),
                title: "title".into(),
                updated_at_ms: 0,
            },
            messages: vec![
                Message::new(Role::User, "hello"),
                Message::assistant_with_tool_calls("", vec![call]),
                Message::tool("call", serde_json::to_string(&output).unwrap()),
                Message::new(Role::Assistant, "finished"),
            ],
        });
        assert!(
            app.transcript
                .iter()
                .any(|line| line.contains("YOU  hello"))
        );
        assert!(
            app.transcript
                .iter()
                .any(|line| line.contains("OMNI finished"))
        );
        assert_eq!(app.tools.items["call"].status, ToolStatus::Succeeded);
    }

    #[test]
    fn responsive_renderer_handles_narrow_and_wide_terminals() {
        use ratatui::backend::TestBackend;

        for (width, height) in [(50, 14), (120, 30)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut app = App::new(TuiOptions {
                data_dir: std::path::PathBuf::new(),
                session_id: None,
                verify: false,
                full_access: false,
                initial_model: ModelSpec::Fake,
                available_models: vec![ModelSpec::Fake],
            });
            app.tools
                .requested("one".into(), "read_file".into(), "{}".into());
            app.workflows = Some(WorkflowDashboard {
                runs: vec![WorkflowRunSummary {
                    id: "run-id".into(),
                    workflow_path: "workflow.yml".into(),
                    status: "running".into(),
                    lease_expires_at_ms: None,
                    created_at_ms: 0,
                    updated_at_ms: 0,
                }],
                selected: 0,
                selected_step: 0,
                focus: WorkflowFocus::Runs,
                detail_scroll: 0,
                snapshot: None,
                active_run_id: Some("run-id".into()),
                error: None,
            });
            app.models = Some(ModelPicker {
                models: vec![ModelSpec::Fake],
                selected: 0,
                error: None,
            });
            terminal.draw(|frame| render(frame, &app)).unwrap();
        }
    }

    #[test]
    fn tui_commands_route_workflows_without_model_prompts() {
        assert_eq!(
            parse_tui_command("/workflows".into()),
            TuiCommand::Workflows
        );
        assert_eq!(
            parse_tui_command("/workflow run examples/inspect.yml".into()),
            TuiCommand::WorkflowRun("examples/inspect.yml".into())
        );
        assert_eq!(
            parse_tui_command("/workflow resume run-id".into()),
            TuiCommand::WorkflowResume("run-id".into())
        );
        assert_eq!(
            parse_tui_command("ordinary task".into()),
            TuiCommand::Agent("ordinary task".into())
        );
        assert_eq!(parse_tui_command("/models".into()), TuiCommand::Models);
        assert_eq!(parse_tui_command("/model".into()), TuiCommand::Models);
        assert_eq!(parse_tui_command("/help".into()), TuiCommand::Help);
        assert_eq!(parse_tui_command("/keys".into()), TuiCommand::Help);
        assert_eq!(parse_tui_command("/export".into()), TuiCommand::Export);
        assert_eq!(parse_tui_command("/clear".into()), TuiCommand::Clear);
        assert_eq!(parse_tui_command("/new".into()), TuiCommand::Clear);
        assert_eq!(parse_tui_command("/yolo".into()), TuiCommand::Yolo);
        assert_eq!(parse_tui_command("/full-access".into()), TuiCommand::Yolo);
        assert_eq!(parse_tui_command("/retry".into()), TuiCommand::Retry);
        assert_eq!(
            parse_tui_command("/model openai/test".into()),
            TuiCommand::Model("openai/test".into())
        );
        assert_eq!(
            parse_tui_command("/modelish task".into()),
            TuiCommand::Agent("/modelish task".into())
        );
        assert_eq!(
            parse_tui_command("/supervisors".into()),
            TuiCommand::Supervisors
        );
        assert_eq!(
            parse_tui_command("/supervisor run tasks.yml".into()),
            TuiCommand::SupervisorRun("tasks.yml".into())
        );
    }

    #[test]
    fn workflow_step_detail_renders_artifacts_and_output() {
        let report = StepReport {
            id: "package".into(),
            tool: "read_file".into(),
            status: StepStatus::Succeeded,
            attempts: 1,
            output: Some(ToolOutput {
                success: true,
                stdout: "built".into(),
                stderr: String::new(),
                truncated: false,
                metadata: serde_json::json!({"exit_code": 0}),
            }),
            blocked_by: Vec::new(),
            artifacts: vec![ArtifactMetadata {
                path: "target/app".into(),
                size_bytes: 42,
                sha256: "a".repeat(64),
            }],
            error: None,
        };
        let step = StoredWorkflowStep {
            step_id: "package".into(),
            ordinal: 0,
            tool: Some("read_file".into()),
            needs: vec!["build".into()],
            artifacts: vec!["target/app".into()],
            status: "succeeded".into(),
            attempts: 1,
            report_json: Some(serde_json::to_string(&report).unwrap()),
        };
        let rendered = workflow_step_detail_lines(&step)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Declared artifacts"));
        assert!(rendered.contains("Captured artifacts"));
        assert!(rendered.contains(&"a".repeat(64)));
        assert!(rendered.contains("built"));

        let failed = StepReport {
            error: Some(WorkflowFailure {
                code: "artifact_missing".into(),
                message: "missing".into(),
            }),
            ..report
        };
        assert!(
            serde_json::to_string(&failed)
                .unwrap()
                .contains("artifact_missing")
        );
    }

    #[test]
    fn failed_model_construction_preserves_active_provider() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(&directory.path().join("state.db")).unwrap());
        let agent = Agent::new(
            Arc::new(crate::FakeProvider),
            crate::ToolRegistry::standard(),
            Policy::new(directory.path().to_path_buf(), false, false, false),
            store,
            crate::agent::default_tool_context(directory.path().to_path_buf(), 1024, 1024, 5),
            2,
        );
        let openai = ModelSpec::OpenAi {
            base_url: "https://example.invalid/v1/".into(),
            model: "test-model".into(),
            timeout: Duration::from_secs(5),
        };
        let mut app = App::new(TuiOptions {
            data_dir: std::path::PathBuf::new(),
            session_id: None,
            verify: false,
            full_access: false,
            initial_model: ModelSpec::Fake,
            available_models: vec![ModelSpec::Fake, openai.clone()],
        });
        open_model_picker(&mut app);
        switch_model(
            &mut app,
            &agent,
            &ProviderFactory::new(None),
            openai.clone(),
        );
        assert_eq!(app.active_model, ModelSpec::Fake);
        assert!(app.models.as_ref().unwrap().error.is_some());

        switch_model(
            &mut app,
            &agent,
            &ProviderFactory::new(Some("test-key".into())),
            openai.clone(),
        );
        assert_eq!(app.active_model, openai);
        assert!(app.models.is_none());
    }
}

#[cfg(test)]
mod editor_word_tests {
    use super::Editor;

    fn editor_with(text: &str) -> Editor {
        Editor {
            text: text.to_string(),
            cursor: text.len(),
        }
    }

    #[test]
    fn word_left_stops_at_word_start() {
        let mut editor = editor_with("hello world");
        editor.move_word_left();
        assert_eq!(editor.cursor, 6);
        editor.move_word_left();
        assert_eq!(editor.cursor, 0);
    }

    #[test]
    fn word_right_moves_past_word() {
        let mut editor = editor_with("one two");
        editor.cursor = 0;
        editor.move_word_right();
        assert_eq!(editor.cursor, 3);
        editor.move_word_right();
        assert_eq!(editor.cursor, 7);
    }

    #[test]
    fn delete_word_back_removes_previous_word() {
        let mut editor = editor_with("cargo build");
        editor.delete_word_back();
        assert_eq!(editor.text, "cargo ");
        assert_eq!(editor.cursor, 6);
    }

    #[test]
    fn delete_word_back_on_empty_is_noop() {
        let mut editor = editor_with("");
        editor.delete_word_back();
        assert_eq!(editor.text, "");
        assert_eq!(editor.cursor, 0);
    }
}
