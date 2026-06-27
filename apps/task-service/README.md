# Task Service

Microservice for hierarchical task management — part of the **DTMS** ecosystem.

## Stack

| Layer | Technology |
|---|---|
| Runtime | Rust 1.78 + Tokio |
| HTTP | Axum 0.7 |
| Database | PostgreSQL 16 + SQLx 0.7 |
| Messaging | NATS 2.10 |
| OpenAPI | utoipa 4 + Swagger UI |
| Tracing | OpenTelemetry (OTLP → Jaeger) |
| Logging | tracing-subscriber JSON |

## Architecture

```
src/
├── config/        — AppConfig from env vars (dotenvy + config crate)
├── domain/        — Task aggregate, TaskStatus, DomainError, event payloads
├── repository/    — TaskRepository trait + PgTaskRepository (SQLx)
├── service/       — TaskService: business logic, tree building, event handlers
├── handlers/      — Axum route handlers + OpenAPI annotations
│   └── openapi.rs — ApiDoc struct (utoipa)
└── infrastructure/
    ├── nats/      — EventPublisher trait, NatsEventPublisher, NatsSubscriber
    └── telemetry/ — OTel tracer init + JSON log subscriber
```

**Layering rule:** each layer only imports the one directly below it.
`handlers → service → repository → domain`
NATS infrastructure is injected via trait objects — no direct coupling.

## API

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `POST` | `/tasks` | Create task |
| `GET` | `/tasks?parent_id=` | List tasks (root or by parent) |
| `GET` | `/tasks/:id` | Get single task |
| `GET` | `/tasks/:id/tree` | Get full subtree |
| `PATCH` | `/tasks/:id` | Partial update |
| `DELETE` | `/tasks/:id` | Soft-delete |
| `GET` | `/swagger-ui/` | Swagger UI |
| `GET` | `/api-docs/openapi.json` | Raw OpenAPI spec |

Authentication: API Gateway validates JWT and forwards `X-User-Id` header.

## NATS Events

### Published
| Subject | Trigger |
|---|---|
| `task.created` | POST /tasks |
| `task.updated` | PATCH /tasks/:id |
| `task.deleted` | DELETE /tasks/:id |

### Consumed
| Subject | Action |
|---|---|
| `user.deleted` | Soft-delete all tasks for that user |
| `timer.stopped` | Add `duration_seconds/60` to `task.spent_mins` |

## Running locally

```bash
# 1. Start dependencies
docker compose up -d postgres nats otel-collector jaeger

# 2. Copy env
cp .env.example .env

# 3. Run service
cargo run

# Swagger UI:   http://localhost:8080/swagger-ui/
# Jaeger UI:    http://localhost:16686
```

## Running full stack

```bash
docker compose up --build
```

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `HOST` | `0.0.0.0:8080` | Bind address |
| `DATABASE_URL` | — | PostgreSQL DSN (required) |
| `DATABASE_POOL_SIZE` | `10` | PgPool max connections |
| `NATS_URL` | `nats://localhost:4222` | NATS address |
| `OTLP_ENDPOINT` | `http://localhost:4317` | OTLP gRPC collector |
| `LOG_LEVEL` | `info` | RUST_LOG filter string |
| `SERVICE_NAME` | `task-service` | OTel service.name attribute |

## Business Rules

- Task ownership is enforced on every mutation — `user_id` from `X-User-Id` must match.
- Parent task must belong to the same user.
- Circular references detected via recursive CTE subtree check.
- Self-reference (`parent_id == id`) rejected with `422`.
- `DELETE` is a soft-delete (`is_deleted = true`); data is retained for audit.
- `user.deleted` cascades soft-delete to all user's tasks.
- `timer.stopped` increments `spent_mins` atomically (`UPDATE … spent_mins + $2`).

## Graceful degradation

If NATS is unreachable at startup the service starts normally with a `NoopEventPublisher` — events are dropped with a warning log. HTTP API remains fully functional.
