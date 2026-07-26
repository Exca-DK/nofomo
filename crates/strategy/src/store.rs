use anyhow::Result;
use async_trait::async_trait;

use crate::{Level, Order};

/// Storage port for the standing rules the daemon evaluates.
#[async_trait]
pub trait LevelStore: Send + Sync {
    /// Idempotently upserts a level, keyed on its ID.
    async fn upsert_level(&self, level: &Level) -> Result<()>;

    async fn get_level(&self, id: &str) -> Result<Option<Level>>;

    async fn list_levels(&self) -> Result<Vec<Level>>;

    /// Deletes a level unless an order references it; missing levels succeed.
    async fn delete_level(&self, id: &str) -> Result<()>;
}

/// Durable storage for execution attempts.
#[async_trait]
pub trait OrderStore: Send + Sync {
    /// Idempotently upserts an order, keyed on its ID.
    async fn upsert_order(&self, order: &Order) -> Result<()>;

    async fn get_order(&self, id: &str) -> Result<Option<Order>>;

    async fn list_orders(&self) -> Result<Vec<Order>>;
}
