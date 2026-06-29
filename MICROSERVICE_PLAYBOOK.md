# Microservice Playbook — Rust / Axum / DTMS

> Этот файл — единственный источник правды при генерации нового микросервиса.
> Передавай его вместе с описанием сервиса: ТЗ + этот файл → готовый проект.

---

## 0. Философия

| Принцип | Следствие |
|---|---|
| **Library-first** | Весь бизнес-код и Axum router живут в `lib.rs`. `main.rs` — тонкая обёртка на 30 строк. |
| **Compose over configure** | Монолит — отдельный Cargo-проект, который берёт нужные сервисы как зависимости и монтирует их роутеры. Каждый сервис при этом не знает о существовании монолита. |
| **Ports & Adapters (Hexagonal)** | Бизнес-логика не знает о Axum, SQLx, NATS — только о трейтах. |
| **dyn-compatible traits** | Все порты — `dyn Trait` без generics в сигнатурах. Generic-хелперы выносятся в свободные функции. |
| **Observability first** | Tracing span на каждой операции, structured JSON logs, OTLP из коробки. |
| **Graceful degradation** | Недоступный NATS / Redis не роняет HTTP — только warn-лог. |

---

## 1. Структура проекта

```
{service-name}/
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
├── otel-collector.yaml
├── .env.example
├── migrations/
│   └── 0001_init.sql
└── src/
    ├── lib.rs              ← точка входа библиотеки (re-export всего публичного)
    ├── main.rs             ← тонкая обёртка: config + wiring + serve
    ├── config/
    │   └── mod.rs          ← AppConfig (dotenvy + config crate)
    ├── domain/
    │   └── mod.rs          ← агрегаты, value objects, DomainError, event payloads
    ├── repository/
    │   └── mod.rs          ← Repository trait (порт) + Pg/SQLite адаптер
    ├── service/
    │   └── mod.rs          ← бизнес-логика, обработчики событий
    ├── handlers/
    │   ├── mod.rs          ← Axum handlers + request/response DTOs
    │   ├── openapi.rs      ← #[derive(OpenApi)] + ApiDoc struct
    │   └── grpc.rs         ← (опционально) tonic handlers
    └── infrastructure/
        ├── mod.rs
        ├── nats/
        │   └── mod.rs      ← EventPublisher trait + NATS/Noop адаптеры
        └── telemetry/
            └── mod.rs      ← init_telemetry() + shutdown_telemetry()
```

### Правило зависимостей (строго односторонние):

```
handlers / grpc
    └── service
          └── repository (trait)
                └── domain
infrastructure  ←  инжектируется в service через Arc<dyn Trait>
config          ←  читается только в main.rs / lib.rs
```

**Ни один внутренний слой не импортирует внешний.**

---

## 2. Cargo.toml — обязательные зависимости

```toml
[package]
name = "{service-name}"
version = "0.1.0"
edition = "2021"

# ── Library — публичный API для переиспользования в монолите ──────────────────
[lib]
name = "{service_name}"   # snake_case, именно так импортируется: use task_service::...
path = "src/lib.rs"

# ── Бинарник — запуск как самостоятельного микросервиса ───────────────────────
[[bin]]
name = "{service-name}"
path = "src/main.rs"

[dependencies]
# Web
axum            = { version = "0.7", features = ["macros"] }
tokio           = { version = "1",   features = ["full"] }
tower           = "0.4"
tower-http      = { version = "0.5", features = ["cors", "trace", "request-id", "util"] }

# gRPC (добавить если нужно — бизнес-логика не меняется)
# tonic = "0.11"
# prost = "0.12"

# Database
sqlx = { version = "0.7", features = [
    "runtime-tokio-rustls", "postgres", "uuid", "chrono", "migrate"
] }

# Serialization
serde      = { version = "1", features = ["derive"] }
serde_json = "1"

# Identity & time
uuid   = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }

# Config
dotenvy = "0.15"
config  = "0.14"

# OpenAPI
utoipa            = { version = "4", features = ["axum_extras", "chrono", "uuid"] }
utoipa-swagger-ui = { version = "6", features = ["axum"] }

# Messaging
async-nats  = "0.35"
async-trait = "0.1"

# Observability
tracing                          = "0.1"
tracing-subscriber               = { version = "0.3", features = ["env-filter", "json"] }
tracing-opentelemetry            = "0.24"
opentelemetry                    = { version = "0.23", features = ["trace"] }
opentelemetry_sdk                = { version = "0.23", features = ["rt-tokio", "trace"] }
opentelemetry-otlp               = { version = "0.16", features = ["grpc-tonic", "trace"] }
opentelemetry-semantic-conventions = "0.15"

# Errors & validation
thiserror = "1"
anyhow    = "1"
validator = { version = "0.18", features = ["derive"] }

futures = "0.3"
```

---

## 3. Domain layer

### 3.1 Модели

```rust
// src/domain/mod.rs

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct {Entity} {
    pub id: Uuid,
    // ... поля
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

### 3.2 DomainError — единственный тип ошибки

```rust
#[derive(Debug, Error)]
pub enum DomainError {
    #[error("{Entity} not found: {0}")]
    NotFound(Uuid),

    #[error("Access denied")]
    Forbidden(Uuid),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Messaging error: {0}")]
    Messaging(String),
}
```

**Правило:** никаких `anyhow::Error` в domain. `anyhow` используется только в `main.rs`.

### 3.3 Event payloads

```rust
// Исходящие (публикуем в NATS)
#[derive(Serialize, Deserialize)] pub struct {Entity}CreatedEvent { pub id: Uuid, ... }
#[derive(Serialize, Deserialize)] pub struct {Entity}DeletedEvent { pub id: Uuid }

// Входящие (подписываемся)
#[derive(Serialize, Deserialize)] pub struct UserDeletedEvent  { pub user_id: Uuid }
```

---

## 4. Repository layer

### 4.1 Трейт (порт) — dyn-compatible

```rust
#[async_trait]
pub trait {Entity}Repository: Send + Sync {
    async fn create(&self, row: &CreateRow)        -> Result<{Entity}, DomainError>;
    async fn find_by_id(&self, id: Uuid)           -> Result<Option<{Entity}>, DomainError>;
    async fn update(&self, id: Uuid, p: &PatchRow) -> Result<{Entity}, DomainError>;
    async fn soft_delete(&self, id: Uuid)          -> Result<(), DomainError>;
}
```

**Правило:** никаких generics в сигнатурах трейта — нарушает dyn-совместимость.

### 4.2 Адаптер PostgreSQL

- Внутренний `{Entity}Row` с `#[derive(sqlx::FromRow)]` — не экспортировать.
- `TryFrom<{Entity}Row> for {Entity}` — маппинг строки → домен.
- `query_as!` макрос для compile-time проверки SQL.
- Soft delete через `is_deleted = TRUE`, индексы `WHERE is_deleted = FALSE`.

---

## 5. Service layer

```rust
pub struct {Entity}Service {
    repo:   Arc<dyn {Entity}Repository>,
    events: Arc<dyn EventPublisher>,
}

impl {Entity}Service {
    // Каждый публичный метод:
    // 1. #[instrument(skip(self, ...))]
    // 2. валидация входа
    // 3. бизнес-правила
    // 4. repo.*()
    // 5. publish_event(...) с warn при ошибке (не пробрасывать)
}
```

**Правило:** ошибки публикации события — `warn!`, не `return Err`. HTTP-ответ не должен зависеть от доступности NATS.

---

## 6. Infrastructure — EventPublisher

### Ключевое правило: dyn-compatible trait

```rust
// ПРАВИЛЬНО — конкретный тип в сигнатуре
#[async_trait]
pub trait EventPublisher: Send + Sync {
    async fn publish_raw(
        &self,
        subject: &str,
        payload: serde_json::Value,
    ) -> Result<(), DomainError>;
}

// Generic — свободная функция, НЕ метод трейта
pub async fn publish_event<T: Serialize>(
    publisher: &dyn EventPublisher,
    subject: &str,
    event: &T,
) -> Result<(), DomainError> {
    let value = serde_json::to_value(event)
        .map_err(|e| DomainError::Messaging(e.to_string()))?;
    publisher.publish_raw(subject, value).await
}
```

**Реализации:** `NatsEventPublisher` и `NoopEventPublisher` (при недоступном NATS).

---

## 7. Handlers layer

### 7.1 AppState

```rust
#[derive(Clone)]
pub struct AppState {
    pub {entity}_service: Arc<{Entity}Service>,
}
```

### 7.2 Маппинг DomainError → HTTP

```rust
impl IntoResponse for DomainError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            DomainError::NotFound(_)   => (StatusCode::NOT_FOUND,            "NOT_FOUND"),
            DomainError::Forbidden(_)  => (StatusCode::FORBIDDEN,            "FORBIDDEN"),
            DomainError::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "VALIDATION_ERROR"),
            DomainError::Database(_)   => (StatusCode::INTERNAL_SERVER_ERROR, "DB_ERROR"),
            DomainError::Messaging(_)  => (StatusCode::INTERNAL_SERVER_ERROR, "MESSAGING_ERROR"),
        };
        (status, Json(ErrorResponse { error: self.to_string(), code: code.into() })).into_response()
    }
}
```

### 7.3 extract_user_id — обязательный хелпер

```rust
fn extract_user_id(headers: &HeaderMap) -> Result<Uuid, Response> {
    let raw = headers
        .get("X-User-Id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            (StatusCode::UNAUTHORIZED, Json(ErrorResponse {
                error: "missing X-User-Id header".into(),
                code: "UNAUTHORIZED".into(),
            })).into_response()
        })?;

    Uuid::parse_str(raw).map_err(|_| {
        (StatusCode::BAD_REQUEST, Json(ErrorResponse {
            error: "invalid X-User-Id format".into(),
            code: "BAD_REQUEST".into(),
        })).into_response()
    })
}

// В хендлере — обязательно .map_err(|e| e.into_response())?
// Не используй ? напрямую — тип ошибки не совпадёт с Response
let user_id = extract_user_id(&headers).map_err(|e| e)?;
```

### 7.4 OpenAPI — ограничения utoipa

```rust
#[openapi(
    info(
        title = "...",
        version = "0.1.0",   // ← только строковый литерал, НЕ env!()
    ),
    ...
)]
pub struct ApiDoc;
```

### 7.5 Обязательные эндпоинты каждого сервиса

| Path | Method | Описание |
|---|---|---|
| `/health` | GET | `{ "status": "ok", "service": "..." }` |
| `/swagger-ui/` | GET | Swagger UI |
| `/api-docs/openapi.json` | GET | Raw spec |

---

## 8. Observability

### 8.1 Инициализация (src/infrastructure/telemetry/mod.rs)

```rust
pub fn init_telemetry(cfg: &AppConfig) -> Result<()> {
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(&cfg.otlp_endpoint),
        )
        .with_trace_config(
            trace::config()
                .with_sampler(Sampler::AlwaysOn)
                .with_id_generator(RandomIdGenerator::default())
                .with_resource(Resource::new(vec![
                    KeyValue::new(SERVICE_NAME,    cfg.service_name.clone()),
                    KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
                ])),
        )
        .install_batch(runtime::Tokio)?;

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cfg.log_level));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().json().with_target(true).with_file(true).with_line_number(true))
        .with(OpenTelemetryLayer::new(tracer))
        .try_init()?;

    Ok(())
}

pub fn shutdown_telemetry() {
    global::shutdown_tracer_provider();
}
```

### 8.2 Span на каждой операции

```rust
// Service methods
#[instrument(skip(self, input), fields(user_id = %user_id))]
pub async fn create_{entity}(&self, ...) -> Result<...> { ... }

// HTTP layer — автоматически через tower-http TraceLayer
TraceLayer::new_for_http()
    .make_span_with(|req: &Request<_>| {
        let request_id = req.headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        info_span!("http_request",
            method = %req.method(),
            uri    = %req.uri(),
            request_id,
        )
    })
```

### 8.3 Обязательные поля в логах

| Поле | Где |
|---|---|
| `request_id` | каждый HTTP span (x-request-id header) |
| `user_id` | все service spans, где применимо |
| `{entity}_id` | create/update/delete операции |
| `error` | все `warn!` / `error!` |

---

## 9. Config

```rust
// src/config/mod.rs
#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default = "default_host")]
    pub host: String,                  // "0.0.0.0:8080"

    pub database_url: String,          // обязательное

    #[serde(default = "default_pool")]
    pub database_pool_size: u32,       // 10

    #[serde(default = "default_nats")]
    pub nats_url: String,              // "nats://localhost:4222"

    #[serde(default = "default_otlp")]
    pub otlp_endpoint: String,         // "http://localhost:4317"

    #[serde(default = "default_log")]
    pub log_level: String,             // "info"

    #[serde(default = "default_svc")]
    pub service_name: String,          // "{service-name}"
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        config::Config::builder()
            .add_source(config::Environment::default().separator("__"))
            .build()?
            .try_deserialize()
            .context("failed to deserialize config")
    }
}
```

---

## 10. main.rs — порядок инициализации

```rust
#[tokio::main]
async fn main() -> Result<()> {
    // 1. Config
    let cfg = AppConfig::from_env()?;

    // 2. Telemetry (до любых tracing макросов)
    init_telemetry(&cfg)?;
    info!(version = env!("CARGO_PKG_VERSION"), "starting {service-name}");

    // 3. DB pool + migrations
    let pool = PgPoolOptions::new()
        .max_connections(cfg.database_pool_size)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&cfg.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;

    // 4. Repository
    let repo = Arc::new(Pg{Entity}Repository::new(Arc::new(pool)));

    // 5. NATS (graceful degradation)
    let publisher: Arc<dyn EventPublisher> =
        match async_nats::connect(&cfg.nats_url).await {
            Ok(client) => Arc::new(NatsEventPublisher::new(client.clone())),
            Err(e) => {
                warn!(error = %e, "NATS unavailable — using noop publisher");
                Arc::new(NoopEventPublisher)
            }
        };

    // 6. Service
    let service = Arc::new({Entity}Service::new(repo, Arc::clone(&publisher)));

    // 7. NATS subscriber (отдельное соединение)
    if let Ok(sub_client) = async_nats::connect(&cfg.nats_url).await {
        Arc::new(NatsSubscriber::new(sub_client, Arc::clone(&service))).run().await;
    }

    // 8. Router
    let app = build_router(Arc::clone(&service));

    // 9. Serve + graceful shutdown
    let listener = TcpListener::bind(&cfg.host).await?;
    info!(addr = %cfg.host, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    shutdown_telemetry();
    Ok(())
}
```

---

## 11. lib.rs — Library-first

Каждый микросервис экспортирует всё необходимое для внешнего использования. Ключевой экспорт — `build_router`, который принимает уже собранный сервис и возвращает готовый `Router`.

```rust
// src/lib.rs
pub mod config;
pub mod domain;
pub mod handlers;
pub mod infrastructure;
pub mod repository;
pub mod service;

pub use domain::DomainError;
pub use handlers::build_router;   // Arc<{Entity}Service> -> Router
pub use service::{Entity}Service;
```

```rust
// src/handlers/mod.rs
pub fn build_router(service: Arc<{Entity}Service>) -> Router {
    Router::new()
        .route("/{entities}",     post(create).get(list))
        .route("/{entities}/:id", get(get_one).patch(update).delete(delete))
        .route("/health",         get(health))
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .with_state(AppState { service })
}
```

### Как собрать монолит

Монолит — отдельный Cargo-проект (или workspace member). Он не содержит бизнес-логики — только wiring:

```
dtms-monolith/
├── Cargo.toml
├── Dockerfile
└── src/
    └── main.rs
```

```toml
# dtms-monolith/Cargo.toml
[package]
name = "dtms-monolith"
version = "0.1.0"
edition = "2021"

[dependencies]
task_service  = { path = "../task-service" }
user_service  = { path = "../user-service" }
timer_service = { path = "../timer-service" }
auth_service  = { path = "../auth-service" }

axum     = "0.7"
tokio    = { version = "1", features = ["full"] }
anyhow   = "1"
# ... общие infra зависимости
```

```rust
// dtms-monolith/src/main.rs
#[tokio::main]
async fn main() -> Result<()> {
    // Одна инициализация телеметрии на весь процесс
    init_telemetry(&cfg)?;

    // Каждый сервис собирается независимо со своим пулом
    let task_svc  = Arc::new(build_task_service(&cfg).await?);
    let user_svc  = Arc::new(build_user_service(&cfg).await?);
    let timer_svc = Arc::new(build_timer_service(&cfg).await?);

    // Роутеры монтируются под теми же префиксами, что и в микросервисном режиме.
    // Клиент (API Gateway / фронтенд) не замечает разницы.
    let app = Router::new()
        .nest("/tasks",  task_service::build_router(task_svc))
        .nest("/users",  user_service::build_router(user_svc))
        .nest("/timers", timer_service::build_router(timer_svc));

    axum::serve(TcpListener::bind(&cfg.host).await?, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}
```

**Что при этом не меняется в сервисах:** весь код domain / repository / service / handlers идентичен. NATS-подписки каждого сервиса поднимаются так же — просто все в одном процессе. Миграции каждого сервиса накатываются отдельно на свою БД.

---

## 12. gRPC (опционально)

Архитектура поддерживает gRPC без каких-либо изменений в бизнес-логике. gRPC handler — такой же тонкий клей, как Axum handler: принимает запрос, вызывает `{Entity}Service`, возвращает ответ.

```rust
// src/handlers/grpc.rs — добавить когда понадобится
use tonic::{Request, Response, Status};
use crate::service::{Entity}Service;

pub struct {Entity}GrpcServer {
    service: Arc<{Entity}Service>,
}

// impl proto::{Entity}ServiceServer for {Entity}GrpcServer
// Маппинг ошибок: DomainError -> tonic::Status
impl From<DomainError> for Status {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::NotFound(_)   => Status::not_found(e.to_string()),
            DomainError::Forbidden(_)  => Status::permission_denied(e.to_string()),
            DomainError::Validation(_) => Status::invalid_argument(e.to_string()),
            _                          => Status::internal(e.to_string()),
        }
    }
}
```

В `main.rs` gRPC сервер поднимается на отдельном порту рядом с Axum через `tokio::spawn`:

```rust
// Axum на :8080, gRPC на :50051 — оба в одном процессе
tokio::spawn(async move {
    tonic::transport::Server::builder()
        .add_service({Entity}ServiceServer::new(grpc_handler))
        .serve("[::]:50051".parse()?)
        .await
});
```

---

## 13. Dockerfile — multi-stage

```dockerfile
FROM rust:1.78-slim-bookworm AS builder
RUN apt-get update && apt-get install -y pkg-config libssl-dev protobuf-compiler
WORKDIR /app
# Кэш зависимостей
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release && rm -rf src
# Основная сборка
COPY src ./src
COPY migrations ./migrations
RUN touch src/main.rs && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates libssl3 && \
    useradd -ms /bin/bash appuser
USER appuser
WORKDIR /app
COPY --from=builder /app/target/release/{service-name} .
COPY --from=builder /app/migrations ./migrations
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s CMD curl -f http://localhost:8080/health || exit 1
ENTRYPOINT ["./{service-name}"]
```

---

## 14. Чеклист перед генерацией сервиса

Перед тем как начать писать код — задать эти вопросы (если не указано в ТЗ):

- [ ] **БД:** PostgreSQL или SQLite?
- [ ] **Телеметрия:** только structured logs, или + OTLP tracing?
- [ ] **Soft delete или hard delete** при каскадных событиях?
- [ ] **Какие NATS события публикует** этот сервис?
- [ ] **На какие NATS события подписывается?**
- [ ] **gRPC нужен** сейчас или только архитектурно заложить?

---

## 15. Известные подводные камни

| Ошибка | Причина | Решение |
|---|---|---|
| `trait is not dyn compatible` | generic-метод в трейте | убрать `<T>` из трейта, сделать `publish_raw(Value)` + свободная функция `publish_event<T>` |
| `? couldn't convert error to Response` | `extract_user_id` возвращает `Result<_, (StatusCode, Json<...>)>`, не `Response` | использовать `.map_err(\|e\| e.into_response())?` |
| `expected string literal` в `#[openapi]` | `utoipa` не поддерживает `env!()` внутри атрибута | писать версию строкой: `version = "0.1.0"` |
| NATS subscriber роняет сервис | паника в фоновом task | все ошибки внутри subscriber — `error!`, не `unwrap` |
| N+1 при построении дерева | `find_children` на каждый узел | recursive CTE `WITH RECURSIVE` для плоского списка, сборка дерева в памяти |
