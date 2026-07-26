use anyhow::Result;
use async_trait::async_trait;

use crate::{Level, Order, Strategy, StrategyLevel};

/// Durable facts copied from one storage snapshot for observational dashboards.
#[derive(Clone, Debug, PartialEq)]
pub struct DashboardData {
    pub strategies: Vec<Strategy>,
    pub levels: Vec<StrategyLevel>,
    pub orders: Vec<Order>,
}

/// Reads all dashboard-owned durable data from one database snapshot.
#[async_trait]
pub trait DashboardStore: Send + Sync {
    async fn dashboard_data(&self) -> Result<DashboardData>;
}

/// Storage port for strategy markets.
#[async_trait]
pub trait StrategyStore: Send + Sync {
    async fn upsert_strategy(&self, strategy: &Strategy) -> Result<()>;

    async fn get_strategy(&self, id: &str) -> Result<Option<Strategy>>;

    async fn list_strategies(&self) -> Result<Vec<Strategy>>;
}

/// Storage port for the standing rules the daemon evaluates.
#[async_trait]
pub trait LevelStore: Send + Sync {
    /// Upserts a level only while its strategy still matches `expected_strategy`.
    async fn upsert_level(&self, level: &Level, expected_strategy: &Strategy) -> Result<()>;

    async fn get_level(&self, id: &str) -> Result<Option<StrategyLevel>>;

    async fn list_levels(&self) -> Result<Vec<StrategyLevel>>;

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
