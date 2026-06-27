use utoipa::{
    openapi::security::{Http, HttpAuthScheme, SecurityScheme},
    Modify, OpenApi,
};

use crate::domain::{Task, TaskStatus, TaskTree};
use crate::handlers::{
    CreateTaskRequest, ErrorResponse, TaskListResponse, TaskResponse, TaskTreeResponse,
    UpdateTaskRequest,
};

struct BearerAuth;

impl Modify for BearerAuth {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "bearer_auth",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)),
            );
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Task Service API",
        version = "0.1.0",
        description = "Hierarchical task management microservice — part of the DTMS ecosystem.",
        contact(name = "DTMS Team"),
    ),
    paths(
        crate::handlers::health,
        crate::handlers::create_task,
        crate::handlers::list_tasks,
        crate::handlers::get_task,
        crate::handlers::get_task_tree,
        crate::handlers::update_task,
        crate::handlers::delete_task,
    ),
    components(
        schemas(
            Task, TaskStatus, TaskTree,
            CreateTaskRequest, UpdateTaskRequest,
            TaskResponse, TaskListResponse, TaskTreeResponse,
            ErrorResponse,
        )
    ),
    modifiers(&BearerAuth),
    tags(
        (name = "Tasks", description = "Task CRUD and tree operations"),
        (name = "System", description = "Health and diagnostics"),
    )
)]
pub struct ApiDoc;
