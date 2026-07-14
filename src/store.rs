use std::{
    fs,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    events::RunEvent,
    protocol::{Message, Role},
};

pub struct SqliteStore {
    connection: Mutex<Connection>,
}

#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct StoredSession {
    pub summary: SessionSummary,
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug)]
pub struct WorkflowRunRecord {
    pub id: String,
    pub workspace_path: String,
    pub workflow_path: String,
    pub fingerprint: String,
    pub status: String,
    pub lease_owner: Option<String>,
    pub lease_expires_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub steps: Vec<StoredWorkflowStep>,
}

#[derive(Clone, Debug)]
pub struct WorkflowRunSummary {
    pub id: String,
    pub workflow_path: String,
    pub status: String,
    pub lease_expires_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug)]
pub struct StoredWorkflowStep {
    pub step_id: String,
    pub ordinal: usize,
    pub tool: Option<String>,
    pub needs: Vec<String>,
    pub artifacts: Vec<String>,
    pub status: String,
    pub attempts: u32,
    pub report_json: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewSupervisorTask {
    pub id: String,
    pub worktree: String,
    pub workspace_path: String,
    pub verify: bool,
    pub prompt_sha256: String,
    pub idempotent: bool,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupervisorTaskRecord {
    pub id: String,
    pub worktree: String,
    pub workspace_path: String,
    pub verify: bool,
    pub idempotent: bool,
    pub prompt_sha256: String,
    pub session_id: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupervisorRunRecord {
    pub id: String,
    pub task_file: String,
    pub root_workspace: String,
    pub fingerprint: String,
    pub parent_run_id: Option<String>,
    pub model: String,
    pub status: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub tasks: Vec<SupervisorTaskRecord>,
}

impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                provider TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                sequence INTEGER NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                tool_calls_json TEXT NOT NULL DEFAULT '[]',
                tool_call_id TEXT,
                created_at_ms INTEGER NOT NULL,
                UNIQUE(session_id, sequence)
            );
            CREATE TABLE IF NOT EXISTS run_events (
                run_id TEXT NOT NULL,
                sequence INTEGER NOT NULL,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                event_json TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                PRIMARY KEY(run_id, sequence)
            );
            CREATE TABLE IF NOT EXISTS workflow_runs (
                id TEXT PRIMARY KEY,
                workspace_path TEXT NOT NULL,
                workflow_path TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                status TEXT NOT NULL,
                lease_owner TEXT,
                lease_expires_at_ms INTEGER,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER
            );
            CREATE TABLE IF NOT EXISTS workflow_step_reports (
                run_id TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
                step_id TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                status TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                report_json TEXT,
                tool TEXT,
                needs_json TEXT NOT NULL DEFAULT '[]',
                artifacts_json TEXT NOT NULL DEFAULT '[]',
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(run_id, step_id),
                UNIQUE(run_id, ordinal)
            );
            CREATE TABLE IF NOT EXISTS supervisor_runs (
                id TEXT PRIMARY KEY,
                root_workspace_path TEXT NOT NULL,
                task_file_path TEXT NOT NULL,
                model_selector TEXT NOT NULL,
                concurrency INTEGER NOT NULL,
                status TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER
            );
            CREATE TABLE IF NOT EXISTS supervisor_tasks (
                run_id TEXT NOT NULL REFERENCES supervisor_runs(id) ON DELETE CASCADE,
                task_id TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                worktree_name TEXT NOT NULL,
                workspace_path TEXT NOT NULL,
                verify INTEGER NOT NULL,
                prompt_sha256 TEXT NOT NULL,
                session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id),
                status TEXT NOT NULL,
                error_message TEXT,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY(run_id, task_id),
                UNIQUE(run_id, ordinal)
            );",
        )?;
        connection.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS supervisor_active_worktree
             ON supervisor_tasks(workspace_path)
             WHERE status IN ('pending', 'running');",
        )?;
        for (column, migration) in [
            (
                "fingerprint",
                "ALTER TABLE supervisor_runs ADD COLUMN fingerprint TEXT NOT NULL DEFAULT ''",
            ),
            (
                "parent_run_id",
                "ALTER TABLE supervisor_runs ADD COLUMN parent_run_id TEXT",
            ),
            (
                "lease_owner",
                "ALTER TABLE supervisor_runs ADD COLUMN lease_owner TEXT",
            ),
            (
                "lease_expires_at_ms",
                "ALTER TABLE supervisor_runs ADD COLUMN lease_expires_at_ms INTEGER",
            ),
        ] {
            ensure_column(&connection, "supervisor_runs", column, migration)?;
        }
        ensure_column(
            &connection,
            "supervisor_tasks",
            "idempotent",
            "ALTER TABLE supervisor_tasks ADD COLUMN idempotent INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &connection,
            "messages",
            "tool_calls_json",
            "ALTER TABLE messages ADD COLUMN tool_calls_json TEXT NOT NULL DEFAULT '[]'",
        )?;
        ensure_column(
            &connection,
            "workflow_step_reports",
            "artifacts_json",
            "ALTER TABLE workflow_step_reports ADD COLUMN artifacts_json TEXT NOT NULL DEFAULT '[]'",
        )?;
        ensure_column(
            &connection,
            "workflow_step_reports",
            "tool",
            "ALTER TABLE workflow_step_reports ADD COLUMN tool TEXT",
        )?;
        ensure_column(
            &connection,
            "workflow_step_reports",
            "needs_json",
            "ALTER TABLE workflow_step_reports ADD COLUMN needs_json TEXT NOT NULL DEFAULT '[]'",
        )?;
        ensure_column(
            &connection,
            "messages",
            "tool_call_id",
            "ALTER TABLE messages ADD COLUMN tool_call_id TEXT",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn create_session(&self, title: &str, provider: &str) -> Result<String, StoreError> {
        let id = Uuid::now_v7().to_string();
        let now = now_ms()?;
        self.connection()?.execute(
            "INSERT INTO sessions (id, title, provider, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, title, provider, now],
        )?;
        Ok(id)
    }

    pub fn session_exists(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
            [id],
            |row| row.get(0),
        )?)
    }

    pub fn append_message(&self, session_id: &str, message: &Message) -> Result<(), StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM messages WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )?;
        let now = now_ms()?;
        transaction.execute(
            "INSERT INTO messages
             (id, session_id, sequence, role, content, tool_calls_json, tool_call_id, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                Uuid::now_v7().to_string(),
                session_id,
                sequence,
                role_name(&message.role),
                &message.content,
                serde_json::to_string(&message.tool_calls)?,
                &message.tool_call_id,
                now
            ],
        )?;
        transaction.execute(
            "UPDATE sessions SET updated_at_ms = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn load_messages(&self, session_id: &str) -> Result<Vec<Message>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT role, content, tool_calls_json, tool_call_id
             FROM messages WHERE session_id = ?1 ORDER BY sequence",
        )?;
        let rows = statement.query_map([session_id], |row| {
            let role: String = row.get(0)?;
            let content: String = row.get(1)?;
            let tool_calls_json: String = row.get(2)?;
            let tool_call_id: Option<String> = row.get(3)?;
            let tool_calls = serde_json::from_str(&tool_calls_json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    tool_calls_json.len(),
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(Message {
                role: parse_role(&role),
                content,
                tool_calls,
                tool_call_id,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Database)
    }

    pub fn append_event(&self, event: &RunEvent) -> Result<(), StoreError> {
        self.connection()?.execute(
            "INSERT INTO run_events (run_id, sequence, session_id, event_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.run_id,
                event.sequence,
                event.session_id,
                serde_json::to_string(event)?,
                now_ms()?
            ],
        )?;
        Ok(())
    }

    pub fn load_run_events(&self, session_id: &str) -> Result<Vec<RunEvent>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT event_json FROM run_events WHERE session_id = ?1 ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map([session_id], |row| {
            let json: String = row.get(0)?;
            serde_json::from_str(&json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Database)
    }

    pub fn list_sessions(&self, limit: u32) -> Result<Vec<SessionSummary>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, title, updated_at_ms FROM sessions ORDER BY updated_at_ms DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            Ok(SessionSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                updated_at_ms: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Database)
    }

    pub fn session_title(&self, id: &str) -> Result<Option<String>, StoreError> {
        self.connection()?
            .query_row("SELECT title FROM sessions WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(StoreError::Database)
    }

    pub fn load_session(&self, id: &str) -> Result<Option<StoredSession>, StoreError> {
        let Some(title) = self.session_title(id)? else {
            return Ok(None);
        };
        let updated_at_ms = self.connection()?.query_row(
            "SELECT updated_at_ms FROM sessions WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(Some(StoredSession {
            summary: SessionSummary {
                id: id.into(),
                title,
                updated_at_ms,
            },
            messages: self.load_messages(id)?,
        }))
    }

    pub fn create_workflow_run(
        &self,
        workspace: &Path,
        workflow_path: &Path,
        fingerprint: &str,
        step_ids: &[String],
        lease_owner: &str,
        lease_expires_at_ms: i64,
    ) -> Result<String, StoreError> {
        let id = Uuid::now_v7().to_string();
        let now = now_ms()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO workflow_runs
             (id, workspace_path, workflow_path, fingerprint, status, lease_owner,
              lease_expires_at_ms, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, ?7)",
            params![
                id,
                workspace.to_string_lossy(),
                workflow_path.to_string_lossy(),
                fingerprint,
                lease_owner,
                lease_expires_at_ms,
                now,
            ],
        )?;
        for (ordinal, step_id) in step_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO workflow_step_reports
                 (run_id, step_id, ordinal, status, attempts, report_json, updated_at_ms)
                 VALUES (?1, ?2, ?3, 'pending', 0, NULL, ?4)",
                params![id, step_id, ordinal, now],
            )?;
        }
        transaction.commit()?;
        Ok(id)
    }

    pub fn load_workflow_run(&self, id: &str) -> Result<Option<WorkflowRunRecord>, StoreError> {
        let connection = self.connection()?;
        let run = connection
            .query_row(
                "SELECT id, workspace_path, workflow_path, fingerprint, status,
                        lease_owner, lease_expires_at_ms, created_at_ms, updated_at_ms
                 FROM workflow_runs WHERE id = ?1",
                [id],
                |row| {
                    Ok(WorkflowRunRecord {
                        id: row.get(0)?,
                        workspace_path: row.get(1)?,
                        workflow_path: row.get(2)?,
                        fingerprint: row.get(3)?,
                        status: row.get(4)?,
                        lease_owner: row.get(5)?,
                        lease_expires_at_ms: row.get(6)?,
                        created_at_ms: row.get(7)?,
                        updated_at_ms: row.get(8)?,
                        steps: Vec::new(),
                    })
                },
            )
            .optional()?;
        let Some(mut run) = run else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT step_id, ordinal, tool, needs_json, artifacts_json, status, attempts, report_json
             FROM workflow_step_reports WHERE run_id = ?1 ORDER BY ordinal",
        )?;
        run.steps = statement
            .query_map([id], |row| {
                let ordinal: i64 = row.get(1)?;
                Ok(StoredWorkflowStep {
                    step_id: row.get(0)?,
                    ordinal: usize::try_from(ordinal).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            8,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                    tool: row.get(2)?,
                    needs: serde_json::from_str(&row.get::<_, String>(3)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    artifacts: serde_json::from_str(&row.get::<_, String>(4)?).map_err(
                        |error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                4,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        },
                    )?,
                    status: row.get(5)?,
                    attempts: row.get(6)?,
                    report_json: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(run))
    }

    pub fn set_workflow_step_metadata(
        &self,
        run_id: &str,
        step_id: &str,
        tool: &str,
        needs: &[String],
        artifacts: &[String],
        owner: &str,
    ) -> Result<(), StoreError> {
        let updated = self.connection()?.execute(
            "UPDATE workflow_step_reports
             SET tool = ?1, needs_json = ?2, artifacts_json = ?3, updated_at_ms = ?4
             WHERE run_id = ?5 AND step_id = ?6
               AND EXISTS (SELECT 1 FROM workflow_runs WHERE id = ?5 AND lease_owner = ?7)",
            params![
                tool,
                serde_json::to_string(needs)?,
                serde_json::to_string(artifacts)?,
                now_ms()?,
                run_id,
                step_id,
                owner,
            ],
        )?;
        if updated != 1 {
            return Err(StoreError::LeaseLost(run_id.into()));
        }
        Ok(())
    }

    pub fn list_workflow_runs(
        &self,
        workspace: &Path,
        limit: u32,
    ) -> Result<Vec<WorkflowRunSummary>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, workflow_path, status, lease_expires_at_ms, created_at_ms, updated_at_ms
             FROM workflow_runs WHERE workspace_path = ?1
             ORDER BY updated_at_ms DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![workspace.to_string_lossy(), limit], |row| {
            Ok(WorkflowRunSummary {
                id: row.get(0)?,
                workflow_path: row.get(1)?,
                status: row.get(2)?,
                lease_expires_at_ms: row.get(3)?,
                created_at_ms: row.get(4)?,
                updated_at_ms: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Database)
    }

    pub fn claim_workflow_run(
        &self,
        id: &str,
        owner: &str,
        now: i64,
        lease_until: i64,
    ) -> Result<bool, StoreError> {
        let updated = self.connection()?.execute(
            "UPDATE workflow_runs
             SET lease_owner = ?1, lease_expires_at_ms = ?2, status = 'running', updated_at_ms = ?3
             WHERE id = ?4 AND (lease_owner IS NULL OR lease_expires_at_ms < ?3 OR lease_owner = ?1)",
            params![owner, lease_until, now, id],
        )?;
        Ok(updated == 1)
    }

    pub fn renew_workflow_lease(
        &self,
        id: &str,
        owner: &str,
        lease_until: i64,
    ) -> Result<(), StoreError> {
        let updated = self.connection()?.execute(
            "UPDATE workflow_runs SET lease_expires_at_ms = ?1, updated_at_ms = ?2
             WHERE id = ?3 AND lease_owner = ?4",
            params![lease_until, now_ms()?, id, owner],
        )?;
        if updated != 1 {
            return Err(StoreError::LeaseLost(id.into()));
        }
        Ok(())
    }

    pub fn begin_workflow_step(
        &self,
        run_id: &str,
        step_id: &str,
        owner: &str,
    ) -> Result<u32, StoreError> {
        let connection = self.connection()?;
        let updated = connection.execute(
            "UPDATE workflow_step_reports
             SET status = 'running', attempts = attempts + 1, report_json = NULL, updated_at_ms = ?1
             WHERE run_id = ?2 AND step_id = ?3
               AND EXISTS (SELECT 1 FROM workflow_runs
                           WHERE id = ?2 AND lease_owner = ?4)",
            params![now_ms()?, run_id, step_id, owner],
        )?;
        if updated != 1 {
            return Err(StoreError::LeaseLost(run_id.into()));
        }
        connection
            .query_row(
                "SELECT attempts FROM workflow_step_reports WHERE run_id = ?1 AND step_id = ?2",
                params![run_id, step_id],
                |row| row.get(0),
            )
            .map_err(StoreError::Database)
    }

    pub fn complete_workflow_step(
        &self,
        run_id: &str,
        step_id: &str,
        status: &str,
        report_json: &str,
        owner: &str,
    ) -> Result<(), StoreError> {
        let updated = self.connection()?.execute(
            "UPDATE workflow_step_reports
             SET status = ?1, report_json = ?2, updated_at_ms = ?3
             WHERE run_id = ?4 AND step_id = ?5
               AND EXISTS (SELECT 1 FROM workflow_runs
                           WHERE id = ?4 AND lease_owner = ?6)",
            params![status, report_json, now_ms()?, run_id, step_id, owner],
        )?;
        if updated != 1 {
            return Err(StoreError::LeaseLost(run_id.into()));
        }
        Ok(())
    }

    pub fn finish_workflow_run(
        &self,
        id: &str,
        status: &str,
        owner: &str,
    ) -> Result<(), StoreError> {
        let now = now_ms()?;
        let updated = self.connection()?.execute(
            "UPDATE workflow_runs
             SET status = ?1, lease_owner = NULL, lease_expires_at_ms = NULL,
                 updated_at_ms = ?2, completed_at_ms = ?2
             WHERE id = ?3 AND lease_owner = ?4",
            params![status, now, id, owner],
        )?;
        if updated != 1 {
            return Err(StoreError::LeaseLost(id.into()));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_supervisor_run(
        &self,
        root_workspace: &Path,
        task_file: &Path,
        model: &str,
        concurrency: usize,
        fingerprint: &str,
        parent_run_id: Option<&str>,
        owner: &str,
        lease_until: i64,
        tasks: &[NewSupervisorTask],
    ) -> Result<(String, Vec<String>), StoreError> {
        let run_id = Uuid::now_v7().to_string();
        let now = now_ms()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO supervisor_runs
             (id, root_workspace_path, task_file_path, model_selector, concurrency,
              status, fingerprint, parent_run_id, lease_owner, lease_expires_at_ms,
              created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7, ?8, ?9, ?10, ?10)",
            params![
                run_id,
                root_workspace.to_string_lossy(),
                task_file.to_string_lossy(),
                model,
                concurrency,
                fingerprint,
                parent_run_id,
                owner,
                lease_until,
                now,
            ],
        )?;
        let mut sessions = Vec::with_capacity(tasks.len());
        for (ordinal, task) in tasks.iter().enumerate() {
            let session_id = Uuid::now_v7().to_string();
            transaction.execute(
                "INSERT INTO sessions (id, title, provider, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![session_id, task.title, model, now],
            )?;
            transaction.execute(
                "INSERT INTO supervisor_tasks
                 (run_id, task_id, ordinal, worktree_name, workspace_path, verify,
                  prompt_sha256, idempotent, session_id, status, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10)",
                params![
                    run_id,
                    task.id,
                    ordinal,
                    task.worktree,
                    task.workspace_path,
                    task.verify,
                    task.prompt_sha256,
                    task.idempotent,
                    session_id,
                    now,
                ],
            )?;
            sessions.push(session_id);
        }
        transaction.commit()?;
        Ok((run_id, sessions))
    }

    pub fn set_supervisor_task_status(
        &self,
        run_id: &str,
        task_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        let now = now_ms()?;
        self.connection()?.execute(
            "UPDATE supervisor_tasks SET status = ?1, error_message = ?2, updated_at_ms = ?3
             WHERE run_id = ?4 AND task_id = ?5",
            params![
                status,
                error.map(|value| value.chars().take(16_384).collect::<String>()),
                now,
                run_id,
                task_id
            ],
        )?;
        self.connection()?.execute(
            "UPDATE supervisor_runs SET updated_at_ms = ?1 WHERE id = ?2",
            params![now, run_id],
        )?;
        Ok(())
    }

    pub fn finish_supervisor_run(&self, id: &str, status: &str) -> Result<(), StoreError> {
        let now = now_ms()?;
        self.connection()?.execute(
            "UPDATE supervisor_runs
             SET status = ?1, lease_owner = NULL, lease_expires_at_ms = NULL,
                 updated_at_ms = ?2, completed_at_ms = ?2 WHERE id = ?3",
            params![status, now, id],
        )?;
        Ok(())
    }

    pub fn renew_supervisor_lease(
        &self,
        id: &str,
        owner: &str,
        lease_until: i64,
    ) -> Result<(), StoreError> {
        let updated = self.connection()?.execute(
            "UPDATE supervisor_runs SET lease_expires_at_ms = ?1, updated_at_ms = ?2
             WHERE id = ?3 AND lease_owner = ?4 AND status = 'running'",
            params![lease_until, now_ms()?, id, owner],
        )?;
        if updated != 1 {
            return Err(StoreError::LeaseLost(id.into()));
        }
        Ok(())
    }

    pub fn recover_expired_supervisors(&self, now: i64) -> Result<u64, StoreError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut statement = transaction.prepare(
            "SELECT id FROM supervisor_runs
             WHERE status = 'running' AND lease_expires_at_ms IS NOT NULL
               AND lease_expires_at_ms < ?1",
        )?;
        let ids = statement
            .query_map([now], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for id in &ids {
            transaction.execute(
                "UPDATE supervisor_tasks SET status = 'interrupted',
                 error_message = 'supervisor lease expired', updated_at_ms = ?1
                 WHERE run_id = ?2 AND status IN ('pending', 'running')",
                params![now, id],
            )?;
            transaction.execute(
                "UPDATE supervisor_runs SET status = 'interrupted', lease_owner = NULL,
                 lease_expires_at_ms = NULL, updated_at_ms = ?1, completed_at_ms = ?1
                 WHERE id = ?2",
                params![now, id],
            )?;
        }
        transaction.commit()?;
        Ok(ids.len() as u64)
    }

    pub fn load_supervisor_run(&self, id: &str) -> Result<Option<SupervisorRunRecord>, StoreError> {
        let connection = self.connection()?;
        let run = connection
            .query_row(
                "SELECT id, task_file_path, root_workspace_path, fingerprint, parent_run_id,
                        model_selector, status, created_at_ms, updated_at_ms
                 FROM supervisor_runs WHERE id = ?1",
                [id],
                |row| {
                    Ok(SupervisorRunRecord {
                        id: row.get(0)?,
                        task_file: row.get(1)?,
                        root_workspace: row.get(2)?,
                        fingerprint: row.get(3)?,
                        parent_run_id: row.get(4)?,
                        model: row.get(5)?,
                        status: row.get(6)?,
                        created_at_ms: row.get(7)?,
                        updated_at_ms: row.get(8)?,
                        tasks: Vec::new(),
                    })
                },
            )
            .optional()?;
        let Some(mut run) = run else {
            return Ok(None);
        };
        let mut statement = connection.prepare(
            "SELECT task_id, worktree_name, workspace_path, verify, idempotent,
                    prompt_sha256, session_id, status, error_message
             FROM supervisor_tasks WHERE run_id = ?1 ORDER BY ordinal",
        )?;
        run.tasks = statement
            .query_map([id], |row| {
                Ok(SupervisorTaskRecord {
                    id: row.get(0)?,
                    worktree: row.get(1)?,
                    workspace_path: row.get(2)?,
                    verify: row.get(3)?,
                    idempotent: row.get(4)?,
                    prompt_sha256: row.get(5)?,
                    session_id: row.get(6)?,
                    status: row.get(7)?,
                    error: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(run))
    }

    pub fn list_supervisor_runs(
        &self,
        workspace: &Path,
        limit: u32,
    ) -> Result<Vec<SupervisorRunRecord>, StoreError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id FROM supervisor_runs WHERE root_workspace_path = ?1
             ORDER BY updated_at_ms DESC LIMIT ?2",
        )?;
        let ids = statement
            .query_map(params![workspace.to_string_lossy(), limit], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        ids.into_iter()
            .map(|id| {
                self.load_supervisor_run(&id)?
                    .ok_or(StoreError::MissingSupervisorRun(id))
            })
            .collect()
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.connection.lock().map_err(|_| StoreError::Poisoned)
    }
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    migration: &str,
) -> Result<(), rusqlite::Error> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for existing in columns {
        if existing? == column {
            return Ok(());
        }
    }
    connection.execute_batch(migration)
}

fn role_name(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn parse_role(role: &str) -> Role {
    match role {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

fn now_ms() -> Result<i64, StoreError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(StoreError::Clock)?;
    i64::try_from(duration.as_millis()).map_err(|_| StoreError::TimeOverflow)
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("storage I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("event serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("system clock is before Unix epoch: {0}")]
    Clock(std::time::SystemTimeError),
    #[error("system timestamp overflow")]
    TimeOverflow,
    #[error("database lock was poisoned")]
    Poisoned,
    #[error("workflow run lease was lost: {0}")]
    LeaseLost(String),
    #[error("supervisor run disappeared: {0}")]
    MissingSupervisorRun(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolCall;
    use serde_json::json;

    #[test]
    fn sqlite_round_trips_tool_call_history() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(&directory.path().join("sessions.db")).unwrap();
        let session = store.create_session("tool history", "fake").unwrap();
        let call = ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "README.md"}),
        };
        store
            .append_message(
                &session,
                &Message::assistant_with_tool_calls("", vec![call.clone()]),
            )
            .unwrap();
        store
            .append_message(&session, &Message::tool("call_1", "result"))
            .unwrap();

        let messages = store.load_messages(&session).unwrap();
        assert_eq!(messages[0].tool_calls, vec![call]);
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn workflow_journal_round_trips_steps_and_releases_lease() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(&directory.path().join("sessions.db")).unwrap();
        let owner = "owner";
        let run_id = store
            .create_workflow_run(
                directory.path(),
                Path::new("workflow.yml"),
                "fingerprint",
                &["first".into(), "second".into()],
                owner,
                i64::MAX,
            )
            .unwrap();
        store
            .set_workflow_step_metadata(
                &run_id,
                "first",
                "read_file",
                &[],
                &["output.txt".into()],
                owner,
            )
            .unwrap();
        assert_eq!(
            store.begin_workflow_step(&run_id, "first", owner).unwrap(),
            1
        );
        store
            .complete_workflow_step(
                &run_id,
                "first",
                "succeeded",
                r#"{"status":"succeeded"}"#,
                owner,
            )
            .unwrap();
        assert!(!store.claim_workflow_run(&run_id, "other", 0, 1000).unwrap());
        store.finish_workflow_run(&run_id, "failed", owner).unwrap();

        let record = store.load_workflow_run(&run_id).unwrap().unwrap();
        assert_eq!(record.status, "failed");
        assert!(record.lease_owner.is_none());
        assert_eq!(record.steps[0].attempts, 1);
        assert_eq!(record.steps[0].status, "succeeded");
        assert_eq!(record.steps[0].tool.as_deref(), Some("read_file"));
        assert_eq!(record.steps[0].artifacts, ["output.txt"]);
        assert_eq!(record.steps[1].status, "pending");
        let listed = store.list_workflow_runs(directory.path(), 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, run_id);
    }
}
