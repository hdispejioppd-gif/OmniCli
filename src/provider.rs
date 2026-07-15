use std::{collections::BTreeMap, pin::Pin, sync::Arc, time::Duration};

use async_stream::try_stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt, stream};
use reqwest::{
    StatusCode, Url,
    header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::protocol::{Message, ModelEvent, ModelRequest, Role, ToolCall, ToolSpec, Usage};

pub type ModelStream =
    Pin<Box<dyn Stream<Item = Result<ModelEvent, ProviderError>> + Send + 'static>>;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn respond(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ProviderError>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModelSpec {
    Fake,
    OpenAi {
        base_url: String,
        model: String,
        timeout: Duration,
    },
    OpenAiCompatible {
        base_url: String,
        model: String,
        timeout: Duration,
    },
    Custom {
        name: String,
        base_url: String,
        model: String,
        timeout: Duration,
        api_key_env: String,
    },
    Anthropic {
        base_url: String,
        model: String,
        timeout: Duration,
        api_version: String,
    },
    Ollama {
        base_url: String,
        model: String,
        timeout: Duration,
    },
    LmStudio {
        base_url: String,
        model: String,
        timeout: Duration,
    },
    LlamaCpp {
        base_url: String,
        model: String,
        timeout: Duration,
        temperature: f32,
        n_predict: i32,
    },
}

impl ModelSpec {
    pub fn selector(&self) -> String {
        match self {
            Self::Fake => "fake".into(),
            Self::OpenAi { model, .. } => format!("openai/{model}"),
            Self::OpenAiCompatible { model, .. } => format!("openai-compatible/{model}"),
            Self::Custom { name, model, .. } => format!("{name}/{model}"),
            Self::Anthropic { model, .. } => format!("anthropic/{model}"),
            Self::Ollama { model, .. } => format!("ollama/{model}"),
            Self::LmStudio { model, .. } => format!("lm-studio/{model}"),
            Self::LlamaCpp { model, .. } => format!("llama-cpp/{model}"),
        }
    }
}

pub struct ProviderFactory {
    openai_api_key: Option<Arc<str>>,
    openai_compatible_api_key: Option<Arc<str>>,
    anthropic_api_key: Option<Arc<str>>,
}

impl ProviderFactory {
    pub fn from_env() -> Self {
        Self {
            openai_api_key: std::env::var("OPENAI_API_KEY").ok().map(Arc::from),
            openai_compatible_api_key: std::env::var("OPENAI_COMPATIBLE_API_KEY")
                .ok()
                .map(Arc::from),
            anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok().map(Arc::from),
        }
    }

    pub fn new(openai_api_key: Option<String>) -> Self {
        Self {
            openai_api_key: openai_api_key.map(Arc::from),
            openai_compatible_api_key: None,
            anthropic_api_key: None,
        }
    }

    pub fn build(&self, spec: &ModelSpec) -> Result<Arc<dyn ModelProvider>, ProviderError> {
        match spec {
            ModelSpec::Fake => Ok(Arc::new(FakeProvider)),
            ModelSpec::Ollama {
                base_url,
                model,
                timeout,
            } => {
                if model.trim().is_empty() {
                    return Err(ProviderError::InvalidModel);
                }
                Ok(Arc::new(OllamaProvider::new(base_url, model, *timeout)?))
            }
            ModelSpec::LmStudio {
                base_url,
                model,
                timeout,
            } => {
                if model.trim().is_empty() {
                    return Err(ProviderError::InvalidModel);
                }
                Ok(Arc::new(OpenAiProvider::new(
                    base_url, model, *timeout, "",
                )?))
            }
            ModelSpec::LlamaCpp {
                base_url,
                model,
                timeout,
                temperature,
                n_predict,
            } => {
                if model.trim().is_empty() {
                    return Err(ProviderError::InvalidModel);
                }
                Ok(Arc::new(LlamaCppProvider::new(
                    base_url,
                    model,
                    *timeout,
                    *temperature,
                    *n_predict,
                )?))
            }
            ModelSpec::OpenAi {
                base_url,
                model,
                timeout,
            } => {
                if model.trim().is_empty() {
                    return Err(ProviderError::InvalidModel);
                }
                let api_key = self
                    .openai_api_key
                    .as_deref()
                    .ok_or(ProviderError::MissingApiKey)?;
                Ok(Arc::new(OpenAiProvider::new(
                    base_url, model, *timeout, api_key,
                )?))
            }
            ModelSpec::Custom {
                base_url,
                model,
                timeout,
                api_key_env,
                ..
            } => {
                if model.trim().is_empty() {
                    return Err(ProviderError::InvalidModel);
                }
                let api_key = if api_key_env.trim().is_empty() {
                    String::new()
                } else {
                    std::env::var(api_key_env).map_err(|_| ProviderError::MissingApiKey)?
                };
                Ok(Arc::new(OpenAiProvider::new(
                    base_url, model, *timeout, &api_key,
                )?))
            }
            ModelSpec::OpenAiCompatible {
                base_url,
                model,
                timeout,
            } => {
                if model.trim().is_empty() {
                    return Err(ProviderError::InvalidModel);
                }
                let api_key = self
                    .openai_compatible_api_key
                    .as_deref()
                    .ok_or(ProviderError::MissingApiKey)?;
                Ok(Arc::new(OpenAiProvider::new(
                    base_url, model, *timeout, api_key,
                )?))
            }
            ModelSpec::Anthropic {
                base_url,
                model,
                timeout,
                api_version,
            } => {
                if model.trim().is_empty() {
                    return Err(ProviderError::InvalidModel);
                }
                let api_key = self
                    .anthropic_api_key
                    .as_deref()
                    .ok_or(ProviderError::MissingApiKey)?;
                Ok(Arc::new(AnthropicProvider::new(
                    base_url,
                    model,
                    *timeout,
                    api_version,
                    api_key,
                )?))
            }
        }
    }
}

#[derive(Default)]
pub struct FakeProvider;

#[async_trait]
impl ModelProvider for FakeProvider {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn respond(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let last = request
            .messages
            .last()
            .ok_or(ProviderError::EmptyConversation)?;

        let events = if last.role == Role::Tool {
            text_events(format!("Tool result:\n{}", last.content))
        } else {
            let prompt = last.content.trim();
            if prompt == "status" {
                tool_events("git_status", json!({}))
            } else if prompt == "diff" {
                tool_events("git_diff", json!({}))
            } else if prompt == "checks" {
                tool_events("run_checks", json!({}))
            } else if prompt == "map" {
                tool_events("repo_map", json!({}))
            } else if let Some(query) = prompt.strip_prefix("search_code ") {
                tool_events("search_code", json!({ "query": query.trim() }))
            } else if let Some(query) = prompt.strip_prefix("search_files ") {
                tool_events("search_files", json!({ "query": query.trim() }))
            } else if let Some(query) = prompt.strip_prefix("search ") {
                tool_events("search_files", json!({ "query": query.trim() }))
            } else if let Some(query) = prompt.strip_prefix("web_search ") {
                tool_events("web_search", json!({ "query": query.trim() }))
            } else if let Some(url) = prompt.strip_prefix("fetch ") {
                tool_events("web_fetch", json!({ "url": url.trim() }))
            } else if let Some(rest) = prompt.strip_prefix("mcp ") {
                let (name, arguments) = rest.split_once("::").ok_or_else(|| {
                    ProviderError::InvalidPrompt(
                        "mcp syntax is: mcp <namespaced-tool>::<json-arguments>".into(),
                    )
                })?;
                tool_events(name.trim(), serde_json::from_str(arguments.trim())?)
            } else if let Some(query) = prompt.strip_prefix("search ") {
                tool_events("search_files", json!({ "query": query.trim() }))
            } else if let Some(path) = prompt.strip_prefix("read ") {
                tool_events("read_file", json!({ "path": path.trim() }))
            } else if let Some(command) = prompt.strip_prefix("shell ") {
                tool_events("shell", json!({ "command": command.trim() }))
            } else if let Some(rest) = prompt.strip_prefix("write ") {
                let (path, content) = rest.split_once("::").ok_or_else(|| {
                    ProviderError::InvalidPrompt("write syntax is: write <path>::<content>".into())
                })?;
                tool_events(
                    "create_file",
                    json!({ "path": path.trim(), "content": content }),
                )
            } else if let Some(rest) = prompt.strip_prefix("patch ") {
                let parts = rest.splitn(4, "::").collect::<Vec<_>>();
                if parts.len() != 4 {
                    return Err(ProviderError::InvalidPrompt(
                        "patch syntax is: patch <path>::<sha256>::<old>::<new>".into(),
                    ));
                }
                tool_events(
                    "apply_patch",
                    json!({
                        "path": parts[0].trim(),
                        "expected_sha256": parts[1].trim(),
                        "old_text": parts[2],
                        "new_text": parts[3],
                    }),
                )
            } else if let Some(rest) = prompt.strip_prefix("plugin ") {
                let (name, arguments) = rest.split_once("::").ok_or_else(|| {
                    ProviderError::InvalidPrompt(
                        "plugin syntax is: plugin <namespaced-tool>::<json-arguments>".into(),
                    )
                })?;
                tool_events(name.trim(), serde_json::from_str(arguments.trim())?)
            } else {
                text_events(prompt.to_string())
            }
        };

        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

pub struct OpenAiProvider {
    client: reqwest::Client,
    endpoint: Url,
    model: String,
}

impl OpenAiProvider {
    pub fn from_env(
        base_url: &str,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| ProviderError::MissingApiKey)?;
        Self::new(base_url, model, timeout, &api_key)
    }

    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        timeout: Duration,
        api_key: &str,
    ) -> Result<Self, ProviderError> {
        let normalized_base = format!("{}/", base_url.trim_end_matches('/'));
        let base = Url::parse(&normalized_base)
            .map_err(|error| ProviderError::InvalidEndpoint(error.to_string()))?;
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(ProviderError::InvalidEndpoint(
                "userinfo, query, and fragment are not allowed".into(),
            ));
        }
        let endpoint = base
            .join("chat/completions")
            .map_err(|error| ProviderError::InvalidEndpoint(error.to_string()))?;
        let mut headers = HeaderMap::new();
        if !api_key.is_empty() {
            let mut authorization = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|_| ProviderError::InvalidApiKey)?;
            authorization.set_sensitive(true);
            headers.insert(AUTHORIZATION, authorization);
        }
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()?;
        Ok(Self {
            client,
            endpoint,
            model: model.into(),
        })
    }
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    async fn respond(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let body = WireRequest::from_model_request(&self.model, request)?;
        let send = self.client.post(self.endpoint.clone()).json(&body).send();
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(ProviderError::Cancelled),
            response = send => response?,
        };
        if !response.status().is_success() {
            let status = response.status();
            return Err(ProviderError::HttpStatus { status });
        }

        let mut source = response.bytes_stream().eventsource();
        let output = try_stream! {
            let mut calls = BTreeMap::<u32, ToolCallAccumulator>::new();
            let mut finished = false;
            loop {
                let next = tokio::select! {
                    _ = cancellation.cancelled() => Err(ProviderError::Cancelled),
                    event = source.next() => Ok(event),
                }?;
                let Some(event) = next else { break };
                let event = event.map_err(|error| ProviderError::Stream(error.to_string()))?;
                if event.data == "[DONE]" {
                    for (index, call) in calls {
                        yield ModelEvent::ToolCall(call.finish(index)?);
                    }
                    yield ModelEvent::Finished;
                    finished = true;
                    break;
                }

                let chunk: WireChunk = serde_json::from_str(&event.data)?;
                if let Some(error) = chunk.error {
                    Err(ProviderError::Api(error.message))?;
                }
                if let Some(usage) = chunk.usage {
                    yield ModelEvent::Usage(Usage {
                        input_tokens: usage.prompt_tokens,
                        output_tokens: usage.completion_tokens,
                        total_tokens: usage.total_tokens,
                    });
                }
                for choice in chunk.choices.into_iter().filter(|choice| choice.index == 0) {
                    if let Some(reason) = choice.finish_reason.as_deref()
                        && matches!(reason, "length" | "content_filter")
                    {
                        Err(ProviderError::Incomplete(reason.to_string()))?;
                    }
                    if let Some(content) = choice.delta.content.filter(|text| !text.is_empty()) {
                        yield ModelEvent::TextDelta(content);
                    }
                    for fragment in choice.delta.tool_calls {
                        let accumulator = calls.entry(fragment.index).or_default();
                        if let Some(id) = fragment.id {
                            accumulator.id.push_str(&id);
                        }
                        if let Some(function) = fragment.function {
                            if let Some(name) = function.name {
                                accumulator.name.push_str(&name);
                            }
                            if let Some(arguments) = function.arguments {
                                accumulator.arguments.push_str(&arguments);
                            }
                        }
                    }
                    if choice.finish_reason.as_deref() == Some("tool_calls") {
                        let indices: Vec<u32> = calls.keys().copied().collect();
                        for index in indices {
                            let call = calls.remove(&index).expect("index exists");
                            yield ModelEvent::ToolCall(call.finish(index)?);
                        }
                    }
                }
            }
            if !finished {
                Err(ProviderError::UnexpectedEof)?;
            }
        };
        Ok(Box::pin(output))
    }
}

#[derive(Serialize)]
struct WireRequest {
    model: String,
    stream: bool,
    n: u8,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool>,
}

impl WireRequest {
    fn from_model_request(model: &str, request: ModelRequest) -> Result<Self, ProviderError> {
        Ok(Self {
            model: model.into(),
            stream: true,
            n: 1,
            messages: request
                .messages
                .into_iter()
                .map(WireMessage::try_from)
                .collect::<Result<_, _>>()?,
            tools: request.tools.into_iter().map(WireTool::from).collect(),
        })
    }
}

#[derive(Serialize)]
struct WireMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<WireToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

impl TryFrom<Message> for WireMessage {
    type Error = ProviderError;

    fn try_from(message: Message) -> Result<Self, Self::Error> {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let tool_calls = message
            .tool_calls
            .into_iter()
            .map(WireToolCall::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let content = if role == "assistant" && !tool_calls.is_empty() && message.content.is_empty()
        {
            None
        } else {
            Some(message.content)
        };
        Ok(Self {
            role,
            content,
            tool_calls,
            tool_call_id: message.tool_call_id,
        })
    }
}

#[derive(Serialize)]
struct WireToolCall {
    id: String,
    r#type: &'static str,
    function: WireFunctionCall,
}

impl TryFrom<ToolCall> for WireToolCall {
    type Error = ProviderError;

    fn try_from(call: ToolCall) -> Result<Self, Self::Error> {
        Ok(Self {
            id: call.id,
            r#type: "function",
            function: WireFunctionCall {
                name: call.name,
                arguments: serde_json::to_string(&call.arguments)?,
            },
        })
    }
}

#[derive(Serialize)]
struct WireFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct WireTool {
    r#type: &'static str,
    function: WireFunction,
}

impl From<ToolSpec> for WireTool {
    fn from(spec: ToolSpec) -> Self {
        Self {
            r#type: "function",
            function: WireFunction {
                name: spec.name,
                description: spec.description,
                parameters: spec.input_schema,
            },
        }
    }
}

#[derive(Serialize)]
struct WireFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Deserialize)]
struct WireChunk {
    #[serde(default)]
    choices: Vec<WireChoice>,
    error: Option<WireApiError>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

#[derive(Deserialize)]
struct WireApiError {
    message: String,
}

#[derive(Deserialize)]
struct WireChoice {
    index: u32,
    #[serde(default)]
    delta: WireDelta,
    finish_reason: Option<String>,
}

#[derive(Default, Deserialize)]
struct WireDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCallFragment>,
}

#[derive(Deserialize)]
struct WireToolCallFragment {
    index: u32,
    id: Option<String>,
    function: Option<WireFunctionFragment>,
}

#[derive(Deserialize)]
struct WireFunctionFragment {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCallAccumulator {
    fn finish(self, index: u32) -> Result<ToolCall, ProviderError> {
        if self.id.is_empty() {
            return Err(ProviderError::InvalidToolCall {
                index,
                reason: "missing id".into(),
            });
        }
        if self.name.is_empty() {
            return Err(ProviderError::InvalidToolCall {
                index,
                reason: "missing function name".into(),
            });
        }
        let arguments = serde_json::from_str(&self.arguments).map_err(|error| {
            ProviderError::InvalidToolCall {
                index,
                reason: format!("invalid arguments: {error}"),
            }
        })?;
        Ok(ToolCall {
            id: self.id,
            name: self.name,
            arguments,
        })
    }
}

fn tool_events(name: &str, arguments: Value) -> Vec<ModelEvent> {
    vec![
        ModelEvent::ToolCall(ToolCall {
            id: Uuid::now_v7().to_string(),
            name: name.into(),
            arguments,
        }),
        ModelEvent::Finished,
    ]
}

fn text_events(text: String) -> Vec<ModelEvent> {
    let mut events = text
        .split_inclusive(char::is_whitespace)
        .map(|chunk| ModelEvent::TextDelta(chunk.to_owned()))
        .collect::<Vec<_>>();
    events.push(ModelEvent::Finished);
    events
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("conversation is empty")]
    EmptyConversation,
    #[error("invalid prompt: {0}")]
    InvalidPrompt(String),
    #[error("model name must not be empty")]
    InvalidModel,
    #[error("provider API key is not set")]
    MissingApiKey,
    #[error("invalid OpenAI-compatible endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("API key cannot be encoded as an HTTP header")]
    InvalidApiKey,
    #[error("provider request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("provider returned HTTP {status}")]
    HttpStatus { status: StatusCode },
    #[error("provider stream failed: {0}")]
    Stream(String),
    #[error("invalid provider JSON: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("provider API error: {0}")]
    Api(String),
    #[error("invalid tool call at index {index}: {reason}")]
    InvalidToolCall { index: u32, reason: String },
    #[error("provider ended generation with {0}")]
    Incomplete(String),
    #[error("provider stream ended before [DONE]")]
    UnexpectedEof,
    #[error("provider request was cancelled")]
    Cancelled,
}

pub struct AnthropicProvider {
    client: reqwest::Client,
    endpoint: Url,
    model: String,
    api_version: String,
}

impl AnthropicProvider {
    pub fn from_env(
        base_url: &str,
        model: impl Into<String>,
        timeout: Duration,
        api_version: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let api_key =
            std::env::var("ANTHROPIC_API_KEY").map_err(|_| ProviderError::MissingApiKey)?;
        Self::new(base_url, model, timeout, api_version, &api_key)
    }

    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        timeout: Duration,
        api_version: impl Into<String>,
        api_key: &str,
    ) -> Result<Self, ProviderError> {
        let normalized_base = format!("{}/", base_url.trim_end_matches('/'));
        let base = Url::parse(&normalized_base)
            .map_err(|error| ProviderError::InvalidEndpoint(error.to_string()))?;
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(ProviderError::InvalidEndpoint(
                "userinfo, query, and fragment are not allowed".into(),
            ));
        }
        let endpoint = base
            .join("messages")
            .map_err(|error| ProviderError::InvalidEndpoint(error.to_string()))?;
        let mut api_key_header =
            HeaderValue::from_str(api_key).map_err(|_| ProviderError::InvalidApiKey)?;
        api_key_header.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", api_key_header);
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()?;
        Ok(Self {
            client,
            endpoint,
            model: model.into(),
            api_version: api_version.into(),
        })
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn respond(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let body = AnthropicRequest::from_model_request(&self.model, request)?;
        let send = self
            .client
            .post(self.endpoint.clone())
            .header("anthropic-version", self.api_version.clone())
            .json(&body)
            .send();
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(ProviderError::Cancelled),
            response = send => response?,
        };
        if !response.status().is_success() {
            let status = response.status();
            return Err(ProviderError::HttpStatus { status });
        }

        let mut source = response.bytes_stream().eventsource();
        let output = try_stream! {
            let mut blocks: Vec<AnthropicBlock> = Vec::new();
            let mut finished = false;
            loop {
                let next = tokio::select! {
                    _ = cancellation.cancelled() => Err(ProviderError::Cancelled),
                    event = source.next() => Ok(event),
                }?;
                let Some(event) = next else { break };
                let event = event.map_err(|error| ProviderError::Stream(error.to_string()))?;

                if event.event == "error" {
                    let error: AnthropicError = serde_json::from_str(&event.data)?;
                    Err(ProviderError::Api(error.error.message))?;
                }

                if event.data.is_empty() {
                    continue;
                }
                let data: AnthropicEvent = serde_json::from_str(&event.data)?;
                match data {
                    AnthropicEvent::MessageStart { .. } => {}
                    AnthropicEvent::ContentBlockStart { index, content_block } => {
                        if blocks.len() <= index {
                            blocks.resize(index + 1, AnthropicBlock::Text(String::new()));
                        }
                        blocks[index] = match content_block {
                            AnthropicContentBlock::Text { text } => {
                                if !text.is_empty() {
                                    yield ModelEvent::TextDelta(text.clone());
                                }
                                AnthropicBlock::Text(text)
                            }
                            AnthropicContentBlock::ToolUse { id, name, .. } => {
                                AnthropicBlock::ToolUse {
                                    id,
                                    name,
                                    input: String::new(),
                                }
                            }
                        };
                    }
                    AnthropicEvent::ContentBlockDelta { index, delta } => {
                        if let Some(block) = blocks.get_mut(index) {
                            match (block, delta) {
                                (AnthropicBlock::Text(text), AnthropicDelta::TextDelta { delta }) => {
                                    text.push_str(&delta);
                                    if !delta.is_empty() {
                                        yield ModelEvent::TextDelta(delta);
                                    }
                                }
                                (AnthropicBlock::ToolUse { input, .. }, AnthropicDelta::InputJsonDelta { partial_json }) => {
                                    input.push_str(&partial_json);
                                }
                                _ => {}
                            }
                        }
                    }
                    AnthropicEvent::ContentBlockStop { index } => {
                        if let Some(AnthropicBlock::ToolUse { id, name, input }) = blocks.get(index)
                        {
                            let arguments = serde_json::from_str(input).map_err(|error| {
                                ProviderError::InvalidToolCall {
                                    index: index as u32,
                                    reason: format!("invalid arguments: {error}"),
                                }
                            })?;
                            yield ModelEvent::ToolCall(ToolCall {
                                id: id.clone(),
                                name: name.clone(),
                                arguments,
                            });
                        }
                    }
                    AnthropicEvent::MessageDelta { delta, usage } => {
                        if matches!(delta.stop_reason.as_deref(), Some("max_tokens")) {
                            Err(ProviderError::Incomplete("max_tokens".into()))?;
                        }
                        if let Some(usage) = usage {
                            yield ModelEvent::Usage(Usage {
                                input_tokens: usage.input_tokens,
                                output_tokens: usage.output_tokens,
                                total_tokens: usage.input_tokens + usage.output_tokens,
                            });
                        }
                    }
                    AnthropicEvent::MessageStop => {
                        finished = true;
                        yield ModelEvent::Finished;
                        break;
                    }
                    AnthropicEvent::Ping => {}
                }
            }
            if !finished {
                Err(ProviderError::UnexpectedEof)?;
            }
        };
        Ok(Box::pin(output))
    }
}

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    system: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
}

impl AnthropicRequest {
    fn from_model_request(model: &str, request: ModelRequest) -> Result<Self, ProviderError> {
        let mut system = String::new();
        let mut messages = Vec::new();
        for message in request.messages {
            match message.role {
                Role::System => {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(&message.content);
                }
                Role::Tool => messages.push(AnthropicMessage {
                    role: "user",
                    content: vec![AnthropicContent::ToolResult {
                        tool_use_id: message.tool_call_id.unwrap_or_default(),
                        content: message.content,
                    }],
                }),
                Role::User => messages.push(AnthropicMessage {
                    role: "user",
                    content: vec![AnthropicContent::Text {
                        text: message.content,
                    }],
                }),
                Role::Assistant => {
                    if message.tool_calls.is_empty() {
                        messages.push(AnthropicMessage {
                            role: "assistant",
                            content: vec![AnthropicContent::Text {
                                text: message.content,
                            }],
                        });
                    } else {
                        let mut content = Vec::new();
                        if !message.content.is_empty() {
                            content.push(AnthropicContent::Text {
                                text: message.content,
                            });
                        }
                        for call in message.tool_calls {
                            content.push(AnthropicContent::ToolUse {
                                id: call.id,
                                name: call.name,
                                input: call.arguments,
                            });
                        }
                        messages.push(AnthropicMessage {
                            role: "assistant",
                            content,
                        });
                    }
                }
            }
        }
        Ok(Self {
            model: model.into(),
            max_tokens: 4096,
            stream: true,
            system,
            messages,
            tools: request.tools.into_iter().map(AnthropicTool::from).collect(),
        })
    }
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: &'static str,
    content: Vec<AnthropicContent>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContent {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

impl From<ToolSpec> for AnthropicTool {
    fn from(spec: ToolSpec) -> Self {
        Self {
            name: spec.name,
            description: spec.description,
            input_schema: spec.input_schema,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicEvent {
    MessageStart {
        #[serde(rename = "message")]
        _message: AnthropicMessageStart,
    },
    ContentBlockStart {
        index: usize,
        content_block: AnthropicContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: AnthropicDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: AnthropicMessageDelta,
        #[serde(rename = "usage")]
        usage: Option<AnthropicUsage>,
    },
    MessageStop,
    Ping,
}

#[derive(Deserialize)]
struct AnthropicMessageStart {
    #[allow(dead_code)]
    role: String,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(rename = "input")]
        _input: Value,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicDelta {
    TextDelta { delta: String },
    InputJsonDelta { partial_json: String },
}

#[derive(Deserialize)]
struct AnthropicMessageDelta {
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Deserialize)]
struct AnthropicError {
    error: AnthropicErrorDetail,
}

#[derive(Deserialize)]
struct AnthropicErrorDetail {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

enum AnthropicBlock {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        input: String,
    },
}

impl Clone for AnthropicBlock {
    fn clone(&self) -> Self {
        match self {
            Self::Text(text) => Self::Text(text.clone()),
            Self::ToolUse { id, name, input } => Self::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
        }
    }
}

impl Default for AnthropicBlock {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

pub struct OllamaProvider {
    client: reqwest::Client,
    endpoint: Url,
    model: String,
}

impl OllamaProvider {
    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let normalized_base = format!("{}/", base_url.trim_end_matches('/'));
        let base = Url::parse(&normalized_base)
            .map_err(|error| ProviderError::InvalidEndpoint(error.to_string()))?;
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(ProviderError::InvalidEndpoint(
                "userinfo, query, and fragment are not allowed".into(),
            ));
        }
        let endpoint = base
            .join("api/chat")
            .map_err(|error| ProviderError::InvalidEndpoint(error.to_string()))?;
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/x-ndjson"));
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()?;
        Ok(Self {
            client,
            endpoint,
            model: model.into(),
        })
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn name(&self) -> &'static str {
        "ollama"
    }

    async fn respond(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let body = OllamaRequest::from_model_request(&self.model, request)?;
        let send = self.client.post(self.endpoint.clone()).json(&body).send();
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(ProviderError::Cancelled),
            response = send => response?,
        };
        if !response.status().is_success() {
            let status = response.status();
            return Err(ProviderError::HttpStatus { status });
        }

        let output = try_stream! {
            let mut stream = response.bytes_stream();
            let mut finished = false;
            loop {
                let next = tokio::select! {
                    _ = cancellation.cancelled() => Err(ProviderError::Cancelled),
                    item = stream.next() => Ok(item),
                }?;
                let Some(chunk) = next else { break };
                let chunk = chunk.map_err(|error| ProviderError::Stream(error.to_string()))?;
                let text = String::from_utf8_lossy(&chunk);
                for line in text.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let event: OllamaEvent = serde_json::from_str(line)?;
                    if let Some(error) = event.error {
                        Err(ProviderError::Api(error))?;
                    }
                    if let Some(message) = event.message {
                        if !message.content.is_empty() {
                            yield ModelEvent::TextDelta(message.content);
                        }
                        for call in message.tool_calls.unwrap_or_default() {
                            yield ModelEvent::ToolCall(ToolCall {
                                id: Uuid::now_v7().to_string(),
                                name: call.function.name,
                                arguments: call.function.arguments,
                            });
                        }
                    }
                    if event.prompt_eval_count > 0 || event.eval_count > 0 {
                        yield ModelEvent::Usage(Usage {
                            input_tokens: event.prompt_eval_count,
                            output_tokens: event.eval_count,
                            total_tokens: event.prompt_eval_count + event.eval_count,
                        });
                    }
                    if event.done {
                        finished = true;
                        yield ModelEvent::Finished;
                        break;
                    }
                }
                if finished {
                    break;
                }
            }
            if !finished {
                Err(ProviderError::UnexpectedEof)?;
            }
        };
        Ok(Box::pin(output))
    }
}

pub struct LlamaCppProvider {
    client: reqwest::Client,
    endpoint: Url,
    #[allow(dead_code)]
    model: String,
    temperature: f32,
    n_predict: i32,
}

impl LlamaCppProvider {
    pub fn new(
        base_url: &str,
        model: impl Into<String>,
        timeout: Duration,
        temperature: f32,
        n_predict: i32,
    ) -> Result<Self, ProviderError> {
        let normalized_base = format!("{}/", base_url.trim_end_matches('/'));
        let base = Url::parse(&normalized_base)
            .map_err(|error| ProviderError::InvalidEndpoint(error.to_string()))?;
        if !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(ProviderError::InvalidEndpoint(
                "userinfo, query, and fragment are not allowed".into(),
            ));
        }
        let endpoint = base
            .join("completion")
            .map_err(|error| ProviderError::InvalidEndpoint(error.to_string()))?;
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()?;
        Ok(Self {
            client,
            endpoint,
            model: model.into(),
            temperature,
            n_predict,
        })
    }

    fn build_prompt(messages: Vec<Message>) -> String {
        let mut prompt = String::new();
        for message in messages {
            let role = match message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "user",
            };
            prompt.push_str("<|im_start|>");
            prompt.push_str(role);
            prompt.push('\n');
            if !message.content.is_empty() {
                prompt.push_str(&message.content);
                prompt.push('\n');
            }
            if !message.tool_calls.is_empty() {
                for call in message.tool_calls {
                    prompt.push_str("<tool_call>\n");
                    prompt.push_str(&call.name);
                    prompt.push('\n');
                    if let Ok(arguments) = serde_json::to_string(&call.arguments) {
                        prompt.push_str(&arguments);
                        prompt.push('\n');
                    }
                    prompt.push_str("</tool_call>\n");
                }
            }
            prompt.push_str("<|im_end|>\n");
        }
        prompt.push_str("<|im_start|>assistant\n");
        prompt
    }
}

#[derive(Serialize)]
struct LlamaCppRequest {
    prompt: String,
    stream: bool,
    temperature: f32,
    n_predict: i32,
    stop: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    grammar: Option<String>,
}

#[derive(Deserialize, Default)]
struct LlamaCppEvent {
    #[serde(default)]
    content: String,
    #[serde(default)]
    stop: bool,
}

#[async_trait]
impl ModelProvider for LlamaCppProvider {
    fn name(&self) -> &'static str {
        "llama-cpp"
    }

    async fn respond(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let prompt = Self::build_prompt(request.messages);
        let grammar = request
            .response_schema
            .as_ref()
            .and_then(|s| crate::schema::json_schema_to_gbnf(s).ok());
        let body = LlamaCppRequest {
            prompt,
            stream: true,
            temperature: self.temperature,
            n_predict: self.n_predict,
            stop: vec!["<|im_end|>", "<|im_start|>"],
            grammar,
        };
        let send = self.client.post(self.endpoint.clone()).json(&body).send();
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(ProviderError::Cancelled),
            response = send => response?,
        };
        if !response.status().is_success() {
            let status = response.status();
            return Err(ProviderError::HttpStatus { status });
        }

        let output = try_stream! {
            let mut stream = response.bytes_stream().eventsource();
            let mut finished = false;
            loop {
                let next = tokio::select! {
                    _ = cancellation.cancelled() => Err(ProviderError::Cancelled),
                    event = stream.next() => Ok(event),
                }?;
                let Some(event) = next else { break };
                let event = event.map_err(|error| ProviderError::Stream(error.to_string()))?;
                if event.data == "[DONE]" {
                    yield ModelEvent::Finished;
                    finished = true;
                    break;
                }
                let line = event.data.strip_prefix("data: ").unwrap_or(&event.data);
                if line.trim().is_empty() {
                    continue;
                }
                let chunk: LlamaCppEvent = serde_json::from_str(line)?;
                if !chunk.content.is_empty() {
                    yield ModelEvent::TextDelta(chunk.content);
                }
                if chunk.stop {
                    yield ModelEvent::Finished;
                    finished = true;
                    break;
                }
            }
            if !finished {
                Err(ProviderError::UnexpectedEof)?;
            }
        };
        Ok(Box::pin(output))
    }
}

#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    stream: bool,
    messages: Vec<OllamaMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OllamaTool>,
}

impl OllamaRequest {
    fn from_model_request(model: &str, request: ModelRequest) -> Result<Self, ProviderError> {
        Ok(Self {
            model: model.into(),
            stream: true,
            messages: request
                .messages
                .into_iter()
                .map(OllamaMessage::try_from)
                .collect::<Result<_, _>>()?,
            tools: request.tools.into_iter().map(OllamaTool::from).collect(),
        })
    }
}

#[derive(Serialize)]
struct OllamaMessage {
    role: &'static str,
    content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<OllamaToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

impl TryFrom<Message> for OllamaMessage {
    type Error = ProviderError;

    fn try_from(message: Message) -> Result<Self, Self::Error> {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let tool_calls = message
            .tool_calls
            .into_iter()
            .map(OllamaToolCall::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            role,
            content: message.content,
            tool_calls,
            tool_call_id: message.tool_call_id,
        })
    }
}

#[derive(Serialize)]
struct OllamaToolCall {
    function: OllamaFunctionCall,
}

impl TryFrom<ToolCall> for OllamaToolCall {
    type Error = ProviderError;

    fn try_from(call: ToolCall) -> Result<Self, Self::Error> {
        Ok(Self {
            function: OllamaFunctionCall {
                name: call.name,
                arguments: call.arguments,
            },
        })
    }
}

#[derive(Serialize)]
struct OllamaFunctionCall {
    name: String,
    arguments: Value,
}

#[derive(Serialize)]
struct OllamaTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OllamaFunction,
}

impl From<ToolSpec> for OllamaTool {
    fn from(spec: ToolSpec) -> Self {
        Self {
            kind: "function",
            function: OllamaFunction {
                name: spec.name,
                description: spec.description,
                parameters: spec.input_schema,
            },
        }
    }
}

#[derive(Serialize)]
struct OllamaFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Deserialize)]
struct OllamaEvent {
    #[serde(default)]
    message: Option<OllamaResponseMessage>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    prompt_eval_count: u64,
    #[serde(default)]
    eval_count: u64,
}

#[derive(Deserialize)]
struct OllamaResponseMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Option<Vec<OllamaResponseToolCall>>,
}

#[derive(Deserialize)]
struct OllamaResponseToolCall {
    function: OllamaResponseFunction,
}

#[derive(Deserialize)]
struct OllamaResponseFunction {
    name: String,
    arguments: Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::TryStreamExt;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path},
    };

    #[tokio::test]
    async fn openai_stream_assembles_text_and_fragmented_tool_call() {
        let server = MockServer::start().await;
        let body = [
            r#"data: {"choices":[{"index":0,"delta":{"content":"Hi "},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_","arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
            r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"file","arguments":"\"README.md\"}"}}]},"finish_reason":"tool_calls"}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
            "data: [DONE]",
        ]
        .join("\n\n");
        let body = format!("{body}\n\n");

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_json(json!({
                "model": "test-model",
                "stream": true,
                "n": 1,
                "messages": [{"role": "user", "content": "read the file"}]
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(&body),
            )
            .mount(&server)
            .await;

        eprintln!("BODY: {body:?}");

        let provider = OpenAiProvider::new(
            &format!("{}/v1/", server.uri()),
            "test-model",
            Duration::from_secs(5),
            "test-key",
        )
        .unwrap();
        let stream = provider
            .respond(
                ModelRequest {
                    messages: vec![Message::new(Role::User, "read the file")],
                    tools: vec![],
                    response_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let events = stream.try_collect::<Vec<_>>().await.unwrap();
        eprintln!("BODY: {body:?}");
        eprintln!("EVENTS: {events:?}");

        assert!(matches!(&events[0], ModelEvent::TextDelta(text) if text == "Hi "));
        assert!(matches!(
            &events[1],
            ModelEvent::ToolCall(call)
                if call.id == "call_1"
                    && call.name == "read_file"
                    && call.arguments == json!({"path": "README.md"})
        ));
        assert!(matches!(
            &events[2],
            ModelEvent::Usage(usage)
                if usage.input_tokens == 10 && usage.output_tokens == 5 && usage.total_tokens == 15
        ));
        assert!(matches!(events[3], ModelEvent::Finished));
    }

    #[tokio::test]
    async fn openai_provider_rejects_http_errors_without_leaking_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid credentials"))
            .mount(&server)
            .await;
        let provider = OpenAiProvider::new(
            &format!("{}/", server.uri()),
            "test-model",
            Duration::from_secs(5),
            "super-secret",
        )
        .unwrap();
        let error = provider
            .respond(
                ModelRequest {
                    messages: vec![Message::new(Role::User, "hello")],
                    tools: vec![],
                    response_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .err()
            .expect("request should fail");
        let rendered = error.to_string();
        assert!(rendered.contains("401"));
        assert!(!rendered.contains("super-secret"));
        assert!(!rendered.contains("invalid credentials"));
    }

    #[tokio::test]
    async fn anthropic_stream_assembles_text_and_tool_use() {
        let server = MockServer::start().await;
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"role\":\"assistant\",\"content\":[]}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"Hi \"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"delta\":\"there\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\": \\\"README.md\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
            "event: message_stop\n",
            "data: {\"type\":\"message_stop\"}\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let provider = AnthropicProvider::new(
            &format!("{}/v1/", server.uri()),
            "claude-test",
            Duration::from_secs(5),
            "2023-06-01",
            "test-key",
        )
        .unwrap();
        let stream = provider
            .respond(
                ModelRequest {
                    messages: vec![Message::new(Role::User, "hello")],
                    tools: vec![ToolSpec {
                        name: "read_file".into(),
                        description: "read".into(),
                        input_schema: json!({"type": "object", "properties": {}}),
                    }],
                    response_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let events: Vec<ModelEvent> = stream.try_collect().await.unwrap();
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], ModelEvent::TextDelta(text) if text == "Hi "));
        assert!(matches!(&events[1], ModelEvent::TextDelta(text) if text == "there"));
        assert!(matches!(
            &events[2],
            ModelEvent::ToolCall(call)
                if call.id == "tu_1"
                    && call.name == "read_file"
                    && call.arguments == json!({"path": "README.md"})
        ));
        assert!(matches!(events[3], ModelEvent::Finished));
    }

    #[tokio::test]
    async fn ollama_stream_assembles_text_and_tool_call() {
        let server = MockServer::start().await;
        let body = concat!(
            "{\"message\":{\"role\":\"assistant\",\"content\":\"Hi \"},\"done\":false}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":\"there\"},\"done\":false}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":[{\"function\":{\"name\":\"read_file\",\"arguments\":{\"path\":\"README.md\"}}}]},\"done\":true}\n"
        );
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_json(json!({
                "model": "llama-test",
                "stream": true,
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [{
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "description": "read",
                        "parameters": {"type": "object", "properties": {}}
                    }
                }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(
            &format!("{}/", server.uri()),
            "llama-test",
            Duration::from_secs(5),
        )
        .unwrap();
        let stream = provider
            .respond(
                ModelRequest {
                    messages: vec![Message::new(Role::User, "hello")],
                    tools: vec![ToolSpec {
                        name: "read_file".into(),
                        description: "read".into(),
                        input_schema: json!({"type": "object", "properties": {}}),
                    }],
                    response_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let events: Vec<ModelEvent> = stream.try_collect().await.unwrap();
        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], ModelEvent::TextDelta(text) if text == "Hi "));
        assert!(matches!(&events[1], ModelEvent::TextDelta(text) if text == "there"));
        assert!(matches!(
            &events[2],
            ModelEvent::ToolCall(call)
                if call.name == "read_file"
                    && call.arguments == json!({"path": "README.md"})
        ));
        assert!(matches!(events[3], ModelEvent::Finished));
    }

    #[tokio::test]
    async fn llama_cpp_stream_assembles_text() {
        let server = MockServer::start().await;
        let body = [
            "data: {\"content\":\"Hi \",\"stop\":false}",
            "data: {\"content\":\"there\",\"stop\":false}",
            "data: {\"content\":\"\",\"stop\":true}",
            "data: [DONE]",
        ]
        .join("\n\n");
        let body = format!("{body}\n\n");

        Mock::given(method("POST"))
            .and(path("/completion"))
            .and(body_json(json!({
                "stream": true,
                "temperature": 0.7,
                "n_predict": -1,
                "stop": ["<|im_end|>", "<|im_start|>"],
                "prompt": "<|im_start|>user\nhello\n<|im_end|>\n<|im_start|>assistant\n"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(&body),
            )
            .mount(&server)
            .await;

        let provider =
            LlamaCppProvider::new(&server.uri(), "local", Duration::from_secs(5), 0.7, -1).unwrap();
        let stream = provider
            .respond(
                ModelRequest {
                    messages: vec![Message::new(Role::User, "hello")],
                    tools: vec![],
                    response_schema: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let events: Vec<ModelEvent> = stream.try_collect().await.unwrap();
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], ModelEvent::TextDelta(text) if text == "Hi "));
        assert!(matches!(&events[1], ModelEvent::TextDelta(text) if text == "there"));
        assert!(matches!(events[2], ModelEvent::Finished));
    }

    #[test]
    fn provider_factory_builds_only_configured_safe_specs() {
        let factory = ProviderFactory::new(None);
        assert_eq!(factory.build(&ModelSpec::Fake).unwrap().name(), "fake");
        let ollama = ModelSpec::Ollama {
            base_url: "http://localhost:11434".into(),
            model: "llama3.1".into(),
            timeout: Duration::from_secs(5),
        };
        assert_eq!(factory.build(&ollama).unwrap().name(), "ollama");
        let openai = ModelSpec::OpenAi {
            base_url: "https://example.invalid/v1/".into(),
            model: "model".into(),
            timeout: Duration::from_secs(5),
        };
        assert!(matches!(
            factory.build(&openai),
            Err(ProviderError::MissingApiKey)
        ));
        let openai_compatible = ModelSpec::OpenAiCompatible {
            base_url: "https://example.invalid/v1/".into(),
            model: "model".into(),
            timeout: Duration::from_secs(5),
        };
        assert!(matches!(
            factory.build(&openai_compatible),
            Err(ProviderError::MissingApiKey)
        ));
        let custom_keyless = ModelSpec::Custom {
            name: "vllm".into(),
            base_url: "http://localhost:8000/v1/".into(),
            model: "local".into(),
            timeout: Duration::from_secs(5),
            api_key_env: String::new(),
        };
        assert_eq!(custom_keyless.selector(), "vllm/local");
        assert!(factory.build(&custom_keyless).is_ok());
        let custom_with_missing_key = ModelSpec::Custom {
            name: "openrouter".into(),
            base_url: "https://example.invalid/v1/".into(),
            model: "model".into(),
            timeout: Duration::from_secs(5),
            api_key_env: "OMNI_TEST_KEY_THAT_DOES_NOT_EXIST".into(),
        };
        assert!(matches!(
            factory.build(&custom_with_missing_key),
            Err(ProviderError::MissingApiKey)
        ));
        let anthropic = ModelSpec::Anthropic {
            base_url: "https://example.invalid/v1/".into(),
            model: "model".into(),
            timeout: Duration::from_secs(5),
            api_version: "2023-06-01".into(),
        };
        assert!(matches!(
            factory.build(&anthropic),
            Err(ProviderError::MissingApiKey)
        ));

        let lm_studio = ModelSpec::LmStudio {
            base_url: "http://localhost:1234/v1/".into(),
            model: "local-model".into(),
            timeout: Duration::from_secs(5),
        };
        assert_eq!(factory.build(&lm_studio).unwrap().name(), "openai");
        let llama_cpp = ModelSpec::LlamaCpp {
            base_url: "http://localhost:8080".into(),
            model: "local-model".into(),
            timeout: Duration::from_secs(5),
            temperature: 0.7,
            n_predict: -1,
        };
        assert_eq!(factory.build(&llama_cpp).unwrap().name(), "llama-cpp");

        let factory = ProviderFactory::new(Some("test-key".into()));
        let unsafe_url = ModelSpec::OpenAi {
            base_url: "https://user:secret@example.invalid/v1/".into(),
            model: "model".into(),
            timeout: Duration::from_secs(5),
        };
        assert!(matches!(
            factory.build(&unsafe_url),
            Err(ProviderError::InvalidEndpoint(_))
        ));
    }
}
