use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use utoipa::ToSchema;
use thiserror::Error;

// ─── Status ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "VARCHAR", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Done,
    Cancelled,
}

impl Default for TaskStatus {
    fn default() -> Self {
        Self::Todo
    }
}

impl std::fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Todo       => write!(f, "todo"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Done       => write!(f, "done"),
            Self::Cancelled  => write!(f, "cancelled"),
        }
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "todo"        => Ok(Self::Todo),
            "in_progress" => Ok(Self::InProgress),
            "done"        => Ok(Self::Done),
            "cancelled"   => Ok(Self::Cancelled),
            other         => Err(DomainError::InvalidStatus(other.to_string())),
        }
    }
}

// ─── Task aggregate ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Task {
    /// Unique task identifier
    pub id: Uuid,
    /// Owner user id
    pub user_id: Uuid,
    /// Optional parent task id (for subtasks)
    pub parent_id: Option<Uuid>,
    /// Short title
    pub title: String,
    /// Detailed description
    pub description: String,
    /// Current status
    pub status: TaskStatus,
    /// Estimated time in minutes
    pub estimated_mins: i32,
    /// Accumulated tracked time in minutes
    pub spent_mins: i32,
    /// Soft-delete flag
    pub is_deleted: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Task with optional children (for tree endpoint)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TaskTree {
    #[serde(flatten)]
    pub task: Task,
    #[schema(no_recursion)]
    pub children: Vec<TaskTree>,
}

// ─── Domain errors ────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("Task not found: {0}")]
    NotFound(Uuid),

    #[error("Access denied for task: {0}")]
    Forbidden(Uuid),

    #[error("Invalid status value: '{0}'")]
    InvalidStatus(String),

    #[error("Circular parent reference detected")]
    CircularReference,

    #[error("Cannot set parent to itself")]
    SelfReference,

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Messaging error: {0}")]
    Messaging(String),
}

// ─── NATS event payloads ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCreatedEvent {
    pub task_id: Uuid,
    pub user_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskUpdatedEvent {
    pub task_id: Uuid,
    pub changes: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDeletedEvent {
    pub task_id: Uuid,
}

/// Incoming event from Users Service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDeletedEvent {
    pub user_id: Uuid,
}

/// Incoming event from Timer Service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerStoppedEvent {
    pub timer_id: Uuid,
    pub task_id: Uuid,
    pub duration_seconds: i64,
}
