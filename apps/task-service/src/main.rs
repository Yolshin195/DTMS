mod config;
mod domain;
mod handlers;
mod infrastructure;
mod repository;
mod service;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::{
    Router,
    routing::{get, post},
};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceBuilder;
use tower_http::{
    cors::{Any, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing::info;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use config::AppConfig;
use handlers::{
    AppState, create_task, delete_task, get_task, get_task_tree, health, list_tasks, update_task,
};
use handlers::openapi::ApiDoc;
use infrastructure::{
    nats::{NatsEventPublisher, NatsSubscriber, NoopEventPublisher},
    telemetry::{init_telemetry, shutdown_telemetry},
};
use repository::PgTaskRepository;
use service::TaskService;

#[tokio::main]
async fn main() -> Result<()> {
    // ── Config ───────────────────────────────────────────────────────────────
    let cfg = AppConfig::from_env()?;

    // ── Telemetry (must come before any tracing macros) ───────────────────────
    init_telemetry(&cfg)?;
    info!(service = %cfg.service_name, version = env!("CARGO_PKG_VERSION"), "starting");

    // ── Database pool ─────────────────────────────────────────────────────────
    let pool = PgPoolOptions::new()
        .max_connections(cfg.database_pool_size)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&cfg.database_url)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;
    info!("database migrations applied");

    let pool = Arc::new(pool);

    // ── Repository ────────────────────────────────────────────────────────────
    let repo = Arc::new(PgTaskRepository::new(Arc::clone(&pool)));

    // ── NATS (optional — gracefully degrade if unreachable) ───────────────────
    let (publisher, subscriber): (
        Arc<dyn infrastructure::nats::EventPublisher>,
        Option<Arc<NatsSubscriber>>,
    ) = match async_nats::connect(&cfg.nats_url).await {
        Ok(client) => {
            info!(url = %cfg.nats_url, "connected to NATS");
            let pub_client = client.clone();
            let publisher = Arc::new(NatsEventPublisher::new(pub_client));
            // Subscriber created below after TaskService is ready
            (publisher, Some(Arc::new(NatsSubscriber::new(client, Arc::new(build_task_service(Arc::clone(&repo) as _, Arc::new(infrastructure::nats::NoopEventPublisher)))))))
        }
        Err(e) => {
            tracing::warn!(error = %e, "NATS unavailable — events disabled");
            (Arc::new(NoopEventPublisher), None)
        }
    };

    // ── Service ───────────────────────────────────────────────────────────────
    let task_service = Arc::new(build_task_service(
        repo as Arc<dyn repository::TaskRepository>,
        Arc::clone(&publisher),
    ));

    // ── Start NATS subscriber with fully constructed service ──────────────────
    if let Some(_sub) = subscriber {
        // Rebuild subscriber with correct task_service pointer
        let real_sub = Arc::new(NatsSubscriber::new(
            // We need a second NATS connection for the subscriber
            async_nats::connect(&cfg.nats_url).await?,
            Arc::clone(&task_service),
        ));
        real_sub.run().await;
    }

    // ── Router ────────────────────────────────────────────────────────────────
    let state = AppState {
        task_service: Arc::clone(&task_service),
    };

    let api_routes = Router::new()
        .route("/tasks",          post(create_task).get(list_tasks))
        .route("/tasks/:id",      get(get_task).patch(update_task).delete(delete_task))
        .route("/tasks/:id/tree", get(get_task_tree))
        .route("/health",         get(health))
        .with_state(state);

    let swagger = SwaggerUi::new("/swagger-ui")
        .url("/api-docs/openapi.json", ApiDoc::openapi());

    let middleware = ServiceBuilder::new()
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<_>| {
                    let request_id = request
                        .headers()
                        .get("x-request-id")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-");
                    tracing::info_span!(
                        "http_request",
                        method  = %request.method(),
                        uri     = %request.uri(),
                        request_id = request_id,
                    )
                }),
        )
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        );

    let app = Router::new()
        .merge(api_routes)
        .merge(swagger)
        .layer(middleware);

    // ── Serve ─────────────────────────────────────────────────────────────────
    let listener = tokio::net::TcpListener::bind(&cfg.host).await?;
    info!(addr = %cfg.host, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    shutdown_telemetry();
    info!("server shut down gracefully");
    Ok(())
}

fn build_task_service(
    repo: Arc<dyn repository::TaskRepository>,
    events: Arc<dyn infrastructure::nats::EventPublisher>,
) -> TaskService {
    TaskService::new(repo, events)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install CTRL+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c    => {},
        _ = terminate => {},
    }
    info!("shutdown signal received");
}
