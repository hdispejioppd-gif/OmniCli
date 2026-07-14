use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
    time::timeout,
};
use uuid::Uuid;

use crate::{
    permission::PermissionRequest,
    protocol::{ToolOutput, ToolSpec},
    tools::{Tool, ToolContext, ToolError, ToolRegistry},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginConfig {
    pub manifest: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub startup_timeout_seconds: u64,
    pub call_timeout_seconds: u64,
    pub max_output_bytes: usize,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            manifest: PathBuf::new(),
            args: Vec::new(),
            env: Vec::new(),
            startup_timeout_seconds: 10,
            call_timeout_seconds: 30,
            max_output_bytes: 64 * 1024,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginManifest {
    pub plugin: PluginMeta,
    pub permissions: PluginPermissions,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    pub description: String,
    pub entrypoint: PathBuf,
}

impl Default for PluginMeta {
    fn default() -> Self {
        Self {
            name: String::new(),
            version: "0.0.0".into(),
            description: String::new(),
            entrypoint: PathBuf::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginPermissions {
    pub read: bool,
    pub write: bool,
    pub shell: bool,
    pub network: bool,
}

#[derive(Clone, Debug)]
pub struct LoadedPlugin {
    pub name: String,
    pub version: String,
    pub description: String,
    pub manifest_dir: PathBuf,
    pub permissions: PluginPermissions,
    pub tools: Vec<ToolSpec>,
    pub host: Arc<PluginHost>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Value,
    method: String,
    params: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct InitializeResult {
    name: String,
    version: String,
    #[serde(default)]
    description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ToolCallParams {
    tool: String,
    arguments: Value,
}

pub struct PluginHost {
    name: String,
    stdin: Mutex<BufWriter<ChildStdin>>,
    stdout: Mutex<BufReader<ChildStdout>>,
    child: Mutex<Child>,
    call_timeout: Duration,
    max_output_bytes: usize,
}

impl std::fmt::Debug for PluginHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginHost")
            .field("name", &self.name)
            .field("call_timeout", &self.call_timeout)
            .field("max_output_bytes", &self.max_output_bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin manifest not found: {0}")]
    ManifestNotFound(PathBuf),
    #[error("failed to read manifest: {source}")]
    ManifestRead { source: std::io::Error },
    #[error("failed to parse manifest: {0}")]
    ManifestParse(toml::de::Error),
    #[error("invalid plugin name: {0}")]
    InvalidName(String),
    #[error("invalid entrypoint: {0}")]
    InvalidEntrypoint(PathBuf),
    #[error("plugin process failed to start: {0}")]
    Spawn(std::io::Error),
    #[error("plugin initialization failed: {0}")]
    Initialize(String),
    #[error("plugin communication failed: {0}")]
    Communication(String),
    #[error("plugin call timed out after {0}s")]
    Timeout(u64),
    #[error("plugin returned error: {0}")]
    ToolError(String),
    #[error("plugin output too large")]
    OutputTooLarge,
    #[error("duplicate plugin name: {0}")]
    DuplicateName(String),
    #[error("plugin process died")]
    ProcessDied,
}

impl LoadedPlugin {
    pub fn namespaced_name(&self, tool_name: &str) -> String {
        format!("plugin__{}__{}", self.name, tool_name)
    }

    pub fn tool_display_name(&self, namespaced: &str) -> Option<String> {
        let prefix = format!("plugin__{}__", self.name);
        namespaced.strip_prefix(&prefix).map(String::from)
    }
}

impl PluginHost {
    pub async fn start(
        name: &str,
        manifest_dir: &Path,
        config: &PluginConfig,
        manifest: &PluginManifest,
    ) -> Result<Arc<Self>, PluginError> {
        if !valid_plugin_name(name) {
            return Err(PluginError::InvalidName(name.into()));
        }

        let entrypoint = manifest_dir.join(&manifest.plugin.entrypoint);
        if !entrypoint.exists() {
            return Err(PluginError::InvalidEntrypoint(entrypoint));
        }

        let (program, extra_args) = interpreter_for(&entrypoint);
        let mut command = Command::new(program);
        for arg in extra_args {
            command.arg(arg);
        }
        command
            .arg(&entrypoint)
            .arg("serve")
            .args(&config.args)
            .current_dir(manifest_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear();

        for variable in &config.env {
            if let Ok(value) = std::env::var(variable) {
                command.env(variable, value);
            }
        }

        let mut child = command.spawn().map_err(PluginError::Spawn)?;
        let stdin = BufWriter::new(
            child
                .stdin
                .take()
                .ok_or_else(|| PluginError::Spawn(std::io::Error::other("missing stdin")))?,
        );
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| PluginError::Spawn(std::io::Error::other("missing stdout")))?,
        );

        let host = Arc::new(Self {
            name: name.into(),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            child: Mutex::new(child),
            call_timeout: Duration::from_secs(config.call_timeout_seconds),
            max_output_bytes: config.max_output_bytes,
        });

        let init_timeout = Duration::from_secs(config.startup_timeout_seconds);
        let result: InitializeResult = host
            .request("initialize", json!({}), init_timeout)
            .await
            .map_err(|error| PluginError::Initialize(error.to_string()))?
            .ok_or_else(|| PluginError::Initialize("missing initialize result".into()))?;

        if result.name != name {
            return Err(PluginError::Initialize(format!(
                "plugin reported name '{}' but config expects '{}'",
                result.name, name
            )));
        }

        Ok(host)
    }

    pub async fn list_tools(&self) -> Result<Vec<ToolSpec>, PluginError> {
        let response: Vec<ToolSpec> = self
            .request("tools/list", json!({}), self.call_timeout)
            .await?
            .ok_or_else(|| PluginError::Communication("tools/list missing result".into()))?;
        Ok(response)
    }

    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<ToolOutput, PluginError> {
        let output: ToolOutput = self
            .request(
                "tools/call",
                json!(ToolCallParams {
                    tool: tool.into(),
                    arguments,
                }),
                self.call_timeout,
            )
            .await?
            .ok_or_else(|| PluginError::Communication("tools/call missing result".into()))?;
        Ok(output)
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Option<T>, PluginError> {
        let id = Uuid::now_v7().to_string();
        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: json!(id),
            method: method.into(),
            params,
        };
        let line = serde_json::to_string(&request).map_err(|error| {
            PluginError::Communication(format!("failed to serialize request: {error}"))
        })?;

        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| PluginError::Communication(error.to_string()))?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|error| PluginError::Communication(error.to_string()))?;
        stdin
            .flush()
            .await
            .map_err(|error| PluginError::Communication(error.to_string()))?;
        drop(stdin);

        timeout(deadline, self.read_response::<T>(&id))
            .await
            .unwrap_or_else(|_| Err(PluginError::Timeout(deadline.as_secs())))
    }

    async fn read_response<T: serde::de::DeserializeOwned>(
        &self,
        expected_id: &str,
    ) -> Result<Option<T>, PluginError> {
        let mut stdout = self.stdout.lock().await;
        let mut line = String::new();
        let bytes_read = stdout
            .read_line(&mut line)
            .await
            .map_err(|error| PluginError::Communication(error.to_string()))?;
        if bytes_read == 0 {
            return Err(PluginError::ProcessDied);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let response: JsonRpcResponse = serde_json::from_str(trimmed).map_err(|error| {
            PluginError::Communication(format!("failed to parse response: {error}"))
        })?;

        if response.id.as_str() != Some(expected_id) {
            return Err(PluginError::Communication(format!(
                "response id mismatch: expected {expected_id}, got {}",
                response.id
            )));
        }

        if let Some(error) = response.error {
            return Err(PluginError::ToolError(error.message));
        }

        match response.result {
            None => Ok(None),
            Some(value) => {
                let parsed = serde_json::from_value(value).map_err(|error| {
                    PluginError::Communication(format!("failed to deserialize result: {error}"))
                })?;
                Ok(Some(parsed))
            }
        }
    }

    pub async fn shutdown(&self) {
        let _ = self
            .request::<Value>("shutdown", json!({}), Duration::from_secs(2))
            .await;
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
    }
}

#[derive(Clone, Debug, Default)]
pub struct PluginRegistry {
    plugins: HashMap<String, LoadedPlugin>,
}

impl PluginRegistry {
    pub async fn load_from_config(
        configs: &BTreeMap<String, PluginConfig>,
    ) -> Result<Self, PluginError> {
        let mut registry = Self::default();
        for (name, config) in configs {
            registry.load(name, config).await?;
        }
        Ok(registry)
    }

    pub async fn load(&mut self, name: &str, config: &PluginConfig) -> Result<(), PluginError> {
        if !valid_plugin_name(name) {
            return Err(PluginError::InvalidName(name.into()));
        }
        if self.plugins.contains_key(name) {
            return Err(PluginError::DuplicateName(name.into()));
        }

        let manifest_path = &config.manifest;
        if !manifest_path.exists() {
            return Err(PluginError::ManifestNotFound(manifest_path.clone()));
        }
        let raw = std::fs::read_to_string(manifest_path)
            .map_err(|source| PluginError::ManifestRead { source })?;
        let manifest: PluginManifest = toml::from_str(&raw).map_err(PluginError::ManifestParse)?;
        if manifest.plugin.name != name {
            return Err(PluginError::Initialize(format!(
                "manifest name '{}' does not match configured plugin name '{}'",
                manifest.plugin.name, name
            )));
        }

        let manifest_dir = manifest_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let host = PluginHost::start(name, &manifest_dir, config, &manifest).await?;
        let tools = match host.list_tools().await {
            Ok(tools) => tools,
            Err(error) => {
                host.shutdown().await;
                return Err(error);
            }
        };

        let plugin = LoadedPlugin {
            name: name.into(),
            version: manifest.plugin.version.clone(),
            description: manifest.plugin.description.clone(),
            manifest_dir,
            permissions: manifest.permissions.clone(),
            tools,
            host,
        };

        self.plugins.insert(name.into(), plugin);
        Ok(())
    }

    pub fn register_tools(&self, registry: &mut ToolRegistry) {
        for plugin in self.plugins.values() {
            for spec in &plugin.tools {
                let namespaced = plugin.namespaced_name(&spec.name);
                let tool = PluginTool {
                    plugin_name: plugin.name.clone(),
                    spec: ToolSpec {
                        name: namespaced,
                        description: format!("[{}] {}", plugin.name, spec.description),
                        input_schema: spec.input_schema.clone(),
                    },
                    host: plugin.host.clone(),
                    inner_name: spec.name.clone(),
                };
                let _ = registry.register(tool);
            }
        }
    }

    pub fn list(&self) -> Vec<&LoadedPlugin> {
        self.plugins.values().collect()
    }

    pub fn get(&self, name: &str) -> Option<&LoadedPlugin> {
        self.plugins.get(name)
    }

    pub async fn shutdown_all(&self) {
        for plugin in self.plugins.values() {
            plugin.host.shutdown().await;
        }
    }
}

struct PluginTool {
    plugin_name: String,
    spec: ToolSpec,
    host: Arc<PluginHost>,
    inner_name: String,
}

#[async_trait]
impl Tool for PluginTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_request(&self, _arguments: &Value) -> Result<PermissionRequest, ToolError> {
        Ok(PermissionRequest::PluginCall {
            plugin: self.plugin_name.clone(),
            tool: self.inner_name.clone(),
        })
    }

    async fn execute(
        &self,
        arguments: Value,
        _context: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let output = self
            .host
            .call_tool(&self.inner_name, arguments)
            .await
            .map_err(|error| ToolError::Plugin(error.to_string()))?;
        Ok(output)
    }
}

pub fn valid_plugin_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn interpreter_for(path: &Path) -> (String, Vec<String>) {
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    match extension {
        "py" => (
            find_executable(&["python3", "python", "py"]).unwrap_or_else(|| "python".into()),
            Vec::new(),
        ),
        "js" => (
            find_executable(&["node"]).unwrap_or_else(|| "node".into()),
            Vec::new(),
        ),
        "ts" => (
            find_executable(&["tsx", "ts-node"]).unwrap_or_else(|| "tsx".into()),
            Vec::new(),
        ),
        _ => {
            let program = path.to_string_lossy().into_owned();
            (program, Vec::new())
        }
    }
}

fn find_executable(candidates: &[&str]) -> Option<String> {
    for candidate in candidates {
        if std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
        {
            return Some((*candidate).into());
        }
    }
    None
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginListEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub tools: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_minimal_manifest() {
        let dir = TempDir::new().unwrap();
        let manifest_path = dir.path().join("omni-plugin.toml");
        std::fs::write(
            &manifest_path,
            r#"
[plugin]
name = "demo"
version = "0.1.0"
description = "Demo plugin"
entrypoint = "demo.py"

[permissions]
read = true
"#,
        )
        .unwrap();
        let raw = std::fs::read_to_string(&manifest_path).unwrap();
        let manifest: PluginManifest = toml::from_str(&raw).unwrap();
        assert_eq!(manifest.plugin.name, "demo");
        assert!(manifest.permissions.read);
    }

    #[test]
    fn plugin_name_validation_rejects_empty_and_special() {
        assert!(!valid_plugin_name(""));
        assert!(!valid_plugin_name("demo plugin"));
        assert!(!valid_plugin_name("demo/slash"));
        assert!(valid_plugin_name("demo-plugin_2"));
    }

    #[tokio::test]
    async fn subprocess_plugin_lists_and_calls_tools() {
        let python = which_python();
        if python.is_none() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let plugin_py = dir.path().join("plugin.py");
        std::fs::write(
            &plugin_py,
            r#"#!/usr/bin/env python3
import json, sys

def send(v): print(json.dumps(v), flush=True)

def main():
    for line in sys.stdin:
        req = json.loads(line)
        mid = req.get("id")
        method = req.get("method")
        if method == "initialize":
            send({"jsonrpc":"2.0","id":mid,"result":{"name":"test","version":"1.0.0"}})
        elif method == "tools/list":
            send({"jsonrpc":"2.0","id":mid,"result":[{"name":"echo","description":"echo","input_schema":{"type":"object","additionalProperties":False,"required":["text"],"properties":{"text":{"type":"string"}}}}]})
        elif method == "tools/call":
            text = req["params"].get("arguments",{}).get("text","")
            send({"jsonrpc":"2.0","id":mid,"result":{"success":True,"stdout":text,"stderr":"","truncated":False,"metadata":{}}})
        else:
            send({"jsonrpc":"2.0","id":mid,"error":{"code":-32601,"message":"unknown"}})

if __name__ == "__main__":
    main()
"#,
        )
        .unwrap();

        let manifest_path = dir.path().join("omni-plugin.toml");
        std::fs::write(
            &manifest_path,
            r#"
[plugin]
name = "test"
version = "1.0.0"
description = "test"
entrypoint = "plugin.py"
"#,
        )
        .unwrap();

        let config = PluginConfig {
            manifest: manifest_path,
            call_timeout_seconds: 5,
            startup_timeout_seconds: 5,
            ..Default::default()
        };

        let registry = PluginRegistry::load_from_config(&{
            let mut map = std::collections::BTreeMap::new();
            map.insert("test".into(), config);
            map
        })
        .await
        .unwrap();

        let plugin = registry.get("test").unwrap();
        assert_eq!(plugin.tools.len(), 1);
        assert_eq!(plugin.tools[0].name, "echo");

        let output = plugin
            .host
            .call_tool("echo", json!({"text": "hello plugin"}))
            .await
            .unwrap();
        assert!(output.success);
        assert_eq!(output.stdout, "hello plugin");

        registry.shutdown_all().await;
    }

    fn which_python() -> Option<String> {
        for cmd in ["python3", "python"] {
            if std::process::Command::new(cmd)
                .arg("--version")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
            {
                return Some(cmd.into());
            }
        }
        None
    }
}
