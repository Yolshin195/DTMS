use async_nats::Client;
use async_trait::async_trait;
use futures::StreamExt;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;
use tracing::{error, info, instrument, warn};

use crate::domain::{DomainError, TimerStoppedEvent, UserDeletedEvent};
use crate::service::TaskService;

// ─── Publisher port ───────────────────────────────────────────────────────────

#[async_trait]
pub trait EventPublisher: Send + Sync {
    async fn publish_raw(
        &self,
        subject: &str,
        payload: serde_json::Value,
    ) -> Result<(), DomainError>;
}

// + новая свободная функция
pub async fn publish_event<T: Serialize>(
    publisher: &dyn EventPublisher,
    subject: &str,
    event: &T,
) -> Result<(), DomainError> {
    let value = serde_json::to_value(event)
        .map_err(|e| DomainError::Messaging(e.to_string()))?;
    publisher.publish_raw(subject, value).await
}

// ─── NATS publisher adapter ───────────────────────────────────────────────────

pub struct NatsEventPublisher {
    client: Client,
}

impl NatsEventPublisher {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl EventPublisher for NatsEventPublisher {
    #[instrument(skip(self, payload), fields(subject = subject))]
    async fn publish_raw(
        &self,
        subject: &str,
        payload: serde_json::Value,
    ) -> Result<(), DomainError> {
        let bytes = serde_json::to_vec(&payload)
            .map_err(|e| DomainError::Messaging(e.to_string()))?;

        self.client
            .publish(subject.to_string(), bytes.into())
            .await
            .map_err(|e| DomainError::Messaging(e.to_string()))?;

        info!(subject, "event published");
        Ok(())
    }
}

// ─── No-op publisher (for testing / local dev without NATS) ──────────────────

pub struct NoopEventPublisher;

#[async_trait]
impl EventPublisher for NoopEventPublisher {
    async fn publish_raw(
        &self,
        subject: &str,
        _payload: serde_json::Value,
    ) -> Result<(), DomainError> {
        warn!(subject, "NATS not configured — event dropped");
        Ok(())
    }
}

// ─── NATS subscriber — listens for user.deleted and timer.stopped ─────────────

pub struct NatsSubscriber {
    client: Client,
    task_service: Arc<TaskService>,
}

impl NatsSubscriber {
    pub fn new(client: Client, task_service: Arc<TaskService>) -> Self {
        Self {
            client,
            task_service,
        }
    }

    /// Spawn background tokio tasks for each subscription.
    pub async fn run(self: Arc<Self>) {
        let this = Arc::clone(&self);
        tokio::spawn(async move {
            this.subscribe_user_deleted().await;
        });

        let this = Arc::clone(&self);
        tokio::spawn(async move {
            this.subscribe_timer_stopped().await;
        });
    }

    async fn subscribe_user_deleted(&self) {
        match self.client.subscribe("user.deleted").await {
            Ok(mut sub) => {
                info!("subscribed to user.deleted");
                while let Some(msg) = sub.next().await {
                    match parse_payload::<UserDeletedEvent>(&msg.payload) {
                        Ok(event) => {
                            if let Err(e) = self.task_service.handle_user_deleted(event).await {
                                error!(error = %e, "error handling user.deleted");
                            }
                        }
                        Err(e) => warn!(error = %e, "failed to parse user.deleted payload"),
                    }
                }
            }
            Err(e) => error!(error = %e, "failed to subscribe to user.deleted"),
        }
    }

    async fn subscribe_timer_stopped(&self) {
        match self.client.subscribe("timer.stopped").await {
            Ok(mut sub) => {
                info!("subscribed to timer.stopped");
                while let Some(msg) = sub.next().await {
                    match parse_payload::<TimerStoppedEvent>(&msg.payload) {
                        Ok(event) => {
                            if let Err(e) = self.task_service.handle_timer_stopped(event).await {
                                error!(error = %e, "error handling timer.stopped");
                            }
                        }
                        Err(e) => warn!(error = %e, "failed to parse timer.stopped payload"),
                    }
                }
            }
            Err(e) => error!(error = %e, "failed to subscribe to timer.stopped"),
        }
    }
}

fn parse_payload<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(bytes)
}
