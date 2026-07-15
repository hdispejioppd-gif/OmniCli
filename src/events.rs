use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::protocol::{Message, ToolCall, ToolOutput, Usage};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunEvent {
    pub schema_version: u16,
    pub session_id: String,
    pub run_id: String,
    pub sequence: u64,
    #[serde(flatten)]
    pub kind: RunEventKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEventKind {
    RunStarted,
    SystemMessage {
        message: Message,
    },
    UserMessage {
        message: Message,
    },
    ModelTextDelta {
        text: String,
    },
    ToolCallRequested {
        call: ToolCall,
    },
    DiffPreview {
        path: String,
        summary: String,
        diff: String,
    },
    PermissionResolved {
        allowed: bool,
        reason: String,
    },
    ToolFinished {
        call_id: String,
        output: ToolOutput,
    },
    AssistantMessage {
        message: Message,
    },
    Usage {
        usage: Usage,
    },
    RunFinished,
    Failed {
        code: String,
        message: String,
    },
}

impl RunEvent {
    pub fn new(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        sequence: u64,
        kind: RunEventKind,
    ) -> Self {
        Self {
            schema_version: 1,
            session_id: session_id.into(),
            run_id: run_id.into(),
            sequence,
            kind,
        }
    }

    pub fn as_json(&self) -> Result<Value, EventError> {
        serde_json::to_value(self).map_err(EventError::Serialize)
    }
}

#[async_trait]
pub trait EventSink: Send + Sync {
    async fn emit(&self, event: &RunEvent) -> Result<(), EventError>;
}

#[derive(Default)]
pub struct RecordingSink {
    events: Mutex<Vec<RunEvent>>,
}

impl RecordingSink {
    pub fn events(&self) -> Vec<RunEvent> {
        self.events.lock().expect("recording sink poisoned").clone()
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn emit(&self, event: &RunEvent) -> Result<(), EventError> {
        self.events
            .lock()
            .map_err(|_| EventError::Sink("recording sink poisoned".into()))?
            .push(event.clone());
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum EventError {
    #[error("event serialization failed: {0}")]
    Serialize(serde_json::Error),
    #[error("event sink failed: {0}")]
    Sink(String),
}
