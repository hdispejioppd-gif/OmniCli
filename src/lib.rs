pub mod agent;
pub mod config;
pub mod context;
pub mod diffview;
pub mod events;
pub mod lsp;
pub mod mcp;
pub mod permission;
pub mod plugin;
pub mod protocol;
pub mod provider;
pub mod repomap;
pub mod routing;
pub mod schema;
pub mod search;
pub mod store;
pub mod supervisor;
pub mod theme;
pub mod tools;
pub mod tui;
pub mod tui_banner;
pub mod tui_markdown;
pub mod tui_palette;
pub mod workflow;
pub mod worktree;

pub use agent::{Agent, AgentError, DEFAULT_AGENT_SYSTEM_PROMPT, ProviderSwitchError, RunRequest};
pub use config::{
    AppConfig, LlamaCppConfig, LmStudioConfig, McpConfig, McpStdioServerConfig, OllamaConfig,
    OpenAiCompatibleConfig, ProviderKind,
};
pub use context::ContextEngine;
pub use events::{EventSink, RunEvent, RunEventKind};
pub use mcp::{
    McpServer, McpServerListing, McpServerOptions, McpToolListing, list_configured_tools,
    register_configured_tools,
};
pub use permission::{
    AuthorizationRequest, PermissionAuthorizer, PermissionDecision, Policy, PolicyEvaluation,
};
pub use plugin::{
    LoadedPlugin, PluginConfig, PluginError, PluginHost, PluginListEntry, PluginManifest,
    PluginMeta, PluginPermissions, PluginRegistry,
};
pub use protocol::{
    Message, ModelEvent, ModelRequest, Role, ToolCall, ToolOutput, ToolSpec, Usage,
};
pub use provider::{
    AnthropicProvider, FakeProvider, LlamaCppProvider, ModelProvider, ModelSpec, OllamaProvider,
    OpenAiProvider, ProviderFactory,
};
pub use store::SqliteStore;
pub use supervisor::{
    PreparedSupervisor, SupervisorDocument, SupervisorError, SupervisorReport, SupervisorRuntime,
    SupervisorStatus, SupervisorTaskDefinition, SupervisorTaskReport,
};
pub use tools::ToolRegistry;
pub use workflow::{
    ArtifactMetadata, PreparedWorkflow, StepReport, StepStatus, WorkflowReport, WorkflowRunner,
    WorkflowRuntime, WorkflowStatus, load_workflow, validate_workflow, workflow_fingerprint,
};
pub use worktree::{
    WorktreeError, WorktreeInfo, WorktreeManager, WorktreeState, register_worktree_tools,
};
