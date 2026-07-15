use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use thiserror::Error;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    events::{EventError, EventSink, RunEvent, RunEventKind},
    permission::{AuthorizationRequest, PermissionAuthorizer, Policy},
    protocol::{Message, ModelEvent, ModelRequest, Role, ToolCall, ToolOutput},
    provider::{ModelProvider, ProviderError},
    store::{SqliteStore, StoreError},
    tools::{ToolContext, ToolError, ToolRegistry},
};

pub struct Agent {
    provider: RwLock<Arc<dyn ModelProvider>>,
    tools: ToolRegistry,
    authorizer: Arc<dyn PermissionAuthorizer>,
    store: Arc<SqliteStore>,
    context: ToolContext,
    max_turns: u32,
}

pub struct RunRequest {
    pub prompt: String,
    pub session_id: Option<String>,
    pub verify: bool,
    pub system_prompt: Option<String>,
}

/// The default, full-capability operating charter handed to the agent for
/// interactive `run` and TUI sessions. It teaches the model to reach for every
/// tool it has — shell (including native Windows commands), the whole file
/// system, web search/fetch/download — and to close the loop by verifying its
/// own work and fixing what it broke before declaring success.
pub const DEFAULT_AGENT_SYSTEM_PROMPT: &str = concat!(
    "You are omni, an elite autonomous software and systems agent running in a real terminal on the user's computer.\n",
    "You are resourceful, precise, and relentless: you finish the whole task, not just the first step.\n",
    "\n",
    "## Capabilities — use them proactively\n",
    "- Shell: run any command on the host through the `shell` tool. On Windows these are cmd/PowerShell commands (dir, copy, del, robocopy, tasklist, winget, powershell -Command ...); on Unix they are sh commands. Chain commands, inspect the environment, install dependencies, run builds and programs.\n",
    "- Files: read, create, and patch any file in the workspace with `read_file`, `create_file`, and `apply_patch`. Use `repo_map`, `search_code`, and `search_files` to understand a project before editing.\n",
    "- Internet: use `web_search` to find information, `web_fetch` to read a page as text, and `web_download` to save files (installers, datasets, assets, dependencies) from the internet directly into the workspace.\n",
    "- Git & checks: inspect state with `git_status` and `git_diff`, and run the project's real test/lint suite with `run_checks`. It returns a structured `failures[]` array (file/line/column/message) parsed from Rust, Node, and Python output — jump straight to those locations to fix problems. Pass `{\"filter\": \"name\"}` to rerun just one failing test and `{\"ecosystem\": \"rust|node|python\"}` to focus one stack while iterating.\n",
    "- Code intelligence: use `goto_definition`, `find_references`, `hover`, and `workspace_symbols` to navigate code semantically, and `rename_symbol` for safe project-wide renames via the language server.\n",
    "- External integrations: tools named `mcp__<server>__<tool>` come from connected MCP servers and `plugin__<name>__<tool>` from installed plugins. Treat them as first-class capabilities — when a task maps to an available integration (issue trackers, databases, browsers, cloud APIs, etc.), prefer that dedicated tool over improvising through the shell.\n",
    "\n",
    "## Operating loop — always close the loop\n",
    "1. Plan briefly, then act with tools instead of guessing.\n",
    "2. After any change, VERIFY it: run `run_checks`, execute the program, or re-read the file to confirm the edit landed.\n",
    "3. If verification fails, diagnose the error from the output, FIX it, and verify again. Repeat until it passes or you have exhausted reasonable options.\n",
    "4. Prefer real evidence (command output, file contents, HTTP status) over assumptions. Never claim success you have not checked.\n",
    "\n",
    "## Style\n",
    "- Be concise and concrete. Show the key commands you ran and what the results proved.\n",
    "- Respect the workspace sandbox and the permission prompts; if a capability is denied, explain what you need and why.\n",
    "- When a task is ambiguous, make the most reasonable assumption, state it, and proceed.",
);

pub struct RunOutcome {
    pub session_id: String,
    pub response: String,
}

impl Agent {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        tools: ToolRegistry,
        policy: Policy,
        store: Arc<SqliteStore>,
        context: ToolContext,
        max_turns: u32,
    ) -> Self {
        Self::with_authorizer(provider, tools, Arc::new(policy), store, context, max_turns)
    }

    pub fn with_authorizer(
        provider: Arc<dyn ModelProvider>,
        tools: ToolRegistry,
        authorizer: Arc<dyn PermissionAuthorizer>,
        store: Arc<SqliteStore>,
        context: ToolContext,
        max_turns: u32,
    ) -> Self {
        Self {
            provider: RwLock::new(provider),
            tools,
            authorizer,
            store,
            context,
            max_turns,
        }
    }

    pub fn try_switch_provider<F>(&self, build: F) -> Result<(), ProviderSwitchError>
    where
        F: FnOnce() -> Result<Arc<dyn ModelProvider>, ProviderError>,
    {
        let mut provider = self
            .provider
            .try_write()
            .map_err(|_| ProviderSwitchError::Busy)?;
        let replacement = build()?;
        *provider = replacement;
        Ok(())
    }

    pub async fn run(
        &self,
        request: RunRequest,
        sink: Arc<dyn EventSink>,
        cancellation: CancellationToken,
    ) -> Result<RunOutcome, AgentError> {
        let provider = self.provider.read().await;
        let verify = request.verify;
        let session_id = match request.session_id {
            Some(id) if self.store.session_exists(&id)? => id,
            Some(id) => return Err(AgentError::UnknownSession(id)),
            None => self
                .store
                .create_session(&title(&request.prompt), provider.name())?,
        };
        let run_id = Uuid::now_v7().to_string();
        let mut sequence = 0;
        self.emit(
            &sink,
            &session_id,
            &run_id,
            &mut sequence,
            RunEventKind::RunStarted,
        )
        .await?;

        if let Some(mut system_prompt) = request.system_prompt {
            let workspace = self.context.workspace.clone();
            let max_file_bytes = self.context.max_file_bytes as u64;
            let map = match tokio::task::spawn_blocking(move || {
                crate::repomap::build_repo_map(&workspace, max_file_bytes)
            })
            .await
            {
                Ok(Ok(map)) => map,
                _ => Default::default(),
            };
            if !map.is_empty() {
                system_prompt.push_str("\n\n## Repository map (definitions overview)\n");
                system_prompt.push_str(&crate::repomap::render_map(&map, 8192));
                system_prompt
                    .push_str("\nUse read_file for full contents, search_code to find symbols.");
            }
            let system = Message::new(Role::System, system_prompt);
            self.store.append_message(&session_id, &system)?;
            self.emit(
                &sink,
                &session_id,
                &run_id,
                &mut sequence,
                RunEventKind::SystemMessage { message: system },
            )
            .await?;
        }

        let user = Message::new(Role::User, request.prompt);
        self.store.append_message(&session_id, &user)?;
        self.emit(
            &sink,
            &session_id,
            &run_id,
            &mut sequence,
            RunEventKind::UserMessage { message: user },
        )
        .await?;

        let mut workspace_changed = false;
        let mut last_validation_failure = None;
        let mut fix_attempt = 0u32;
        for _ in 0..self.max_turns {
            let model_request = ModelRequest {
                messages: self.store.load_messages(&session_id)?,
                tools: self.tools.specs(),
                response_schema: None,
            };
            let mut events = tokio::select! {
                _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
                response = provider.respond(model_request, cancellation.clone()) => response?,
            };
            let mut turn_text = String::new();
            let mut tool_calls = Vec::new();
            let mut finished = false;
            loop {
                let event = tokio::select! {
                    _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
                    event = events.next() => event,
                };
                let Some(event) = event else { break };
                match event {
                    Ok(ModelEvent::TextDelta(text)) => {
                        turn_text.push_str(&text);
                        self.emit(
                            &sink,
                            &session_id,
                            &run_id,
                            &mut sequence,
                            RunEventKind::ModelTextDelta { text },
                        )
                        .await?;
                    }
                    Ok(ModelEvent::ToolCall(call)) => tool_calls.push(call),
                    Ok(ModelEvent::Usage(event_usage)) => {
                        self.emit(
                            &sink,
                            &session_id,
                            &run_id,
                            &mut sequence,
                            RunEventKind::Usage { usage: event_usage },
                        )
                        .await?;
                    }
                    Ok(ModelEvent::Finished) => finished = true,
                    Err(error) => return Err(error.into()),
                }
            }
            if !finished {
                return Err(AgentError::IncompleteModelResponse);
            }

            if tool_calls.is_empty() {
                let assistant = Message::new(Role::Assistant, turn_text.clone());
                self.store.append_message(&session_id, &assistant)?;
                self.emit(
                    &sink,
                    &session_id,
                    &run_id,
                    &mut sequence,
                    RunEventKind::AssistantMessage { message: assistant },
                )
                .await?;
                if verify && workspace_changed {
                    let validation = self
                        .run_validation(&sink, &session_id, &run_id, &mut sequence, &cancellation)
                        .await?;
                    if validation.metadata["code"] == "no_checks_detected" {
                        return Err(AgentError::NoChecksDetected);
                    }
                    if !validation.success {
                        fix_attempt += 1;
                        let remaining = validation
                            .metadata
                            .get("failure_count")
                            .and_then(|value| value.as_u64());
                        let progress = match remaining {
                            Some(count) => {
                                format!(
                                    " (fix attempt {fix_attempt}; {count} failing check(s) remaining)"
                                )
                            }
                            None => format!(" (fix attempt {fix_attempt})"),
                        };
                        self.emit(
                            &sink,
                            &session_id,
                            &run_id,
                            &mut sequence,
                            RunEventKind::ModelTextDelta {
                                text: format!("\n[auto-fix]{progress}\n"),
                            },
                        )
                        .await?;
                        let feedback = format!(
                            "Automated verification failed{progress}. Read the structured `failures[]` below, jump to each file:line, and make the smallest safe repair using available tools. Then let verification re-run; finish only when every check passes.\n\n{}",
                            serde_json::to_string(&validation)?
                        );
                        last_validation_failure = Some(validation.stderr.clone());
                        let message = Message::new(Role::User, feedback);
                        self.store.append_message(&session_id, &message)?;
                        self.emit(
                            &sink,
                            &session_id,
                            &run_id,
                            &mut sequence,
                            RunEventKind::UserMessage { message },
                        )
                        .await?;
                        continue;
                    }
                }
                self.emit(
                    &sink,
                    &session_id,
                    &run_id,
                    &mut sequence,
                    RunEventKind::RunFinished,
                )
                .await?;
                return Ok(RunOutcome {
                    session_id,
                    response: turn_text,
                });
            }

            self.store.append_message(
                &session_id,
                &Message::assistant_with_tool_calls(turn_text, tool_calls.clone()),
            )?;
            let mut fatal_error = None;
            let mut cancelled = cancellation.is_cancelled();
            for call in tool_calls {
                self.emit(
                    &sink,
                    &session_id,
                    &run_id,
                    &mut sequence,
                    RunEventKind::ToolCallRequested { call: call.clone() },
                )
                .await?;
                let output = if cancelled {
                    failed_tool_output("cancelled", "run was cancelled")
                } else if let Some(tool) = self.tools.get(&call.name) {
                    if let Some(preview) = tool.preview(&call.arguments, &self.context) {
                        self.emit(
                            &sink,
                            &session_id,
                            &run_id,
                            &mut sequence,
                            RunEventKind::DiffPreview {
                                path: preview.path,
                                summary: preview.summary,
                                diff: preview.diff,
                            },
                        )
                        .await?;
                    }
                    match tool.permission_request(&call.arguments) {
                        Ok(permission) => {
                            let authorization = AuthorizationRequest {
                                call_id: call.id.clone(),
                                tool: call.name.clone(),
                                arguments: call.arguments.clone(),
                                permission,
                            };
                            let decision = tokio::select! {
                                _ = cancellation.cancelled() => {
                                    cancelled = true;
                                    None
                                }
                                decision = self.authorizer.authorize(&authorization) => Some(decision),
                            };
                            match decision {
                                None => failed_tool_output("cancelled", "run was cancelled"),
                                Some(decision) => {
                                    self.emit(
                                        &sink,
                                        &session_id,
                                        &run_id,
                                        &mut sequence,
                                        RunEventKind::PermissionResolved {
                                            allowed: decision.allowed,
                                            reason: decision.reason.clone(),
                                        },
                                    )
                                    .await?;
                                    if !decision.allowed {
                                        fatal_error.get_or_insert_with(|| {
                                            AgentError::PermissionDenied(decision.reason.clone())
                                        });
                                        failed_tool_output("permission_denied", &decision.reason)
                                    } else {
                                        tokio::select! {
                                            _ = cancellation.cancelled() => {
                                                cancelled = true;
                                                failed_tool_output("cancelled", "run was cancelled")
                                            }
                                            result = tool.execute(call.arguments, &self.context) => {
                                                match result {
                                                    Ok(output) => output,
                                                    Err(error) => {
                                                        let message = error.to_string();
                                                        fatal_error.get_or_insert_with(|| AgentError::ToolExecution(message.clone()));
                                                        failed_tool_output("tool_error", &message)
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            let message = error.to_string();
                            fatal_error
                                .get_or_insert_with(|| AgentError::ToolExecution(message.clone()));
                            failed_tool_output("invalid_arguments", &message)
                        }
                    }
                } else {
                    fatal_error.get_or_insert_with(|| AgentError::UnknownTool(call.name.clone()));
                    failed_tool_output("unknown_tool", &format!("unknown tool: {}", call.name))
                };
                if output.success && output.metadata["changed"] == true {
                    workspace_changed = true;
                }
                self.emit(
                    &sink,
                    &session_id,
                    &run_id,
                    &mut sequence,
                    RunEventKind::ToolFinished {
                        call_id: call.id.clone(),
                        output: output.clone(),
                    },
                )
                .await?;
                self.store.append_message(
                    &session_id,
                    &Message::tool(call.id, serde_json::to_string(&output)?),
                )?;
            }
            if cancelled {
                return Err(AgentError::Cancelled);
            }
            if let Some(error) = fatal_error {
                return Err(error);
            }
        }
        if let Some(failure) = last_validation_failure {
            return Err(AgentError::VerificationFailed(failure));
        }
        Err(AgentError::MaxTurns(self.max_turns))
    }

    async fn run_validation(
        &self,
        sink: &Arc<dyn EventSink>,
        session_id: &str,
        run_id: &str,
        sequence: &mut u64,
        cancellation: &CancellationToken,
    ) -> Result<ToolOutput, AgentError> {
        let call = ToolCall {
            id: Uuid::now_v7().to_string(),
            name: "run_checks".into(),
            arguments: serde_json::json!({}),
        };
        self.emit(
            sink,
            session_id,
            run_id,
            sequence,
            RunEventKind::ToolCallRequested { call: call.clone() },
        )
        .await?;
        let tool = self
            .tools
            .get("run_checks")
            .ok_or_else(|| AgentError::UnknownTool("run_checks".into()))?;
        let permission = tool.permission_request(&call.arguments)?;
        let authorization = AuthorizationRequest {
            call_id: call.id.clone(),
            tool: call.name.clone(),
            arguments: call.arguments.clone(),
            permission,
        };
        let decision = tokio::select! {
            _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
            decision = self.authorizer.authorize(&authorization) => decision,
        };
        self.emit(
            sink,
            session_id,
            run_id,
            sequence,
            RunEventKind::PermissionResolved {
                allowed: decision.allowed,
                reason: decision.reason.clone(),
            },
        )
        .await?;
        if !decision.allowed {
            return Err(AgentError::PermissionDenied(decision.reason));
        }
        let output = tokio::select! {
            _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
            output = tool.execute(call.arguments, &self.context) => output?,
        };
        self.emit(
            sink,
            session_id,
            run_id,
            sequence,
            RunEventKind::ToolFinished {
                call_id: call.id,
                output: output.clone(),
            },
        )
        .await?;
        Ok(output)
    }

    async fn emit(
        &self,
        sink: &Arc<dyn EventSink>,
        session_id: &str,
        run_id: &str,
        sequence: &mut u64,
        kind: RunEventKind,
    ) -> Result<(), AgentError> {
        let event = RunEvent::new(session_id, run_id, *sequence, kind);
        self.store.append_event(&event)?;
        sink.emit(&event).await?;
        *sequence += 1;
        Ok(())
    }
}

pub fn default_tool_context(
    workspace: std::path::PathBuf,
    max_output_bytes: usize,
    max_file_bytes: usize,
    shell_timeout_seconds: u64,
) -> ToolContext {
    ToolContext {
        workspace,
        max_output_bytes,
        max_file_bytes,
        shell_timeout: Duration::from_secs(shell_timeout_seconds),
    }
}

fn title(prompt: &str) -> String {
    let title = prompt.chars().take(80).collect::<String>();
    if title.is_empty() {
        "Untitled session".into()
    } else {
        title
    }
}

fn failed_tool_output(code: &str, message: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        stdout: String::new(),
        stderr: message.into(),
        truncated: false,
        metadata: serde_json::json!({"code": code, "changed": false}),
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("unknown session: {0}")]
    UnknownSession(String),
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("agent exceeded maximum turns ({0})")]
    MaxTurns(u32),
    #[error("run was cancelled")]
    Cancelled,
    #[error("model stream ended without a finished event")]
    IncompleteModelResponse,
    #[error("no supported project checks were detected")]
    NoChecksDetected,
    #[error("automated verification did not pass: {0}")]
    VerificationFailed(String),
    #[error("tool execution failed: {0}")]
    ToolExecution(String),
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Event(#[from] EventError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum ProviderSwitchError {
    #[error("model cannot be switched while an agent run is active")]
    Busy,
    #[error(transparent)]
    Construction(#[from] ProviderError),
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use async_trait::async_trait;
    use futures_util::stream;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tokio::sync::Notify;

    use super::*;
    use crate::{events::RecordingSink, protocol::ToolCall, provider::ModelStream};

    struct ScriptedProvider {
        turns: Mutex<VecDeque<Vec<ModelEvent>>>,
    }

    struct BlockingProvider {
        started: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for BlockingProvider {
        fn name(&self) -> &'static str {
            "blocking"
        }

        async fn respond(
            &self,
            _request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> Result<ModelStream, ProviderError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(Box::pin(stream::iter(
                text_turn("done").into_iter().map(Ok),
            )))
        }
    }

    #[async_trait]
    impl ModelProvider for ScriptedProvider {
        fn name(&self) -> &'static str {
            "scripted"
        }

        async fn respond(
            &self,
            _request: ModelRequest,
            _cancellation: CancellationToken,
        ) -> Result<ModelStream, ProviderError> {
            let events = self
                .turns
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted turn");
            Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
        }
    }

    fn tool_turn(id: &str, name: &str, arguments: Value) -> Vec<ModelEvent> {
        vec![
            ModelEvent::ToolCall(ToolCall {
                id: id.into(),
                name: name.into(),
                arguments,
            }),
            ModelEvent::Finished,
        ]
    }

    fn text_turn(text: &str) -> Vec<ModelEvent> {
        vec![ModelEvent::TextDelta(text.into()), ModelEvent::Finished]
    }

    #[tokio::test]
    async fn failed_validation_is_repaired_and_rechecked() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("src")).unwrap();
        std::fs::write(
            directory.path().join("Cargo.toml"),
            "[package]\nname='repair_fixture'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        let broken = "#[test]\nfn generated_test() { assert!(false); }\n";
        let broken_hash = hex::encode(Sha256::digest(broken.as_bytes()));
        let provider = ScriptedProvider {
            turns: Mutex::new(VecDeque::from([
                tool_turn(
                    "create",
                    "create_file",
                    json!({"path": "src/lib.rs", "content": broken}),
                ),
                text_turn("candidate"),
                tool_turn(
                    "repair",
                    "apply_patch",
                    json!({
                        "path": "src/lib.rs",
                        "expected_sha256": broken_hash,
                        "old_text": "assert!(false)",
                        "new_text": "assert!(true)"
                    }),
                ),
                text_turn("repaired"),
            ])),
        };
        let store = Arc::new(SqliteStore::open(&directory.path().join("sessions.db")).unwrap());
        let agent = Agent::new(
            Arc::new(provider),
            ToolRegistry::standard(),
            Policy::new(directory.path().to_path_buf(), true, false, true),
            store.clone(),
            default_tool_context(directory.path().to_path_buf(), 64 * 1024, 1024 * 1024, 30),
            8,
        );
        let sink = Arc::new(RecordingSink::default());

        let outcome = agent
            .run(
                RunRequest {
                    prompt: "make the project pass".into(),
                    session_id: None,
                    verify: true,
                    system_prompt: None,
                },
                sink.clone(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(outcome.response, "repaired");
        assert!(
            std::fs::read_to_string(directory.path().join("src/lib.rs"))
                .unwrap()
                .contains("assert!(true)")
        );
        let messages = store.load_messages(&outcome.session_id).unwrap();
        assert!(messages.iter().any(|message| {
            message.role == Role::User && message.content.contains("Automated verification failed")
        }));
        let validation_results = sink
            .events()
            .into_iter()
            .filter(|event| {
                matches!(
                    &event.kind,
                    RunEventKind::ToolFinished { output, .. }
                        if matches!(output.metadata["code"].as_str(), Some("checks_failed" | "checks_passed"))
                )
            })
            .count();
        assert_eq!(validation_results, 2);
    }

    #[tokio::test]
    async fn provider_switch_is_rejected_during_a_run_and_succeeds_afterward() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open(&directory.path().join("sessions.db")).unwrap());
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let agent = Arc::new(Agent::new(
            Arc::new(BlockingProvider {
                started: started.clone(),
                release: release.clone(),
            }),
            ToolRegistry::standard(),
            Policy::new(directory.path().to_path_buf(), false, false, false),
            store,
            default_tool_context(directory.path().to_path_buf(), 1024, 1024, 5),
            2,
        ));
        let running_agent = agent.clone();
        let task = tokio::spawn(async move {
            running_agent
                .run(
                    RunRequest {
                        prompt: "wait".into(),
                        session_id: None,
                        verify: false,
                        system_prompt: None,
                    },
                    Arc::new(RecordingSink::default()),
                    CancellationToken::new(),
                )
                .await
        });
        started.notified().await;
        assert!(matches!(
            agent.try_switch_provider(|| Ok(Arc::new(crate::provider::FakeProvider))),
            Err(ProviderSwitchError::Busy)
        ));
        release.notify_one();
        task.await.unwrap().unwrap();
        agent
            .try_switch_provider(|| Ok(Arc::new(crate::provider::FakeProvider)))
            .unwrap();
    }
}
