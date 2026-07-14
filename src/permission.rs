use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use async_trait::async_trait;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "capability", rename_all = "snake_case")]
pub enum PermissionRequest {
    FileRead {
        path: PathBuf,
    },
    FileWrite {
        path: PathBuf,
    },
    GitRead {
        operation: GitReadOperation,
    },
    GitWorktree {
        operation: GitWorktreeOperation,
        name: Option<String>,
    },
    Validation,
    McpProcessStart {
        server: String,
        command: PathBuf,
    },
    McpToolCall {
        server: String,
        tool: String,
    },
    Shell {
        command: String,
    },
    PluginCall {
        plugin: String,
        tool: String,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitReadOperation {
    Status,
    Diff,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeOperation {
    List,
    Inspect,
    Create,
    Remove,
}

#[derive(Clone, Debug)]
pub struct PermissionDecision {
    pub allowed: bool,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct AuthorizationRequest {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
    pub permission: PermissionRequest,
}

#[derive(Clone, Debug)]
pub enum PolicyEvaluation {
    Resolved(PermissionDecision),
    RequiresApproval { reason: String },
}

#[async_trait]
pub trait PermissionAuthorizer: Send + Sync {
    async fn authorize(&self, request: &AuthorizationRequest) -> PermissionDecision;
}

#[derive(Clone, Debug)]
pub struct Policy {
    workspace: PathBuf,
    allow_write: bool,
    allow_shell: bool,
    allow_validation: bool,
    allow_mcp_start: bool,
    allow_mcp_call: bool,
    allow_worktree: bool,
    allow_plugins: bool,
}

impl Policy {
    pub fn new(
        workspace: PathBuf,
        allow_write: bool,
        allow_shell: bool,
        allow_validation: bool,
    ) -> Self {
        Self {
            workspace,
            allow_write,
            allow_shell,
            allow_validation,
            allow_mcp_start: false,
            allow_mcp_call: false,
            allow_worktree: false,
            allow_plugins: false,
        }
    }

    pub fn with_mcp(mut self, allow_start: bool, allow_call: bool) -> Self {
        self.allow_mcp_start = allow_start;
        self.allow_mcp_call = allow_call;
        self
    }

    pub fn with_worktrees(mut self, allow: bool) -> Self {
        self.allow_worktree = allow;
        self
    }

    pub fn with_plugins(mut self, allow: bool) -> Self {
        self.allow_plugins = allow;
        self
    }

    pub fn for_workspace(&self, workspace: PathBuf) -> Self {
        let mut policy = self.clone();
        policy.workspace = workspace;
        policy
    }

    pub fn decide(&self, request: &PermissionRequest) -> PermissionDecision {
        match self.evaluate(request) {
            PolicyEvaluation::Resolved(decision) => decision,
            PolicyEvaluation::RequiresApproval { reason } => deny(reason),
        }
    }

    pub fn evaluate(&self, request: &PermissionRequest) -> PolicyEvaluation {
        match request {
            PermissionRequest::FileRead { path } => {
                PolicyEvaluation::Resolved(self.path_decision(path, true))
            }
            PermissionRequest::FileWrite { path } => {
                let path_decision = self.path_decision(path, true);
                if !path_decision.allowed || self.allow_write {
                    PolicyEvaluation::Resolved(path_decision)
                } else {
                    approval("file writes require --allow-write")
                }
            }
            PermissionRequest::GitRead { operation } => resolved_allow(format!(
                "read-only git {} inside workspace",
                match operation {
                    GitReadOperation::Status => "status",
                    GitReadOperation::Diff => "diff",
                }
            )),
            PermissionRequest::GitWorktree {
                operation: GitWorktreeOperation::List | GitWorktreeOperation::Inspect,
                ..
            } => resolved_allow("read-only managed worktree inspection"),
            PermissionRequest::GitWorktree { operation, .. } if self.allow_worktree => {
                resolved_allow(format!("managed worktree {operation:?} explicitly enabled"))
            }
            PermissionRequest::GitWorktree { .. } => {
                approval("worktree mutation requires --allow-worktree")
            }
            PermissionRequest::Validation if self.allow_validation => {
                resolved_allow("project validation explicitly enabled")
            }
            PermissionRequest::Validation => approval("validation requires --verify"),
            PermissionRequest::McpProcessStart { server, .. } if self.allow_mcp_start => {
                resolved_allow(format!("MCP process start explicitly enabled for {server}"))
            }
            PermissionRequest::McpProcessStart { .. } => {
                approval("MCP process start requires --allow-mcp-start")
            }
            PermissionRequest::McpToolCall { server, tool } if self.allow_mcp_call => {
                resolved_allow(format!("MCP call explicitly enabled for {server}/{tool}"))
            }
            PermissionRequest::McpToolCall { .. } => {
                approval("MCP tool calls require --allow-mcp-call")
            }
            PermissionRequest::Shell { .. } if self.allow_shell => {
                resolved_allow("shell explicitly allowed")
            }
            PermissionRequest::Shell { .. } => approval("shell requires --allow-shell"),
            PermissionRequest::PluginCall { plugin, tool } if self.allow_plugins => resolved_allow(
                format!("plugin calls explicitly enabled for {plugin}/{tool}"),
            ),
            PermissionRequest::PluginCall { .. } => {
                approval("plugin calls require --allow-plugins")
            }
        }
    }

    fn path_decision(&self, path: &Path, capability_allowed: bool) -> PermissionDecision {
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::Prefix(_) | Component::RootDir
                )
            })
        {
            return deny("path must be relative and remain inside the workspace");
        }
        if !capability_allowed {
            return deny("file writes require --allow-write");
        }
        allow(format!(
            "path is lexically relative to {}",
            self.workspace.display()
        ))
    }
}

#[async_trait]
impl PermissionAuthorizer for Policy {
    async fn authorize(&self, request: &AuthorizationRequest) -> PermissionDecision {
        self.decide(&request.permission)
    }
}

fn resolved_allow(reason: impl Into<String>) -> PolicyEvaluation {
    PolicyEvaluation::Resolved(allow(reason))
}

fn approval(reason: impl Into<String>) -> PolicyEvaluation {
    PolicyEvaluation::RequiresApproval {
        reason: reason.into(),
    }
}

fn allow(reason: impl Into<String>) -> PermissionDecision {
    PermissionDecision {
        allowed: true,
        reason: reason.into(),
    }
}

fn deny(reason: impl Into<String>) -> PermissionDecision {
    PermissionDecision {
        allowed: false,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy::new(PathBuf::from("workspace"), false, false, false)
    }

    #[test]
    fn reads_inside_workspace_are_allowed() {
        let decision = policy().decide(&PermissionRequest::FileRead {
            path: PathBuf::from("src/main.rs"),
        });
        assert!(decision.allowed);
    }

    #[test]
    fn traversal_and_absolute_paths_are_denied() {
        let absolute = std::env::current_dir()
            .expect("current directory")
            .join("secret");
        for path in [PathBuf::from("../secret"), absolute] {
            let decision = policy().decide(&PermissionRequest::FileRead { path });
            assert!(!decision.allowed);
        }
    }

    #[test]
    fn side_effects_require_explicit_flags() {
        let write = policy().decide(&PermissionRequest::FileWrite {
            path: PathBuf::from("output.txt"),
        });
        let shell = policy().decide(&PermissionRequest::Shell {
            command: "echo test".into(),
        });
        assert!(!write.allowed);
        assert!(!shell.allowed);
    }

    #[test]
    fn read_only_git_does_not_require_shell_permission() {
        let decision = policy().decide(&PermissionRequest::GitRead {
            operation: GitReadOperation::Diff,
        });
        assert!(decision.allowed);
    }

    #[test]
    fn validation_requires_its_own_permission() {
        assert!(!policy().decide(&PermissionRequest::Validation).allowed);
        let validation = Policy::new(PathBuf::from("workspace"), false, false, true);
        assert!(validation.decide(&PermissionRequest::Validation).allowed);
        assert!(
            !validation
                .decide(&PermissionRequest::Shell {
                    command: "echo no".into(),
                })
                .allowed
        );
    }

    #[test]
    fn mcp_start_and_calls_are_independent() {
        let start_only = policy().with_mcp(true, false);
        assert!(
            start_only
                .decide(&PermissionRequest::McpProcessStart {
                    server: "test".into(),
                    command: PathBuf::from("server"),
                })
                .allowed
        );
        assert!(
            !start_only
                .decide(&PermissionRequest::McpToolCall {
                    server: "test".into(),
                    tool: "echo".into(),
                })
                .allowed
        );

        let call_only = policy().with_mcp(false, true);
        assert!(
            call_only
                .decide(&PermissionRequest::McpToolCall {
                    server: "test".into(),
                    tool: "echo".into(),
                })
                .allowed
        );
        assert!(
            !call_only
                .decide(&PermissionRequest::McpProcessStart {
                    server: "test".into(),
                    command: PathBuf::from("server"),
                })
                .allowed
        );
    }

    #[test]
    fn worktree_reads_and_mutations_have_separate_policy() {
        let policy = policy();
        assert!(
            policy
                .decide(&PermissionRequest::GitWorktree {
                    operation: GitWorktreeOperation::List,
                    name: None,
                })
                .allowed
        );
        assert!(
            !policy
                .decide(&PermissionRequest::GitWorktree {
                    operation: GitWorktreeOperation::Create,
                    name: Some("task".into()),
                })
                .allowed
        );
        assert!(
            policy
                .with_worktrees(true)
                .decide(&PermissionRequest::GitWorktree {
                    operation: GitWorktreeOperation::Create,
                    name: Some("task".into()),
                })
                .allowed
        );
    }
}
