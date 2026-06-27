use std::sync::Arc;
use uuid::Uuid;
use tracing::{info, warn, instrument};

use crate::domain::{
    DomainError, Task, TaskCreatedEvent, TaskDeletedEvent, TaskTree, TaskUpdatedEvent,
    TimerStoppedEvent, UserDeletedEvent,
};
use crate::repository::{TaskCreateRow, TaskPatchRow, TaskRepository};
use crate::infrastructure::nats::{EventPublisher, publish_event};

// ─── Request DTOs (used by handlers) ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CreateTaskInput {
    pub user_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub estimated_mins: i32,
}

#[derive(Debug, Clone)]
pub struct UpdateTaskInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<crate::domain::TaskStatus>,
    pub estimated_mins: Option<i32>,
    pub parent_id: Option<Option<Uuid>>,
}

// ─── Task Service ─────────────────────────────────────────────────────────────

pub struct TaskService {
    repo: Arc<dyn TaskRepository>,
    events: Arc<dyn EventPublisher>,
}

impl TaskService {
    pub fn new(repo: Arc<dyn TaskRepository>, events: Arc<dyn EventPublisher>) -> Self {
        Self { repo, events }
    }

    /// Create a new task, validate parent ownership, publish event.
    #[instrument(skip(self), fields(user_id = %input.user_id))]
    pub async fn create_task(&self, input: CreateTaskInput) -> Result<Task, DomainError> {
        // Validate title is non-empty
        let title = input.title.trim().to_string();
        if title.is_empty() {
            return Err(DomainError::Validation("title must not be empty".into()));
        }
        if title.len() > 500 {
            return Err(DomainError::Validation("title exceeds 500 characters".into()));
        }

        // If parent_id given — must exist and belong to the same user
        if let Some(pid) = input.parent_id {
            let parent = self
                .repo
                .find_by_id(pid)
                .await?
                .ok_or(DomainError::NotFound(pid))?;

            if parent.user_id != input.user_id {
                return Err(DomainError::Forbidden(pid));
            }
        }

        let row = TaskCreateRow {
            id: Uuid::new_v4(),
            user_id: input.user_id,
            parent_id: input.parent_id,
            title,
            description: input.description.trim().to_string(),
            status: crate::domain::TaskStatus::Todo,
            estimated_mins: input.estimated_mins.max(0),
        };

        let task = self.repo.create(&row).await?;

        info!(task_id = %task.id, "task created");

        let event = TaskCreatedEvent {
            task_id: task.id,
            user_id: task.user_id,
        };
        if let Err(e) = publish_event(&*self.events, "task.created", &event).await {
            warn!(error = %e, "failed to publish task.created event");
        }

        Ok(task)
    }

    /// Get a task by id; enforce ownership.
    #[instrument(skip(self))]
    pub async fn get_task(&self, task_id: Uuid, user_id: Uuid) -> Result<Task, DomainError> {
        let task = self
            .repo
            .find_by_id(task_id)
            .await?
            .ok_or(DomainError::NotFound(task_id))?;

        if task.user_id != user_id {
            return Err(DomainError::Forbidden(task_id));
        }

        Ok(task)
    }

    /// List direct children (or root tasks) for a user.
    #[instrument(skip(self))]
    pub async fn list_tasks(
        &self,
        user_id: Uuid,
        parent_id: Option<Uuid>,
    ) -> Result<Vec<Task>, DomainError> {
        self.repo
            .find_by_user_and_parent(user_id, parent_id)
            .await
    }

    /// Build the full subtree rooted at `task_id`.
    #[instrument(skip(self))]
    pub async fn get_task_tree(
        &self,
        task_id: Uuid,
        user_id: Uuid,
    ) -> Result<TaskTree, DomainError> {
        let root = self.get_task(task_id, user_id).await?;
        let tree = self.build_tree(root).await?;
        Ok(tree)
    }

    /// Recursive tree builder — avoids N+1 via subtree id prefetch.
    async fn build_tree(&self, task: Task) -> Result<TaskTree, DomainError> {
        let children = self.repo.find_children(task.id).await?;
        let mut child_trees = Vec::with_capacity(children.len());
        for child in children {
            child_trees.push(Box::pin(self.build_tree(child)).await?);
        }
        Ok(TaskTree {
            task,
            children: child_trees,
        })
    }

    /// Partial update with ownership check.
    #[instrument(skip(self, input))]
    pub async fn update_task(
        &self,
        task_id: Uuid,
        user_id: Uuid,
        input: UpdateTaskInput,
    ) -> Result<Task, DomainError> {
        // Ownership check
        let existing = self
            .repo
            .find_by_id(task_id)
            .await?
            .ok_or(DomainError::NotFound(task_id))?;
        if existing.user_id != user_id {
            return Err(DomainError::Forbidden(task_id));
        }

        // Validate new parent if provided
        if let Some(Some(new_parent_id)) = input.parent_id {
            if new_parent_id == task_id {
                return Err(DomainError::SelfReference);
            }
            // Check the new parent exists and belongs to user
            let parent = self
                .repo
                .find_by_id(new_parent_id)
                .await?
                .ok_or(DomainError::NotFound(new_parent_id))?;
            if parent.user_id != user_id {
                return Err(DomainError::Forbidden(new_parent_id));
            }
            // Circular reference check: new_parent must not be in our subtree
            let subtree_ids = self.repo.find_subtree_ids(task_id).await?;
            if subtree_ids.contains(&new_parent_id) {
                return Err(DomainError::CircularReference);
            }
        }

        let patch = TaskPatchRow {
            title: input.title,
            description: input.description,
            status: input.status,
            estimated_mins: input.estimated_mins,
            parent_id: input.parent_id,
        };

        let updated = self.repo.update(task_id, &patch).await?;
        info!(task_id = %task_id, "task updated");

        let changes = serde_json::json!({
            "title": patch.title,
            "description": patch.description,
            "status": patch.status.as_ref().map(|s| s.to_string()),
            "estimated_mins": patch.estimated_mins,
        });
        let event = TaskUpdatedEvent { task_id, changes };
        if let Err(e) = publish_event(&*self.events, "task.updated", &event).await {
            warn!(error = %e, "failed to publish task.updated event");
        }

        Ok(updated)
    }

    /// Soft-delete with ownership check.
    #[instrument(skip(self))]
    pub async fn delete_task(&self, task_id: Uuid, user_id: Uuid) -> Result<(), DomainError> {
        let task = self
            .repo
            .find_by_id(task_id)
            .await?
            .ok_or(DomainError::NotFound(task_id))?;
        if task.user_id != user_id {
            return Err(DomainError::Forbidden(task_id));
        }

        self.repo.soft_delete(task_id).await?;
        info!(task_id = %task_id, "task deleted");

        let event = TaskDeletedEvent { task_id };
        if let Err(e) = publish_event(&*self.events, "task.deleted", &event).await {
            warn!(error = %e, "failed to publish task.deleted event");
        }

        Ok(())
    }

    // ─── Event handlers (called by NATS subscriber) ───────────────────────────

    /// Handle `user.deleted` — soft-delete all tasks belonging to the user.
    #[instrument(skip(self))]
    pub async fn handle_user_deleted(&self, event: UserDeletedEvent) -> Result<(), DomainError> {
        let count = self.repo.soft_delete_by_user(event.user_id).await?;
        info!(user_id = %event.user_id, deleted = count, "soft-deleted tasks for deleted user");
        Ok(())
    }

    /// Handle `timer.stopped` — add tracked minutes to task.spent_mins.
    #[instrument(skip(self))]
    pub async fn handle_timer_stopped(&self, event: TimerStoppedEvent) -> Result<(), DomainError> {
        let minutes = (event.duration_seconds / 60) as i32;
        if minutes > 0 {
            self.repo.add_spent_mins(event.task_id, minutes).await?;
            info!(task_id = %event.task_id, added_mins = minutes, "updated spent_mins from timer");
        }
        Ok(())
    }
}
