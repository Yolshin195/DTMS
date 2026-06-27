pub mod openapi;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;
use validator::Validate;

use crate::domain::{DomainError, Task, TaskStatus, TaskTree};
use crate::service::{CreateTaskInput, TaskService, UpdateTaskInput};

// ─── Shared application state ─────────────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub task_service: Arc<TaskService>,
}

// ─── Request / Response schemas ───────────────────────────────────────────────

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct CreateTaskRequest {
    /// Short task title (required, max 500 chars)
    #[validate(length(min = 1, max = 500))]
    pub title: String,
    /// Optional detailed description
    #[serde(default)]
    pub description: String,
    /// Optional parent task UUID
    pub parent_id: Option<Uuid>,
    /// Estimated time in minutes (≥ 0)
    #[serde(default)]
    #[validate(range(min = 0))]
    pub estimated_mins: i32,
}

#[derive(Debug, Deserialize, Validate, ToSchema)]
pub struct UpdateTaskRequest {
    /// New title
    #[validate(length(min = 1, max = 500))]
    pub title: Option<String>,
    /// New description
    pub description: Option<String>,
    /// New status
    pub status: Option<TaskStatus>,
    /// New estimated minutes
    #[validate(range(min = 0))]
    pub estimated_mins: Option<i32>,
    /// Set or clear parent (pass `null` to unset)
    #[serde(
        default,
        deserialize_with = "deserialize_optional_parent"
    )]
    pub parent_id: Option<Option<Uuid>>,
}

/// Custom deserializer: `"parent_id": null` → Some(None), field absent → None
fn deserialize_optional_parent<'de, D>(d: D) -> Result<Option<Option<Uuid>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<Option<Uuid>>::deserialize(d)?;
    Ok(v)
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListTasksQuery {
    /// Filter by parent task id (omit for root-level tasks)
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TaskResponse {
    pub task: Task,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TaskListResponse {
    pub tasks: Vec<Task>,
    pub total: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TaskTreeResponse {
    pub tree: TaskTree,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
    pub code: String,
}

// ─── Error → HTTP mapping ─────────────────────────────────────────────────────

impl IntoResponse for DomainError {
    fn into_response(self) -> axum::response::Response {
        let (status, code) = match &self {
            DomainError::NotFound(_)       => (StatusCode::NOT_FOUND, "NOT_FOUND"),
            DomainError::Forbidden(_)      => (StatusCode::FORBIDDEN, "FORBIDDEN"),
            DomainError::Validation(_)     => (StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION_ERROR"),
            DomainError::SelfReference     => (StatusCode::UNPROCESSABLE_ENTITY, "SELF_REFERENCE"),
            DomainError::CircularReference => (StatusCode::UNPROCESSABLE_ENTITY, "CIRCULAR_REF"),
            DomainError::InvalidStatus(_)  => (StatusCode::BAD_REQUEST, "INVALID_STATUS"),
            DomainError::Database(_)       => (StatusCode::INTERNAL_SERVER_ERROR, "DB_ERROR"),
            DomainError::Messaging(_)      => (StatusCode::INTERNAL_SERVER_ERROR, "MESSAGING_ERROR"),
        };

        let body = Json(ErrorResponse {
            error: self.to_string(),
            code: code.to_string(),
        });
        (status, body).into_response()
    }
}

// ─── Helper: extract user_id from JWT claim header ────────────────────────────
// API Gateway is responsible for JWT verification and forwards X-User-Id header.

fn extract_user_id(
    headers: &axum::http::HeaderMap,
) -> Result<Uuid, (StatusCode, Json<ErrorResponse>)> {
    let header = headers
        .get("X-User-Id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "missing X-User-Id header".into(),
                    code: "UNAUTHORIZED".into(),
                }),
            )
        })?;

    Uuid::parse_str(header).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "invalid X-User-Id format".into(),
                code: "BAD_REQUEST".into(),
            }),
        )
    })
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// Create a new task
#[utoipa::path(
    post,
    path = "/tasks",
    tag = "Tasks",
    request_body = CreateTaskRequest,
    responses(
        (status = 201, description = "Task created", body = TaskResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 422, description = "Validation error", body = ErrorResponse),
        (status = 500, description = "Internal error", body = ErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_task(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<CreateTaskRequest>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let user_id = extract_user_id(&headers)
        .map_err(|e| e.into_response())?;

    if let Err(e) = body.validate() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: e.to_string(),
                code: "VALIDATION_ERROR".into(),
            }),
        )
            .into_response());
    }

    let input = CreateTaskInput {
        user_id,
        parent_id: body.parent_id,
        title: body.title,
        description: body.description,
        estimated_mins: body.estimated_mins,
    };

    state
        .task_service
        .create_task(input)
        .await
        .map(|task| (StatusCode::CREATED, Json(TaskResponse { task })).into_response())
        .map_err(|e| e.into_response())
}

/// List tasks (root or by parent_id)
#[utoipa::path(
    get,
    path = "/tasks",
    tag = "Tasks",
    params(ListTasksQuery),
    responses(
        (status = 200, description = "Task list", body = TaskListResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_tasks(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<ListTasksQuery>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let user_id = extract_user_id(&headers)
        .map_err(|e| e.into_response())?;

    state
        .task_service
        .list_tasks(user_id, query.parent_id)
        .await
        .map(|tasks| {
            let total = tasks.len();
            Json(TaskListResponse { tasks, total }).into_response()
        })
        .map_err(|e| e.into_response())
}

/// Get full task subtree
#[utoipa::path(
    get,
    path = "/tasks/{id}/tree",
    tag = "Tasks",
    params(("id" = Uuid, Path, description = "Task ID")),
    responses(
        (status = 200, description = "Task tree", body = TaskTreeResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_task_tree(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let user_id = extract_user_id(&headers)
        .map_err(|e| e.into_response())?;

    state
        .task_service
        .get_task_tree(id, user_id)
        .await
        .map(|tree| Json(TaskTreeResponse { tree }).into_response())
        .map_err(|e| e.into_response())
}

/// Get a single task
#[utoipa::path(
    get,
    path = "/tasks/{id}",
    tag = "Tasks",
    params(("id" = Uuid, Path, description = "Task ID")),
    responses(
        (status = 200, description = "Task found", body = TaskResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_task(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let user_id = extract_user_id(&headers)
        .map_err(|e| e.into_response())?;

    state
        .task_service
        .get_task(id, user_id)
        .await
        .map(|task| Json(TaskResponse { task }).into_response())
        .map_err(|e| e.into_response())
}

/// Partially update a task
#[utoipa::path(
    patch,
    path = "/tasks/{id}",
    tag = "Tasks",
    params(("id" = Uuid, Path, description = "Task ID")),
    request_body = UpdateTaskRequest,
    responses(
        (status = 200, description = "Task updated", body = TaskResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 422, description = "Validation error", body = ErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_task(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateTaskRequest>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let user_id = extract_user_id(&headers)
        .map_err(|e| e.into_response())?;

    if let Err(e) = body.validate() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: e.to_string(),
                code: "VALIDATION_ERROR".into(),
            }),
        )
            .into_response());
    }

    let input = UpdateTaskInput {
        title: body.title,
        description: body.description,
        status: body.status,
        estimated_mins: body.estimated_mins,
        parent_id: body.parent_id,
    };

    state
        .task_service
        .update_task(id, user_id, input)
        .await
        .map(|task| Json(TaskResponse { task }).into_response())
        .map_err(|e| e.into_response())
}

/// Soft-delete a task
#[utoipa::path(
    delete,
    path = "/tasks/{id}",
    tag = "Tasks",
    params(("id" = Uuid, Path, description = "Task ID")),
    responses(
        (status = 204, description = "Task deleted"),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_task(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let user_id = extract_user_id(&headers)
        .map_err(|e| e.into_response())?;

    state
        .task_service
        .delete_task(id, user_id)
        .await
        .map(|_| StatusCode::NO_CONTENT.into_response())
        .map_err(|e| e.into_response())
}

/// Health check
#[utoipa::path(
    get,
    path = "/health",
    tag = "System",
    responses((status = 200, description = "Service healthy"))
)]
pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok", "service": "task-service" }))
}
