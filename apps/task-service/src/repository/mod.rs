use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;
use std::sync::Arc;

use crate::domain::{DomainError, Task, TaskStatus};

// ─── Repository trait (port) ──────────────────────────────────────────────────

#[async_trait]
pub trait TaskRepository: Send + Sync {
    async fn create(&self, task: &TaskCreateRow) -> Result<Task, DomainError>;
    async fn find_by_id(&self, id: Uuid) -> Result<Option<Task>, DomainError>;
    async fn find_by_user_and_parent(
        &self,
        user_id: Uuid,
        parent_id: Option<Uuid>,
    ) -> Result<Vec<Task>, DomainError>;
    async fn find_subtree_ids(&self, root_id: Uuid) -> Result<Vec<Uuid>, DomainError>;
    async fn find_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Task>, DomainError>;
    async fn update(&self, id: Uuid, patch: &TaskPatchRow) -> Result<Task, DomainError>;
    async fn soft_delete(&self, id: Uuid) -> Result<(), DomainError>;
    async fn soft_delete_by_user(&self, user_id: Uuid) -> Result<u64, DomainError>;
    async fn add_spent_mins(&self, task_id: Uuid, minutes: i32) -> Result<(), DomainError>;
}

// ─── Data transfer structs ────────────────────────────────────────────────────

pub struct TaskCreateRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub estimated_mins: i32,
}

pub struct TaskPatchRow {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<TaskStatus>,
    pub estimated_mins: Option<i32>,
    pub parent_id: Option<Option<Uuid>>, // Some(None) = unset parent
}

// ─── PostgreSQL adapter ───────────────────────────────────────────────────────

pub struct PgTaskRepository {
    pool: Arc<PgPool>,
}

impl PgTaskRepository {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }
}

/// Internal DB row — maps 1:1 with the tasks table
#[derive(sqlx::FromRow)]
struct TaskRow {
    id: Uuid,
    user_id: Uuid,
    parent_id: Option<Uuid>,
    title: String,
    description: String,
    status: String,
    estimated_mins: i32,
    spent_mins: i32,
    is_deleted: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<TaskRow> for Task {
    type Error = DomainError;

    fn try_from(row: TaskRow) -> Result<Self, Self::Error> {
        use std::str::FromStr;
        Ok(Task {
            id: row.id,
            user_id: row.user_id,
            parent_id: row.parent_id,
            title: row.title,
            description: row.description,
            status: TaskStatus::from_str(&row.status)?,
            estimated_mins: row.estimated_mins,
            spent_mins: row.spent_mins,
            is_deleted: row.is_deleted,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[async_trait]
impl TaskRepository for PgTaskRepository {
    async fn create(&self, row: &TaskCreateRow) -> Result<Task, DomainError> {
        let result = sqlx::query_as!(
            TaskRow,
            r#"
            INSERT INTO tasks (id, user_id, parent_id, title, description, status, estimated_mins)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            RETURNING *
            "#,
            row.id,
            row.user_id,
            row.parent_id,
            row.title,
            row.description,
            row.status.to_string(),
            row.estimated_mins,
        )
        .fetch_one(self.pool.as_ref())
        .await?;

        Task::try_from(result)
    }

    async fn find_by_id(&self, id: Uuid) -> Result<Option<Task>, DomainError> {
        let row = sqlx::query_as!(
            TaskRow,
            "SELECT * FROM tasks WHERE id = $1 AND is_deleted = FALSE",
            id
        )
        .fetch_optional(self.pool.as_ref())
        .await?;

        row.map(Task::try_from).transpose()
    }

    async fn find_by_user_and_parent(
        &self,
        user_id: Uuid,
        parent_id: Option<Uuid>,
    ) -> Result<Vec<Task>, DomainError> {
        let rows: Vec<TaskRow> = match parent_id {
            Some(pid) => sqlx::query_as!(
                TaskRow,
                "SELECT * FROM tasks WHERE user_id = $1 AND parent_id = $2 AND is_deleted = FALSE ORDER BY created_at ASC",
                user_id,
                pid
            )
            .fetch_all(self.pool.as_ref())
            .await?,
            None => sqlx::query_as!(
                TaskRow,
                "SELECT * FROM tasks WHERE user_id = $1 AND parent_id IS NULL AND is_deleted = FALSE ORDER BY created_at ASC",
                user_id
            )
            .fetch_all(self.pool.as_ref())
            .await?,
        };

        rows.into_iter().map(Task::try_from).collect()
    }

    /// Recursive CTE to collect all descendant IDs of a root task
    async fn find_subtree_ids(&self, root_id: Uuid) -> Result<Vec<Uuid>, DomainError> {
        let rows = sqlx::query!(
            r#"
            WITH RECURSIVE subtree AS (
                SELECT id FROM tasks WHERE id = $1 AND is_deleted = FALSE
                UNION ALL
                SELECT t.id FROM tasks t
                INNER JOIN subtree s ON t.parent_id = s.id
                WHERE t.is_deleted = FALSE
            )
            SELECT id FROM subtree
            "#,
            root_id
        )
        .fetch_all(self.pool.as_ref())
        .await?;

        Ok(rows.into_iter().filter_map(|r| r.id).collect())
    }

    async fn find_by_ids(&self, ids: &[Uuid]) -> Result<Vec<Task>, DomainError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let rows = sqlx::query_as!(
            TaskRow,
            "SELECT * FROM tasks WHERE id = ANY($1) AND is_deleted = FALSE",
            ids as &[Uuid],
        )
        .fetch_all(self.pool.as_ref())
        .await?;

        rows.into_iter().map(Task::try_from).collect()
    }

    async fn update(&self, id: Uuid, patch: &TaskPatchRow) -> Result<Task, DomainError> {
        // Build dynamic UPDATE via sqlx — explicit fields for compile-time safety
        let row = sqlx::query_as!(
            TaskRow,
            r#"
            UPDATE tasks SET
                title           = COALESCE($2, title),
                description     = COALESCE($3, description),
                status          = COALESCE($4, status),
                estimated_mins  = COALESCE($5, estimated_mins),
                parent_id       = CASE WHEN $6 THEN $7 ELSE parent_id END
            WHERE id = $1 AND is_deleted = FALSE
            RETURNING *
            "#,
            id,
            patch.title.as_deref(),
            patch.description.as_deref(),
            patch.status.as_ref().map(|s| s.to_string()),
            patch.estimated_mins,
            patch.parent_id.is_some(),      // flag: update parent_id?
            patch.parent_id.flatten(),      // new value (may be NULL)
        )
        .fetch_optional(self.pool.as_ref())
        .await?
        .ok_or(DomainError::NotFound(id))?;

        Task::try_from(row)
    }

    async fn soft_delete(&self, id: Uuid) -> Result<(), DomainError> {
        let result = sqlx::query!(
            "UPDATE tasks SET is_deleted = TRUE WHERE id = $1 AND is_deleted = FALSE",
            id
        )
        .execute(self.pool.as_ref())
        .await?;

        if result.rows_affected() == 0 {
            return Err(DomainError::NotFound(id));
        }
        Ok(())
    }

    async fn soft_delete_by_user(&self, user_id: Uuid) -> Result<u64, DomainError> {
        let result = sqlx::query!(
            "UPDATE tasks SET is_deleted = TRUE WHERE user_id = $1 AND is_deleted = FALSE",
            user_id
        )
        .execute(self.pool.as_ref())
        .await?;

        Ok(result.rows_affected())
    }

    async fn add_spent_mins(&self, task_id: Uuid, minutes: i32) -> Result<(), DomainError> {
        sqlx::query!(
            "UPDATE tasks SET spent_mins = spent_mins + $2 WHERE id = $1 AND is_deleted = FALSE",
            task_id,
            minutes
        )
        .execute(self.pool.as_ref())
        .await?;

        Ok(())
    }
}
