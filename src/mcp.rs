use std::{
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    process::Child,
    sync::Mutex,
};

use crate::{
    config::{McpConfig, McpStdioServerConfig},
    permission::{PermissionRequest, Policy},
    protocol::{ToolOutput, ToolSpec},
    tools::{Tool, ToolContext, ToolError, ToolRegistry},
};

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Clone, Debug)]
pub struct McpClientOptions {
    pub startup_timeout: Duration,
    pub call_timeout: Duration,
    pub max_message_bytes: usize,
    pub max_output_bytes: usize,
}

struct McpConnection {
    reader: Box<dyn AsyncBufRead + Unpin + Send>,
    writer: Box<dyn AsyncWrite + Unpin + Send>,
    child: Option<Child>,
    poisoned: bool,
}

pub struct McpClient {
    server_name: String,
    next_id: AtomicU64,
    connection: Mutex<McpConnection>,
    options: McpClientOptions,
}

impl McpClient {
    pub async fn connect<R, W>(
        server_name: String,
        reader: R,
        writer: W,
        options: McpClientOptions,
    ) -> Result<Arc<Self>, McpError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        Self::connect_inner(server_name, reader, writer, None, options).await
    }

    async fn connect_process(
        server_name: String,
        config: &McpStdioServerConfig,
        workspace: &Path,
        max_message_bytes: usize,
        max_output_bytes: usize,
    ) -> Result<Arc<Self>, McpError> {
        let mut command = tokio::process::Command::new(&config.command);
        command
            .args(&config.args)
            .current_dir(workspace)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for name in &config.env {
            let value = std::env::var_os(name).ok_or_else(|| McpError::MissingEnvironment {
                server: server_name.clone(),
                name: name.clone(),
            })?;
            command.env(name, value);
        }
        let mut child = command.spawn().map_err(|source| McpError::Spawn {
            server: server_name.clone(),
            source,
        })?;
        let reader = child.stdout.take().ok_or_else(|| McpError::Protocol {
            server: server_name.clone(),
            message: "child stdout was not piped".into(),
        })?;
        let writer = child.stdin.take().ok_or_else(|| McpError::Protocol {
            server: server_name.clone(),
            message: "child stdin was not piped".into(),
        })?;
        Self::connect_inner(
            server_name,
            reader,
            writer,
            Some(child),
            McpClientOptions {
                startup_timeout: Duration::from_secs(config.startup_timeout_seconds),
                call_timeout: Duration::from_secs(config.call_timeout_seconds),
                max_message_bytes,
                max_output_bytes,
            },
        )
        .await
    }

    async fn connect_inner<R, W>(
        server_name: String,
        reader: R,
        writer: W,
        child: Option<Child>,
        options: McpClientOptions,
    ) -> Result<Arc<Self>, McpError>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let client = Arc::new(Self {
            server_name,
            next_id: AtomicU64::new(1),
            connection: Mutex::new(McpConnection {
                reader: Box::new(BufReader::new(reader)),
                writer: Box::new(writer),
                child,
                poisoned: false,
            }),
            options,
        });
        let result = client
            .request_with_timeout(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "omnicli", "version": env!("CARGO_PKG_VERSION")}
                }),
                client.options.startup_timeout,
            )
            .await?;
        if result["protocolVersion"] != MCP_PROTOCOL_VERSION {
            return Err(McpError::Protocol {
                server: client.server_name.clone(),
                message: "server selected an unsupported protocol version".into(),
            });
        }
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub async fn list_tools(&self) -> Result<Vec<RemoteTool>, McpError> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::Protocol {
                server: self.server_name.clone(),
                message: "tools/list result did not contain a tools array".into(),
            })?;
        tools
            .iter()
            .map(|tool| {
                let name = tool.get("name").and_then(Value::as_str).ok_or_else(|| {
                    McpError::InvalidTool {
                        server: self.server_name.clone(),
                        message: "tool name is missing".into(),
                    }
                })?;
                if !valid_component(name) {
                    return Err(McpError::InvalidTool {
                        server: self.server_name.clone(),
                        message: format!("invalid tool name: {name}"),
                    });
                }
                let input_schema = tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"}));
                if !input_schema.is_object() {
                    return Err(McpError::InvalidTool {
                        server: self.server_name.clone(),
                        message: format!("tool {name} inputSchema is not an object"),
                    });
                }
                Ok(RemoteTool {
                    name: name.into(),
                    description: tool
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                    input_schema,
                })
            })
            .collect()
    }

    pub async fn call_tool(
        &self,
        remote_name: &str,
        arguments: Value,
    ) -> Result<ToolOutput, McpError> {
        let result = self
            .request(
                "tools/call",
                json!({"name": remote_name, "arguments": arguments}),
            )
            .await?;
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if let Some(structured) = result.get("structuredContent")
            && let Ok(output) = serde_json::from_value::<ToolOutput>(structured.clone())
        {
            return Ok(bound_output(output, self.options.max_output_bytes));
        }
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let text = content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        let (text, truncated) = truncate_text(&text, self.options.max_output_bytes);
        Ok(ToolOutput {
            success: !is_error,
            stdout: if is_error {
                String::new()
            } else {
                text.clone()
            },
            stderr: if is_error { text } else { String::new() },
            truncated,
            metadata: json!({"server": self.server_name, "tool": remote_name}),
        })
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        self.request_with_timeout(method, params, self.options.call_timeout)
            .await
    }

    async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let operation = method.to_string();
        let transaction = async {
            let mut connection = self.connection.lock().await;
            if connection.poisoned {
                return Err(McpError::Closed {
                    server: self.server_name.clone(),
                });
            }
            write_frame(
                connection.writer.as_mut(),
                &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
                self.options.max_message_bytes,
                &self.server_name,
            )
            .await?;
            loop {
                let response = read_frame(
                    connection.reader.as_mut(),
                    self.options.max_message_bytes,
                    &self.server_name,
                )
                .await?
                .ok_or_else(|| McpError::Closed {
                    server: self.server_name.clone(),
                })?;
                if response.get("id") != Some(&json!(id)) {
                    continue;
                }
                if let Some(error) = response.get("error") {
                    return Err(McpError::Remote {
                        server: self.server_name.clone(),
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(-32603),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown remote error")
                            .into(),
                    });
                }
                return response
                    .get("result")
                    .cloned()
                    .ok_or_else(|| McpError::Protocol {
                        server: self.server_name.clone(),
                        message: "response contains neither result nor error".into(),
                    });
            }
        };
        match tokio::time::timeout(timeout, transaction).await {
            Ok(result) => result,
            Err(_) => {
                let mut connection = self.connection.lock().await;
                connection.poisoned = true;
                if let Some(child) = &mut connection.child {
                    let _ = child.kill().await;
                }
                Err(McpError::Timeout {
                    server: self.server_name.clone(),
                    operation,
                })
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let mut connection = self.connection.lock().await;
        write_frame(
            connection.writer.as_mut(),
            &json!({"jsonrpc": "2.0", "method": method, "params": params}),
            self.options.max_message_bytes,
            &self.server_name,
        )
        .await
    }
}

#[derive(Clone, Debug)]
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

struct McpToolProxy {
    client: Arc<McpClient>,
    remote_name: String,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for McpToolProxy {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_request(&self, _arguments: &Value) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest::McpToolCall {
            server: self.client.server_name().into(),
            tool: self.remote_name.clone(),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        _context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        self.client
            .call_tool(&self.remote_name, arguments)
            .await
            .map_err(|error| ToolError::Mcp(error.to_string()))
    }
}

pub async fn register_configured_tools(
    registry: &mut ToolRegistry,
    config: &McpConfig,
    workspace: &Path,
    policy: &Policy,
    max_output_bytes: usize,
) -> Result<(), McpError> {
    for (server_name, server) in &config.servers {
        let decision = policy.decide(&PermissionRequest::McpProcessStart {
            server: server_name.clone(),
            command: server.command.clone(),
        });
        if !decision.allowed {
            return Err(McpError::StartDenied {
                server: server_name.clone(),
                reason: decision.reason,
            });
        }
        let client = McpClient::connect_process(
            server_name.clone(),
            server,
            workspace,
            config.max_message_bytes,
            max_output_bytes,
        )
        .await?;
        for remote in client.list_tools().await? {
            let name = format!("mcp__{server_name}__{}", remote.name);
            if name.len() > 64 || !valid_component(&name) {
                return Err(McpError::InvalidTool {
                    server: server_name.clone(),
                    message: format!("namespaced tool name is invalid: {name}"),
                });
            }
            registry.register(McpToolProxy {
                client: client.clone(),
                remote_name: remote.name,
                spec: ToolSpec {
                    name,
                    description: format!("[MCP server: {server_name}] {}", remote.description),
                    input_schema: remote.input_schema,
                },
            })?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct McpServerOptions {
    pub call_timeout: Duration,
    pub max_message_bytes: usize,
}

pub struct McpServer {
    tools: ToolRegistry,
    policy: Policy,
    context: ToolContext,
    options: McpServerOptions,
}

impl McpServer {
    pub fn new(
        tools: ToolRegistry,
        policy: Policy,
        context: ToolContext,
        options: McpServerOptions,
    ) -> Self {
        Self {
            tools,
            policy,
            context,
            options,
        }
    }

    pub async fn serve<R, W>(self, reader: R, mut writer: W) -> Result<(), McpError>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut reader = BufReader::new(reader);
        let mut initialized = false;
        while let Some(message) =
            read_frame(&mut reader, self.options.max_message_bytes, "client").await?
        {
            let method = message.get("method").and_then(Value::as_str);
            let id = message.get("id").cloned();
            if method == Some("notifications/initialized") && id.is_none() {
                initialized = true;
                continue;
            }
            let Some(id) = id else {
                continue;
            };
            let response = match method {
                Some("initialize") if !initialized => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "omnicli", "version": env!("CARGO_PKG_VERSION")}
                    }
                }),
                Some("tools/list") if initialized => {
                    let tools = self
                        .tools
                        .specs()
                        .into_iter()
                        .map(|spec| {
                            json!({
                                "name": spec.name,
                                "description": spec.description,
                                "inputSchema": spec.input_schema,
                            })
                        })
                        .collect::<Vec<_>>();
                    json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools}})
                }
                Some("tools/call") if initialized => {
                    self.handle_tool_call(id, message.get("params").cloned().unwrap_or_default())
                        .await
                }
                Some("ping") => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
                Some(_) if !initialized => rpc_error(id, -32002, "server is not initialized"),
                Some(_) => rpc_error(id, -32601, "method not found"),
                None => rpc_error(id, -32600, "invalid request"),
            };
            write_frame(
                &mut writer,
                &response,
                self.options.max_message_bytes,
                "client",
            )
            .await?;
        }
        Ok(())
    }

    async fn handle_tool_call(&self, id: Value, params: Value) -> Value {
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return rpc_error(id, -32602, "tool name is required");
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let Some(tool) = self.tools.get(name) else {
            return rpc_error(id, -32602, "unknown tool");
        };
        let output = match tool.permission_request(&arguments) {
            Ok(permission) => {
                let decision = self.policy.decide(&permission);
                if !decision.allowed {
                    error_output("permission_denied", &decision.reason)
                } else {
                    match tokio::time::timeout(
                        self.options.call_timeout,
                        tool.execute(arguments, &self.context),
                    )
                    .await
                    {
                        Ok(Ok(output)) => output,
                        Ok(Err(error)) => error_output("tool_error", &error.to_string()),
                        Err(_) => error_output("timeout", "tool call timed out"),
                    }
                }
            }
            Err(error) => error_output("invalid_arguments", &error.to_string()),
        };
        let text = if output.success {
            &output.stdout
        } else {
            &output.stderr
        };
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": text}],
                "structuredContent": output,
                "isError": !output.success,
            }
        })
    }
}

fn error_output(code: &str, message: &str) -> ToolOutput {
    ToolOutput {
        success: false,
        stdout: String::new(),
        stderr: message.into(),
        truncated: false,
        metadata: json!({"code": code}),
    }
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

async fn read_frame<R: AsyncBufRead + Unpin + ?Sized>(
    reader: &mut R,
    limit: usize,
    peer: &str,
) -> Result<Option<Value>, McpError> {
    let mut bytes = Vec::new();
    loop {
        let buffer = reader.fill_buf().await.map_err(|source| McpError::Io {
            server: peer.into(),
            source,
        })?;
        if buffer.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let take = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        if bytes.len() + take > limit {
            return Err(McpError::MessageTooLarge {
                server: peer.into(),
                limit,
            });
        }
        bytes.extend_from_slice(&buffer[..take]);
        reader.consume(take);
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            break;
        }
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| McpError::Protocol {
            server: peer.into(),
            message: error.to_string(),
        })
}

async fn write_frame<W: AsyncWrite + Unpin + ?Sized>(
    writer: &mut W,
    value: &Value,
    limit: usize,
    peer: &str,
) -> Result<(), McpError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() + 1 > limit {
        return Err(McpError::MessageTooLarge {
            server: peer.into(),
            limit,
        });
    }
    writer
        .write_all(&bytes)
        .await
        .map_err(|source| McpError::Io {
            server: peer.into(),
            source,
        })?;
    writer
        .write_all(b"\n")
        .await
        .map_err(|source| McpError::Io {
            server: peer.into(),
            source,
        })?;
    writer.flush().await.map_err(|source| McpError::Io {
        server: peer.into(),
        source,
    })
}

fn bound_output(mut output: ToolOutput, limit: usize) -> ToolOutput {
    let stdout_budget = limit.saturating_sub(limit / 4);
    let stderr_budget = limit / 4;
    let (stdout, stdout_truncated) = truncate_text(&output.stdout, stdout_budget);
    let (stderr, stderr_truncated) = truncate_text(&output.stderr, stderr_budget);
    output.stdout = stdout;
    output.stderr = stderr;
    output.truncated |= stdout_truncated || stderr_truncated;
    output
}

fn truncate_text(text: &str, limit: usize) -> (String, bool) {
    let mut end = text.len().min(limit);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].into(), text.len() > end)
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP server {server} process start denied: {reason}")]
    StartDenied { server: String, reason: String },
    #[error("failed to start MCP server {server}: {source}")]
    Spawn {
        server: String,
        source: std::io::Error,
    },
    #[error("MCP server {server} environment variable {name} is not set")]
    MissingEnvironment { server: String, name: String },
    #[error("MCP server {server} timed out during {operation}")]
    Timeout { server: String, operation: String },
    #[error("MCP server {server} closed its output")]
    Closed { server: String },
    #[error("MCP server {server} sent a message larger than {limit} bytes")]
    MessageTooLarge { server: String, limit: usize },
    #[error("invalid MCP message from {server}: {message}")]
    Protocol { server: String, message: String },
    #[error("MCP request to {server} failed ({code}): {message}")]
    Remote {
        server: String,
        code: i64,
        message: String,
    },
    #[error("invalid MCP tool from {server}: {message}")]
    InvalidTool { server: String, message: String },
    #[error("MCP transport I/O failed for {server}: {source}")]
    Io {
        server: String,
        source: std::io::Error,
    },
    #[error("failed to serialize MCP message: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Tool(#[from] ToolError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::GitReadOperation;
    use tempfile::TempDir;

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "echo".into(),
                description: "Echo one message".into(),
                input_schema: json!({
                    "type": "object",
                    "required": ["message"],
                    "properties": {"message": {"type": "string"}}
                }),
            }
        }

        fn permission_request(&self, _arguments: &Value) -> Result<PermissionRequest, ToolError> {
            Ok(PermissionRequest::GitRead {
                operation: GitReadOperation::Status,
            })
        }

        async fn execute(
            &self,
            arguments: Value,
            _context: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            let message = arguments
                .get("message")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::Mcp("message is required".into()))?;
            Ok(ToolOutput {
                success: true,
                stdout: message.into(),
                stderr: String::new(),
                truncated: false,
                metadata: json!({"echoed": true}),
            })
        }
    }

    fn context(directory: &TempDir) -> ToolContext {
        ToolContext {
            workspace: directory.path().to_path_buf(),
            max_output_bytes: 64 * 1024,
            max_file_bytes: 1024 * 1024,
            shell_timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn initialize_list_and_call_over_duplex() {
        let directory = tempfile::tempdir().unwrap();
        let mut registry = ToolRegistry::default();
        registry.register(EchoTool).unwrap();
        let server = McpServer::new(
            registry,
            Policy::new(directory.path().to_path_buf(), false, false, false),
            context(&directory),
            McpServerOptions {
                call_timeout: Duration::from_secs(5),
                max_message_bytes: 64 * 1024,
            },
        );
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (server_read, server_write) = tokio::io::split(server_io);
        let server_task = tokio::spawn(server.serve(server_read, server_write));

        let client = McpClient::connect(
            "test".into(),
            client_read,
            client_write,
            McpClientOptions {
                startup_timeout: Duration::from_secs(5),
                call_timeout: Duration::from_secs(5),
                max_message_bytes: 64 * 1024,
                max_output_bytes: 64 * 1024,
            },
        )
        .await
        .unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        let output = client
            .call_tool("echo", json!({"message": "hello MCP"}))
            .await
            .unwrap();
        assert!(output.success);
        assert_eq!(output.stdout, "hello MCP");

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn server_rechecks_write_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let server = McpServer::new(
            ToolRegistry::standard(),
            Policy::new(directory.path().to_path_buf(), false, false, false),
            context(&directory),
            McpServerOptions {
                call_timeout: Duration::from_secs(5),
                max_message_bytes: 64 * 1024,
            },
        );
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (server_read, server_write) = tokio::io::split(server_io);
        let server_task = tokio::spawn(server.serve(server_read, server_write));
        let client = McpClient::connect(
            "test".into(),
            client_read,
            client_write,
            McpClientOptions {
                startup_timeout: Duration::from_secs(5),
                call_timeout: Duration::from_secs(5),
                max_message_bytes: 64 * 1024,
                max_output_bytes: 64 * 1024,
            },
        )
        .await
        .unwrap();

        let output = client
            .call_tool(
                "create_file",
                json!({"path": "denied.txt", "content": "no"}),
            )
            .await
            .unwrap();
        assert!(!output.success);
        assert_eq!(output.metadata["code"], "permission_denied");
        assert!(!directory.path().join("denied.txt").exists());

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn bounded_reader_rejects_oversized_frames() {
        let data = vec![b'x'; 33];
        let mut reader = BufReader::new(std::io::Cursor::new(data));
        let error = read_frame(&mut reader, 32, "test").await.unwrap_err();
        assert!(matches!(error, McpError::MessageTooLarge { .. }));
    }
}
