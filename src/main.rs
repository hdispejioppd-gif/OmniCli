use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    sync::Arc,
};

use async_trait::async_trait;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use omnicli::tui::{TuiOptions, permission_bridge, run_tui};
use omnicli::{
    Agent, AnthropicProvider, AppConfig, ContextEngine, EventSink, FakeProvider, LlamaCppProvider,
    McpServer, McpServerOptions, ModelProvider, ModelSpec, OllamaProvider, OpenAiProvider,
    PluginListEntry, PluginRegistry, Policy, ProviderFactory, ProviderKind, RunEvent, RunEventKind,
    RunRequest, SqliteStore, SupervisorRuntime, ToolRegistry, WorkflowRuntime, WorktreeManager,
    WorktreeState, agent::default_tool_context, events::EventError, register_configured_tools,
    register_worktree_tools,
};

use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(name = "omni", version, about = "Provider-neutral agentic CLI runtime")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[arg(long, global = true)]
    workspace: Option<PathBuf>,
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true, value_parser = ["fake", "openai", "anthropic", "ollama", "lm-studio", "llama-cpp", "openai-compatible"])]
    provider: Option<String>,
    #[arg(long, global = true)]
    model: Option<String>,
    #[arg(long, global = true)]
    base_url: Option<String>,
    #[arg(long, global = true)]
    worktree: Option<String>,
    #[arg(long, global = true)]
    profile: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run {
        prompt: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        allow_write: bool,
        #[arg(long)]
        allow_shell: bool,
        #[arg(long)]
        verify: bool,
        #[arg(long)]
        allow_worktree: bool,
        #[arg(long)]
        allow_mcp_start: bool,
        #[arg(long)]
        allow_mcp_call: bool,
        #[arg(long)]
        allow_plugins: bool,
    },
    Ask {
        prompt: String,
        #[arg(long)]
        session: Option<String>,
    },
    Plan {
        prompt: String,
        #[arg(long)]
        session: Option<String>,
    },
    Review {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        verify: bool,
    },
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    Worktree {
        #[command(subcommand)]
        command: WorktreeCommand,
    },
    Supervisor {
        #[command(subcommand)]
        command: SupervisorCommand,
    },
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    Tui {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        allow_write: bool,
        #[arg(long)]
        allow_shell: bool,
        #[arg(long)]
        verify: bool,
        #[arg(long)]
        allow_worktree: bool,
        #[arg(long)]
        allow_mcp_start: bool,
        #[arg(long)]
        allow_mcp_call: bool,
        #[arg(long)]
        allow_plugins: bool,
    },
    Sessions {
        #[command(subcommand)]
        command: SessionCommand,
    },
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    Models,
    Tools,
    Plugins {
        #[command(subcommand)]
        command: PluginCommand,
    },
    Completions {
        shell: String,
    },
    Doctor,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    List {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    Show {
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum ContextCommand {
    Index,
    Query {
        prompt: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    Status,
    Map,
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
    List,
    Install { source: String },
    Show { name: String },
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    Serve {
        #[arg(long)]
        allow_write: bool,
        #[arg(long)]
        allow_shell: bool,
        #[arg(long)]
        verify: bool,
        #[arg(long)]
        allow_worktree: bool,
    },
}

#[derive(Debug, Subcommand)]
enum WorktreeCommand {
    Create {
        name: String,
        #[arg(long = "ref", default_value = "HEAD")]
        reference: String,
    },
    List,
    Inspect {
        name: String,
    },
    Remove {
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum SupervisorCommand {
    Run {
        file: PathBuf,
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        #[arg(long)]
        allow_write: bool,
        #[arg(long)]
        allow_shell: bool,
        #[arg(long)]
        verify: bool,
        #[arg(long)]
        allow_plugins: bool,
    },
}

#[derive(Debug, Subcommand)]
enum WorkflowCommand {
    Run {
        file: PathBuf,
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        #[arg(long)]
        allow_write: bool,
        #[arg(long)]
        allow_shell: bool,
        #[arg(long)]
        verify: bool,
        #[arg(long)]
        allow_worktree: bool,
        #[arg(long)]
        allow_mcp_start: bool,
        #[arg(long)]
        allow_mcp_call: bool,
        #[arg(long)]
        allow_plugins: bool,
    },
    Resume {
        run_id: String,
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
        #[arg(long)]
        allow_write: bool,
        #[arg(long)]
        allow_shell: bool,
        #[arg(long)]
        verify: bool,
        #[arg(long)]
        allow_worktree: bool,
        #[arg(long)]
        allow_mcp_start: bool,
        #[arg(long)]
        allow_mcp_call: bool,
        #[arg(long)]
        allow_plugins: bool,
    },
}

struct ConsoleSink {
    json: bool,
}

#[async_trait]
impl EventSink for ConsoleSink {
    async fn emit(&self, event: &RunEvent) -> Result<(), EventError> {
        if self.json {
            println!(
                "{}",
                serde_json::to_string(event).map_err(EventError::Serialize)?
            );
        }
        match &event.kind {
            RunEventKind::ModelTextDelta { text } => {
                if !self.json {
                    print!("{text}");
                }
            }
            RunEventKind::ToolFinished { output, .. } => {
                if !self.json {
                    let _ = io::stdout().write_all(output.stdout.as_bytes());
                    let _ = io::stdout().write_all(b"\n");
                }
            }
            RunEventKind::RunFinished => {
                if !self.json {
                    let _ = io::stdout().write_all(b"\n");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let mut config = AppConfig::load(cli.config)?;
    if let Some(data_dir) = cli.data_dir {
        config.data_dir = data_dir;
    }
    if let Some(workspace) = cli.workspace {
        config.workspace = workspace;
    }
    if let Some(provider) = cli.provider {
        config.provider = match provider.as_str() {
            "openai" => ProviderKind::OpenAi,
            "anthropic" => ProviderKind::Anthropic,
            "ollama" => ProviderKind::Ollama,
            "lm-studio" => ProviderKind::LmStudio,
            "llama-cpp" => ProviderKind::LlamaCpp,
            "openai-compatible" => ProviderKind::OpenAiCompatible,
            _ => ProviderKind::Fake,
        };
    }
    if let Some(model) = cli.model {
        match config.provider {
            ProviderKind::Anthropic => config.anthropic.model = model,
            ProviderKind::Ollama => config.ollama.model = model,
            ProviderKind::LmStudio => config.lm_studio.model = model,
            ProviderKind::LlamaCpp => config.llama_cpp.model = model,
            ProviderKind::OpenAiCompatible => config.openai_compatible.model = model,
            _ => config.openai.model = model,
        }
    }
    if let Some(base_url) = cli.base_url {
        match config.provider {
            ProviderKind::Anthropic => config.anthropic.base_url = base_url,
            ProviderKind::Ollama => config.ollama.base_url = base_url,
            ProviderKind::LmStudio => config.lm_studio.base_url = base_url,
            ProviderKind::LlamaCpp => config.llama_cpp.base_url = base_url,
            ProviderKind::OpenAiCompatible => config.openai_compatible.base_url = base_url,
            _ => config.openai.base_url = base_url,
        }
    }
    let active_profile = cli.profile.as_deref().map(String::from);
    if let Some(profile) = &active_profile {
        config.apply_profile(profile)?;
    }
    config.validate()?;
    if cli.worktree.is_some() && matches!(&cli.command, Command::Supervisor { .. }) {
        return Err("--worktree cannot be combined with supervisor".into());
    }
    if let Some(name) = cli.worktree.as_deref() {
        let manager = worktree_manager(&config);
        let info = manager.inspect(name).await?;
        if info.state != WorktreeState::Active {
            return Err(format!("managed worktree is not active: {name}").into());
        }
        config.workspace = info.path;
    }
    match cli.command {
        Command::Context {
            command: ContextCommand::Map,
        } => {
            let map =
                omnicli::repomap::build_repo_map(&config.workspace, config.max_file_bytes as u64)?;
            if cli.json {
                let defs: Vec<_> = map
                    .iter()
                    .flat_map(|(path, defs)| {
                        defs.iter().map(move |d| {
                            serde_json::json!({
                                "path": path,
                                "kind": d.kind,
                                "signature": d.signature,
                                "line": d.line,
                            })
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string(&defs)?);
            } else {
                println!("{}", omnicli::repomap::render_map(&map, 65536));
            }
        }
        Command::Context {
            command: ContextCommand::Index,
        } => {
            let engine = ContextEngine::index(config.workspace.clone())?;
            let snapshot_path = config.data_dir.join("context.json");
            if let Some(parent) = snapshot_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&snapshot_path, serde_json::to_string_pretty(&engine)?)?;

            let search_index = omnicli::search::CodeIndex::open(&config.data_dir)?;
            let indexed = search_index.reindex(&config.workspace, config.max_file_bytes as u64)?;

            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({"snapshot": snapshot_path.to_string_lossy(), "search_indexed": indexed})
                );
            } else {
                println!("indexed {} files", engine.profile.indexed_files);
                println!("search indexed {} files", indexed);
                println!("snapshot: {}", snapshot_path.to_string_lossy());
            }
        }
        Command::Context {
            command: ContextCommand::Status,
        } => {
            let snapshot_path = config.data_dir.join("context.json");
            let engine = if snapshot_path.exists() {
                serde_json::from_str(&fs::read_to_string(&snapshot_path)?)?
            } else {
                ContextEngine::index(config.workspace.clone())?
            };
            if cli.json {
                println!("{}", serde_json::to_string(&engine.profile)?);
            } else {
                println!("workspace: {}", engine.workspace.display());
                println!("languages: {}", engine.profile.languages.join(", "));
                println!(
                    "package managers: {}",
                    engine.profile.package_managers.join(", ")
                );
                println!("indexed files: {}", engine.profile.indexed_files);
                println!("total bytes: {}", engine.profile.total_bytes);
                println!("estimated tokens: {}", engine.profile.estimated_tokens);
            }
        }
        Command::Context {
            command: ContextCommand::Query { prompt, limit },
        } => {
            let engine = ContextEngine::index(config.workspace.clone())?;
            let results = engine.query(&prompt, limit);
            if cli.json {
                println!("{}", serde_json::to_string(&results)?);
            } else {
                for result in results {
                    println!(
                        "{}\t{:.2}\t{}",
                        result.path, result.score, result.explanation
                    );
                }
            }
        }
        Command::Models => {
            let mut models = vec![
                serde_json::json!({
                    "selector": "fake",
                    "provider": "fake",
                    "model": null,
                }),
                serde_json::json!({
                    "selector": format!("openai/{}", config.openai.model),
                    "provider": "openai",
                    "model": config.openai.model,
                    "base_url": config.openai.base_url,
                }),
                serde_json::json!({
                    "selector": format!("anthropic/{}", config.anthropic.model),
                    "provider": "anthropic",
                    "model": config.anthropic.model,
                    "base_url": config.anthropic.base_url,
                    "api_version": config.anthropic.api_version,
                }),
                serde_json::json!({
                    "selector": format!("ollama/{}", config.ollama.model),
                    "provider": "ollama",
                    "model": config.ollama.model,
                    "base_url": config.ollama.base_url,
                }),
                serde_json::json!({
                    "selector": format!("lm-studio/{}", config.lm_studio.model),
                    "provider": "lm-studio",
                    "model": config.lm_studio.model,
                    "base_url": config.lm_studio.base_url,
                }),
                serde_json::json!({
                    "selector": format!("llama-cpp/{}", config.llama_cpp.model),
                    "provider": "llama-cpp",
                    "model": config.llama_cpp.model,
                    "base_url": config.llama_cpp.base_url,
                }),
            ];
            if !config.openai_compatible.base_url.is_empty()
                && !config.openai_compatible.model.is_empty()
            {
                models.push(serde_json::json!({
                    "selector": format!("openai-compatible/{}", config.openai_compatible.model),
                    "provider": "openai-compatible",
                    "model": config.openai_compatible.model,
                    "base_url": config.openai_compatible.base_url,
                    "api_key_env": config.openai_compatible.api_key_env,
                }));
            }
            if cli.json {
                println!("{}", serde_json::to_string(&models)?);
            } else {
                for model in models {
                    println!("{}", model["selector"].as_str().unwrap_or(""));
                }
            }
        }
        Command::Tools => {
            let tools = configured_tools(&config).specs();
            if cli.json {
                println!("{}", serde_json::to_string(&tools)?);
            } else {
                for tool in tools {
                    println!("{}\t{}", tool.name, tool.description);
                }
            }
        }
        Command::Mcp {
            command:
                McpCommand::Serve {
                    allow_write,
                    allow_shell,
                    verify,
                    allow_worktree,
                },
        } => {
            let policy = Policy::new(config.workspace.clone(), allow_write, allow_shell, verify)
                .with_worktrees(allow_worktree);
            let context = default_tool_context(
                config.workspace.clone(),
                config.max_tool_output_bytes,
                config.max_file_bytes,
                config.shell_timeout_seconds,
            );
            McpServer::new(
                configured_tools(&config),
                policy,
                context,
                McpServerOptions {
                    call_timeout: std::time::Duration::from_secs(config.shell_timeout_seconds),
                    max_message_bytes: config.mcp.max_message_bytes,
                },
            )
            .serve(tokio::io::stdin(), tokio::io::stdout())
            .await?;
        }
        Command::Workflow {
            command:
                WorkflowCommand::Run {
                    file,
                    concurrency,
                    allow_write,
                    allow_shell,
                    verify,
                    allow_worktree,
                    allow_mcp_start,
                    allow_mcp_call,
                    allow_plugins,
                },
        } => {
            let policy = Policy::new(config.workspace.clone(), allow_write, allow_shell, verify)
                .with_mcp(allow_mcp_start, allow_mcp_call)
                .with_worktrees(allow_worktree)
                .with_plugins(allow_plugins);
            let (mut tools, plugins) =
                configured_tools_with_plugins(&config, allow_plugins).await?;
            if allow_mcp_start {
                register_configured_tools(
                    &mut tools,
                    &config.mcp,
                    &config.workspace,
                    &policy,
                    config.max_tool_output_bytes,
                )
                .await?;
            }
            let store = Arc::new(SqliteStore::open(&config.database_path())?);
            let runtime = WorkflowRuntime::new(
                config.workspace.clone(),
                store,
                tools,
                policy,
                default_tool_context(
                    config.workspace,
                    config.max_tool_output_bytes,
                    config.max_file_bytes,
                    config.shell_timeout_seconds,
                ),
            )?;
            let prepared = runtime.prepare_start(file, concurrency).await?;
            let cancellation = CancellationToken::new();
            let signal_token = cancellation.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal_token.cancel();
                }
            });
            let report = prepared.execute(cancellation).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("workflow: {:?} ({})", report.status, report.run_id);
                for step in &report.steps {
                    println!(
                        "{:?}\t{}\t{}\tattempts={}",
                        step.status, step.id, step.tool, step.attempts
                    );
                    if let Some(error) = &step.error {
                        println!("  {}: {}", error.code, error.message);
                    }
                }
            }
            plugins.shutdown_all().await;
            if !report.succeeded() {
                return Err("workflow did not succeed".into());
            }
        }
        Command::Tui {
            session,
            allow_write,
            allow_shell,
            verify,
            allow_worktree,
            allow_mcp_start,
            allow_mcp_call,
            allow_plugins,
        } => {
            if cli.json {
                return Err("--json cannot be used with tui".into());
            }
            let openai_model = ModelSpec::OpenAiCompatible {
                base_url: config.openai.base_url.clone(),
                model: config.openai.model.clone(),
                timeout: std::time::Duration::from_secs(config.openai.timeout_seconds),
            };
            let anthropic_model = ModelSpec::Anthropic {
                base_url: config.anthropic.base_url.clone(),
                model: config.anthropic.model.clone(),
                timeout: std::time::Duration::from_secs(config.anthropic.timeout_seconds),
                api_version: config.anthropic.api_version.clone(),
            };
            let ollama_model = ModelSpec::Ollama {
                base_url: config.ollama.base_url.clone(),
                model: config.ollama.model.clone(),
                timeout: std::time::Duration::from_secs(config.ollama.timeout_seconds),
            };
            let lm_studio_model = ModelSpec::LmStudio {
                base_url: config.lm_studio.base_url.clone(),
                model: config.lm_studio.model.clone(),
                timeout: std::time::Duration::from_secs(config.lm_studio.timeout_seconds),
            };
            let llama_cpp_model = ModelSpec::LlamaCpp {
                base_url: config.llama_cpp.base_url.clone(),
                model: config.llama_cpp.model.clone(),
                timeout: std::time::Duration::from_secs(config.llama_cpp.timeout_seconds),
                temperature: config.llama_cpp.temperature,
                n_predict: config.llama_cpp.n_predict,
            };
            let openai_compatible_model = ModelSpec::OpenAiCompatible {
                base_url: config.openai_compatible.base_url.clone(),
                model: config.openai_compatible.model.clone(),
                timeout: std::time::Duration::from_secs(config.openai_compatible.timeout_seconds),
            };
            let initial_model = match config.provider {
                ProviderKind::Fake => ModelSpec::Fake,
                ProviderKind::OpenAi => openai_model.clone(),
                ProviderKind::Anthropic => anthropic_model.clone(),
                ProviderKind::Ollama => ollama_model.clone(),
                ProviderKind::LmStudio => lm_studio_model.clone(),
                ProviderKind::LlamaCpp => llama_cpp_model.clone(),
                ProviderKind::OpenAiCompatible => openai_compatible_model.clone(),
            };
            let provider_factory = Arc::new(ProviderFactory::from_env());
            let provider = provider_factory.build(&initial_model)?;
            let policy = Policy::new(config.workspace.clone(), allow_write, allow_shell, verify)
                .with_mcp(allow_mcp_start, allow_mcp_call)
                .with_worktrees(allow_worktree)
                .with_plugins(allow_plugins);
            let (mut tools, plugins) =
                configured_tools_with_plugins(&config, allow_plugins).await?;
            if allow_mcp_start {
                register_configured_tools(
                    &mut tools,
                    &config.mcp,
                    &config.workspace,
                    &policy,
                    config.max_tool_output_bytes,
                )
                .await?;
            }
            let (authorizer, permission_receiver) = permission_bridge(policy);
            let store = Arc::new(SqliteStore::open(&config.database_path())?);
            let context = default_tool_context(
                config.workspace.clone(),
                config.max_tool_output_bytes,
                config.max_file_bytes,
                config.shell_timeout_seconds,
            );
            let workflow_runtime = Arc::new(WorkflowRuntime::new(
                config.workspace.clone(),
                store.clone(),
                tools.clone(),
                Policy::new(config.workspace.clone(), allow_write, allow_shell, verify)
                    .with_mcp(allow_mcp_start, allow_mcp_call)
                    .with_worktrees(allow_worktree),
                context.clone(),
            )?);
            let supervisor_runtime = Arc::new(SupervisorRuntime::new(
                config.workspace.clone(),
                store.clone(),
                worktree_manager(&config),
                provider_factory.clone(),
                Policy::new(config.workspace.clone(), allow_write, allow_shell, verify),
                config.max_turns,
                config.max_tool_output_bytes,
                config.max_file_bytes,
                std::time::Duration::from_secs(config.shell_timeout_seconds),
            )?);
            let agent = Arc::new(Agent::with_authorizer(
                provider,
                tools,
                authorizer,
                store.clone(),
                context,
                config.max_turns,
            ));
            run_tui(
                agent,
                store,
                workflow_runtime,
                supervisor_runtime,
                provider_factory,
                permission_receiver,
                TuiOptions {
                    session_id: session,
                    verify,
                    initial_model,
                    available_models: vec![
                        ModelSpec::Fake,
                        openai_model,
                        anthropic_model,
                        ollama_model,
                        openai_compatible_model,
                    ],
                },
            )
            .await?;
            plugins.shutdown_all().await;
        }
        Command::Workflow {
            command:
                WorkflowCommand::Resume {
                    run_id,
                    concurrency,
                    allow_write,
                    allow_shell,
                    verify,
                    allow_worktree,
                    allow_mcp_start,
                    allow_mcp_call,
                    allow_plugins,
                },
        } => {
            let store = Arc::new(SqliteStore::open(&config.database_path())?);
            let policy = Policy::new(config.workspace.clone(), allow_write, allow_shell, verify)
                .with_mcp(allow_mcp_start, allow_mcp_call)
                .with_worktrees(allow_worktree)
                .with_plugins(allow_plugins);
            let (mut tools, plugins) =
                configured_tools_with_plugins(&config, allow_plugins).await?;
            if allow_mcp_start {
                register_configured_tools(
                    &mut tools,
                    &config.mcp,
                    &config.workspace,
                    &policy,
                    config.max_tool_output_bytes,
                )
                .await?;
            }
            let runtime = WorkflowRuntime::new(
                config.workspace.clone(),
                store,
                tools,
                policy,
                default_tool_context(
                    config.workspace,
                    config.max_tool_output_bytes,
                    config.max_file_bytes,
                    config.shell_timeout_seconds,
                ),
            )?;
            let prepared = runtime.prepare_resume(&run_id, concurrency).await?;
            let cancellation = CancellationToken::new();
            let signal_token = cancellation.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal_token.cancel();
                }
            });
            let report = prepared.execute(cancellation).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("workflow: {:?} ({})", report.status, report.run_id);
                for step in &report.steps {
                    println!(
                        "{:?}\t{}\t{}\tattempts={}",
                        step.status, step.id, step.tool, step.attempts
                    );
                }
            }
            plugins.shutdown_all().await;
            if !report.succeeded() {
                return Err("workflow did not succeed".into());
            }
        }
        Command::Worktree { command } => {
            let manager = worktree_manager(&config);
            let value = match command {
                WorktreeCommand::Create { name, reference } => {
                    serde_json::to_value(manager.create(&name, &reference).await?)?
                }
                WorktreeCommand::List => serde_json::to_value(manager.list().await?)?,
                WorktreeCommand::Inspect { name } => {
                    serde_json::to_value(manager.inspect(&name).await?)?
                }
                WorktreeCommand::Remove { name } => {
                    serde_json::to_value(manager.remove(&name).await?)?
                }
            };
            if cli.json {
                println!("{value}");
            } else {
                println!("{}", serde_json::to_string_pretty(&value)?);
            }
        }
        Command::Supervisor {
            command:
                SupervisorCommand::Run {
                    file,
                    concurrency,
                    allow_write,
                    allow_shell,
                    verify,
                    allow_plugins,
                },
        } => {
            let (_, plugins) = configured_tools_with_plugins(&config, allow_plugins).await?;
            let store = Arc::new(SqliteStore::open(&config.database_path())?);
            let runtime = SupervisorRuntime::new(
                config.workspace.clone(),
                store,
                worktree_manager(&config),
                Arc::new(ProviderFactory::from_env()),
                Policy::new(config.workspace.clone(), allow_write, allow_shell, verify)
                    .with_plugins(allow_plugins),
                config.max_turns,
                config.max_tool_output_bytes,
                config.max_file_bytes,
                std::time::Duration::from_secs(config.shell_timeout_seconds),
            )?;
            let prepared = runtime
                .prepare_start(file, concurrency, current_model_spec(&config))
                .await?;
            let cancellation = CancellationToken::new();
            let signal_token = cancellation.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal_token.cancel();
                }
            });
            let report = prepared.execute(cancellation).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("supervisor: {:?} ({})", report.status, report.run_id);
                for task in &report.tasks {
                    println!(
                        "{:?}\t{}\t{}\tsession={}",
                        task.status, task.id, task.worktree, task.session_id
                    );
                }
            }
            plugins.shutdown_all().await;
            if !report.succeeded() {
                return Err("supervisor did not succeed".into());
            }
        }
        Command::Completions { shell } => {
            let shell = Shell::from_str(&shell, true)
                .map_err(|error| format!("unsupported shell: {error}"))?;
            let mut command = Cli::command();
            generate(shell, &mut command, "omni", &mut io::stdout());
        }
        Command::Plugins { command } => match command {
            PluginCommand::List => {
                let registry = PluginRegistry::load_from_config(&config.plugins).await?;
                let entries: Vec<PluginListEntry> = registry
                    .list()
                    .iter()
                    .map(|plugin| PluginListEntry {
                        name: plugin.name.clone(),
                        version: plugin.version.clone(),
                        description: plugin.description.clone(),
                        tools: plugin.tools.iter().map(|t| t.name.clone()).collect(),
                    })
                    .collect();
                if cli.json {
                    println!("{}", serde_json::to_string(&entries)?);
                } else {
                    for entry in entries {
                        println!("{}\t{}\t{}", entry.name, entry.version, entry.description);
                        for tool in &entry.tools {
                            println!("  - {tool}");
                        }
                    }
                }
                registry.shutdown_all().await;
            }
            PluginCommand::Show { name } => {
                let registry = PluginRegistry::load_from_config(&config.plugins).await?;
                let plugin = registry.get(&name).ok_or("plugin not found")?;
                let entry = PluginListEntry {
                    name: plugin.name.clone(),
                    version: plugin.version.clone(),
                    description: plugin.description.clone(),
                    tools: plugin.tools.iter().map(|t| t.name.clone()).collect(),
                };
                if cli.json {
                    println!("{}", serde_json::to_string(&entry)?);
                } else {
                    println!("{}\t{}\t{}", entry.name, entry.version, entry.description);
                    println!("manifest dir: {}", plugin.manifest_dir.display());
                    println!("permissions: {:?}", plugin.permissions);
                    for tool in &entry.tools {
                        println!("  - {tool}");
                    }
                }
                registry.shutdown_all().await;
            }
            PluginCommand::Install { source } => {
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!({"status": "install not implemented", "source": source})
                    );
                } else {
                    println!(
                        "install from {source} is not implemented; configure plugins in omni.toml"
                    );
                }
            }
        },
        Command::Doctor => {
            let provider = match config.provider {
                ProviderKind::Fake => "fake",
                ProviderKind::OpenAi => "openai",
                ProviderKind::Anthropic => "anthropic",
                ProviderKind::Ollama => "ollama",
                ProviderKind::LmStudio => "lm-studio",
                ProviderKind::LlamaCpp => "llama-cpp",
                ProviderKind::OpenAiCompatible => "openai-compatible",
            };
            let api_key_present = match config.provider {
                ProviderKind::OpenAi => std::env::var_os("OPENAI_API_KEY").is_some(),
                ProviderKind::Anthropic => std::env::var_os("ANTHROPIC_API_KEY").is_some(),
                ProviderKind::OpenAiCompatible => {
                    std::env::var_os(&config.openai_compatible.api_key_env).is_some()
                }
                ProviderKind::Fake
                | ProviderKind::Ollama
                | ProviderKind::LmStudio
                | ProviderKind::LlamaCpp => true,
            };
            let status = if !api_key_present { "degraded" } else { "ok" };
            let (model, base_url) = match config.provider {
                ProviderKind::Fake => (None::<String>, None::<String>),
                ProviderKind::OpenAi => (
                    Some(config.openai.model.clone()),
                    Some(config.openai.base_url.clone()),
                ),
                ProviderKind::Anthropic => (
                    Some(config.anthropic.model.clone()),
                    Some(config.anthropic.base_url.clone()),
                ),
                ProviderKind::Ollama => (
                    Some(config.ollama.model.clone()),
                    Some(config.ollama.base_url.clone()),
                ),
                ProviderKind::LmStudio => (
                    Some(config.lm_studio.model.clone()),
                    Some(config.lm_studio.base_url.clone()),
                ),
                ProviderKind::LlamaCpp => (
                    Some(config.llama_cpp.model.clone()),
                    Some(config.llama_cpp.base_url.clone()),
                ),
                ProviderKind::OpenAiCompatible => (
                    Some(config.openai_compatible.model.clone()),
                    Some(config.openai_compatible.base_url.clone()),
                ),
            };
            let report = serde_json::json!({
                "status": status,
                "provider": provider,
                "model": model,
                "base_url": base_url,
                "api_key_present": api_key_present,
                "profile": active_profile,
                "database": config.database_path(),
                "workspace": config.workspace,
                "tools": ["apply_patch", "create_file", "git_diff", "git_status", "git_worktree_create", "git_worktree_inspect", "git_worktree_list", "git_worktree_remove", "read_file", "run_checks", "search_files", "shell"],
                "mcp_servers_configured": config.mcp.servers.len(),
                "plugins_configured": config.plugins.len(),
            });
            if cli.json {
                println!("{report}");
            } else {
                println!("omni doctor: {status}\n{report:#}");
            }
        }
        Command::Run {
            prompt,
            session,
            allow_write,
            allow_shell,
            verify,
            allow_worktree,
            allow_mcp_start,
            allow_mcp_call,
            allow_plugins,
        } => {
            execute_run(
                &config,
                cli.json,
                RunRequest {
                    prompt,
                    session_id: session,
                    verify,
                    system_prompt: None,
                },
                allow_write,
                allow_shell,
                verify,
                allow_worktree,
                allow_mcp_start,
                allow_mcp_call,
                allow_plugins,
            )
            .await?;
        }
        Command::Ask { prompt, session } => {
            execute_run(
                &config,
                cli.json,
                RunRequest {
                    prompt,
                    session_id: session,
                    verify: false,
                    system_prompt: Some(
                        "You are a helpful assistant. Answer questions clearly and concisely."
                            .into(),
                    ),
                },
                false,
                false,
                false,
                false,
                false,
                false,
                false,
            )
            .await?;
        }
        Command::Plan { prompt, session } => {
            execute_run(
                &config,
                cli.json,
                RunRequest {
                    prompt,
                    session_id: session,
                    verify: false,
                    system_prompt: Some(
                        "You are an architect. Create a detailed plan. Do not write code.".into(),
                    ),
                },
                false,
                false,
                false,
                false,
                false,
                false,
                false,
            )
            .await?;
        }
        Command::Review { session, verify } => {
            execute_run(
                &config,
                cli.json,
                RunRequest {
                    prompt: "Review the current workspace for issues.".into(),
                    session_id: session,
                    verify,
                    system_prompt: Some(
                        "You are a code reviewer. Examine the project and list concrete issues."
                            .into(),
                    ),
                },
                false,
                false,
                verify,
                false,
                false,
                false,
                false,
            )
            .await?;
        }
        Command::Sessions { command } => match command {
            SessionCommand::List { limit } => {
                let store = SqliteStore::open(&config.database_path())?;
                let sessions = store.list_sessions(limit)?;
                for s in sessions {
                    println!("{}\t{}\t{}", s.id, s.title, s.updated_at_ms);
                }
            }
            SessionCommand::Show { id } => {
                let store = SqliteStore::open(&config.database_path())?;
                let title = store.session_title(&id)?.ok_or("session not found")?;
                let messages = store.load_messages(&id)?;
                let events = store.load_run_events(&id)?;
                let usage =
                    events
                        .iter()
                        .fold(omnicli::Usage::default(), |mut accumulator, event| {
                            if let RunEventKind::Usage { usage } = &event.kind {
                                accumulator.input_tokens += usage.input_tokens;
                                accumulator.output_tokens += usage.output_tokens;
                                accumulator.total_tokens += usage.total_tokens;
                            }
                            accumulator
                        });
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "session_id": id,
                            "title": title,
                            "messages": messages,
                            "usage": usage,
                        })
                    );
                } else {
                    println!("{title}\n");
                    for message in messages {
                        println!("{:?}: {}", message.role, message.content);
                    }
                    if usage.total_tokens > 0 {
                        println!(
                            "\ntokens: input={}, output={}, total={}",
                            usage.input_tokens, usage.output_tokens, usage.total_tokens
                        );
                    }
                }
            }
        },
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn execute_run(
    config: &AppConfig,
    json: bool,
    request: RunRequest,
    allow_write: bool,
    allow_shell: bool,
    verify: bool,
    allow_worktree: bool,
    allow_mcp_start: bool,
    allow_mcp_call: bool,
    allow_plugins: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let provider: Arc<dyn ModelProvider> = match config.provider {
        ProviderKind::Fake => Arc::new(FakeProvider),
        ProviderKind::OpenAi => Arc::new(OpenAiProvider::from_env(
            &config.openai.base_url,
            config.openai.model.clone(),
            std::time::Duration::from_secs(config.openai.timeout_seconds),
        )?),
        ProviderKind::Anthropic => Arc::new(AnthropicProvider::from_env(
            &config.anthropic.base_url,
            config.anthropic.model.clone(),
            std::time::Duration::from_secs(config.anthropic.timeout_seconds),
            &config.anthropic.api_version,
        )?),
        ProviderKind::Ollama => Arc::new(OllamaProvider::new(
            &config.ollama.base_url,
            config.ollama.model.clone(),
            std::time::Duration::from_secs(config.ollama.timeout_seconds),
        )?),
        ProviderKind::LmStudio => Arc::new(OpenAiProvider::new(
            &config.lm_studio.base_url,
            config.lm_studio.model.clone(),
            std::time::Duration::from_secs(config.lm_studio.timeout_seconds),
            "",
        )?),
        ProviderKind::LlamaCpp => Arc::new(LlamaCppProvider::new(
            &config.llama_cpp.base_url,
            config.llama_cpp.model.clone(),
            std::time::Duration::from_secs(config.llama_cpp.timeout_seconds),
            config.llama_cpp.temperature,
            config.llama_cpp.n_predict,
        )?),
        ProviderKind::OpenAiCompatible => {
            let api_key = std::env::var(&config.openai_compatible.api_key_env).map_err(|_| {
                format!(
                    "{} environment variable not set",
                    config.openai_compatible.api_key_env
                )
            })?;
            Arc::new(OpenAiProvider::new(
                &config.openai_compatible.base_url,
                config.openai_compatible.model.clone(),
                std::time::Duration::from_secs(config.openai_compatible.timeout_seconds),
                &api_key,
            )?)
        }
    };

    let policy = Policy::new(config.workspace.clone(), allow_write, allow_shell, verify)
        .with_mcp(allow_mcp_start, allow_mcp_call)
        .with_worktrees(allow_worktree)
        .with_plugins(allow_plugins);
    let (mut tools, plugins) = configured_tools_with_plugins(config, allow_plugins).await?;
    if allow_mcp_start {
        register_configured_tools(
            &mut tools,
            &config.mcp,
            &config.workspace,
            &policy,
            config.max_tool_output_bytes,
        )
        .await?;
    }
    let store = Arc::new(SqliteStore::open(&config.database_path())?);
    let agent = Agent::new(
        provider,
        tools,
        policy,
        store,
        default_tool_context(
            config.workspace.clone(),
            config.max_tool_output_bytes,
            config.max_file_bytes,
            config.shell_timeout_seconds,
        ),
        config.max_turns,
    );
    let cancellation = CancellationToken::new();
    let signal_token = cancellation.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_token.cancel();
        }
    });
    let outcome = agent
        .run(request, Arc::new(ConsoleSink { json }), cancellation)
        .await?;
    if !json {
        println!("\n\nsession: {}", outcome.session_id);
    }
    plugins.shutdown_all().await;
    Ok(())
}

fn worktree_manager(config: &AppConfig) -> Arc<WorktreeManager> {
    Arc::new(WorktreeManager::new(
        config.workspace.clone(),
        config.data_dir.clone(),
        config.max_tool_output_bytes,
        std::time::Duration::from_secs(config.shell_timeout_seconds),
    ))
}

fn configured_tools(config: &AppConfig) -> ToolRegistry {
    let mut tools = ToolRegistry::standard();
    register_worktree_tools(&mut tools, worktree_manager(config));
    tools
}

async fn configured_tools_with_plugins(
    config: &AppConfig,
    allow_plugins: bool,
) -> Result<(ToolRegistry, PluginRegistry), Box<dyn std::error::Error>> {
    let mut tools = configured_tools(config);
    let registry = if allow_plugins && !config.plugins.is_empty() {
        let registry = PluginRegistry::load_from_config(&config.plugins).await?;
        registry.register_tools(&mut tools);
        registry
    } else {
        PluginRegistry::default()
    };
    Ok((tools, registry))
}

fn current_model_spec(config: &AppConfig) -> ModelSpec {
    match config.provider {
        ProviderKind::Fake => ModelSpec::Fake,
        ProviderKind::OpenAi => ModelSpec::OpenAi {
            base_url: config.openai.base_url.clone(),
            model: config.openai.model.clone(),
            timeout: std::time::Duration::from_secs(config.openai.timeout_seconds),
        },
        ProviderKind::Anthropic => ModelSpec::Anthropic {
            base_url: config.anthropic.base_url.clone(),
            model: config.anthropic.model.clone(),
            timeout: std::time::Duration::from_secs(config.anthropic.timeout_seconds),
            api_version: config.anthropic.api_version.clone(),
        },
        ProviderKind::Ollama => ModelSpec::Ollama {
            base_url: config.ollama.base_url.clone(),
            model: config.ollama.model.clone(),
            timeout: std::time::Duration::from_secs(config.ollama.timeout_seconds),
        },
        ProviderKind::LmStudio => ModelSpec::LmStudio {
            base_url: config.lm_studio.base_url.clone(),
            model: config.lm_studio.model.clone(),
            timeout: std::time::Duration::from_secs(config.lm_studio.timeout_seconds),
        },
        ProviderKind::LlamaCpp => ModelSpec::LlamaCpp {
            base_url: config.llama_cpp.base_url.clone(),
            model: config.llama_cpp.model.clone(),
            timeout: std::time::Duration::from_secs(config.llama_cpp.timeout_seconds),
            temperature: config.llama_cpp.temperature,
            n_predict: config.llama_cpp.n_predict,
        },
        ProviderKind::OpenAiCompatible => ModelSpec::OpenAiCompatible {
            base_url: config.openai_compatible.base_url.clone(),
            model: config.openai_compatible.model.clone(),
            timeout: std::time::Duration::from_secs(config.openai_compatible.timeout_seconds),
        },
    }
}
