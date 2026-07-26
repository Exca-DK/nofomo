use std::collections::{BTreeMap, HashMap};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tempo_agentic_price::{PricePair, PriceTick};
use tempo_agentic_strategy::{Order, OrderStatus};

use crate::cooling_down;

/// Runtime facts owned by the daemon and shared by the feed, trigger, and dashboard.
pub struct RuntimeStatus {
    started_at: i64,
    allow_broadcast: bool,
    freshness_secs: i64,
    inner: RwLock<RuntimeState>,
}

#[derive(Default)]
struct RuntimeState {
    feeds: HashMap<PricePair, FeedState>,
    quiet_until: BTreeMap<String, i64>,
}

struct FeedState {
    phase: FeedPhase,
    last_event_at: i64,
    last_tick: Option<ObservedTick>,
    last_error: Option<FeedError>,
}

#[derive(Clone, Copy)]
enum FeedPhase {
    Connecting,
    Tick,
    Degraded,
    Ended,
}

/// Immutable runtime view ready for the authenticated dashboard response.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RuntimeSnapshot {
    pub started_at: i64,
    pub generated_at: i64,
    pub allow_broadcast: bool,
    pub feeds: Vec<FeedSnapshot>,
    pub quiet_until: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FeedSnapshot {
    pub pair: PricePair,
    pub health: FeedHealth,
    pub last_event_at: i64,
    pub last_tick: Option<ObservedTick>,
    pub last_error: Option<FeedError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FeedHealth {
    Connecting,
    Live,
    Stale,
    Degraded,
    Ended,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ObservedTick {
    pub price_usd: f64,
    pub published_at: i64,
    pub accepted_at: i64,
}

/// Deliberately safe for operators and JSON; provider bodies and URLs never enter runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FeedError {
    pub category: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeLevelState {
    Quarantined,
    Executing,
    Filled,
    Cooldown,
    Armed,
}

impl RuntimeStatus {
    pub fn new(allow_broadcast: bool, freshness_secs: i64) -> Self {
        Self::new_at(now_secs(), allow_broadcast, freshness_secs)
    }

    pub fn new_at(started_at: i64, allow_broadcast: bool, freshness_secs: i64) -> Self {
        Self {
            started_at,
            allow_broadcast,
            freshness_secs: freshness_secs.max(0),
            inner: RwLock::new(RuntimeState::default()),
        }
    }

    pub fn feed_connecting(&self, pair: PricePair, at: i64) {
        let mut state = self.write();
        let feed = state.feeds.entry(pair).or_insert_with(|| FeedState {
            phase: FeedPhase::Connecting,
            last_event_at: at,
            last_tick: None,
            last_error: None,
        });
        feed.phase = FeedPhase::Connecting;
        feed.last_event_at = at;
        feed.last_error = None;
    }

    pub fn feed_tick(&self, tick: &PriceTick, accepted_at: i64) {
        self.write().feeds.insert(
            tick.pair.clone(),
            FeedState {
                phase: FeedPhase::Tick,
                last_event_at: accepted_at,
                last_tick: Some(ObservedTick {
                    price_usd: tick.price_usd,
                    published_at: tick.published_at,
                    accepted_at,
                }),
                last_error: None,
            },
        );
    }

    pub fn feed_error(&self, pair: &PricePair, at: i64) {
        let mut state = self.write();
        let feed = state
            .feeds
            .entry(pair.clone())
            .or_insert_with(|| FeedState {
                phase: FeedPhase::Connecting,
                last_event_at: at,
                last_tick: None,
                last_error: None,
            });
        feed.phase = FeedPhase::Degraded;
        feed.last_event_at = at;
        feed.last_error = Some(FeedError {
            category: "source_error",
            message: "price source reported an error",
        });
    }

    pub fn feed_ended(&self, pair: &PricePair, at: i64) {
        let mut state = self.write();
        let feed = state
            .feeds
            .entry(pair.clone())
            .or_insert_with(|| FeedState {
                phase: FeedPhase::Connecting,
                last_event_at: at,
                last_tick: None,
                last_error: None,
            });
        feed.phase = FeedPhase::Ended;
        feed.last_event_at = at;
    }

    pub fn remove_feed(&self, pair: &PricePair) {
        self.write().feeds.remove(pair);
    }

    pub fn is_quiet(&self, level_id: &str, now: i64) -> bool {
        self.read()
            .quiet_until
            .get(level_id)
            .is_some_and(|until| now < *until)
    }

    pub fn set_quiet_until(&self, level_id: impl Into<String>, until: i64) {
        self.write().quiet_until.insert(level_id.into(), until);
    }

    pub fn clear_quiet(&self, level_id: &str) {
        self.write().quiet_until.remove(level_id);
    }

    /// Applies the dashboard's one canonical state priority at the supplied time.
    pub fn level_state(&self, level_id: &str, orders: &[Order], now: i64) -> RuntimeLevelState {
        self.snapshot_at(now).level_state(level_id, orders)
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.snapshot_at(now_secs())
    }

    pub fn snapshot_at(&self, generated_at: i64) -> RuntimeSnapshot {
        let state = self.read();
        let mut feeds = state
            .feeds
            .iter()
            .map(|(pair, feed)| FeedSnapshot {
                pair: pair.clone(),
                health: feed.health(generated_at, self.freshness_secs),
                last_event_at: feed.last_event_at,
                last_tick: feed.last_tick.clone(),
                last_error: feed.last_error.clone(),
            })
            .collect::<Vec<_>>();
        feeds.sort_by(|left, right| {
            (left.pair.chain_id, &left.pair.token_address)
                .cmp(&(right.pair.chain_id, &right.pair.token_address))
        });

        RuntimeSnapshot {
            started_at: self.started_at,
            generated_at,
            allow_broadcast: self.allow_broadcast,
            feeds,
            quiet_until: state
                .quiet_until
                .iter()
                .filter(|(_, until)| generated_at < **until)
                .map(|(id, until)| (id.clone(), *until))
                .collect(),
        }
    }

    fn read(&self) -> RwLockReadGuard<'_, RuntimeState> {
        self.inner.read().unwrap_or_else(|error| error.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, RuntimeState> {
        self.inner
            .write()
            .unwrap_or_else(|error| error.into_inner())
    }
}

impl RuntimeSnapshot {
    /// Applies the canonical state priority from this single immutable runtime copy.
    pub fn level_state(&self, level_id: &str, orders: &[Order]) -> RuntimeLevelState {
        let statuses = orders
            .iter()
            .filter(|order| order.level_id == level_id)
            .map(Order::status)
            .collect::<Vec<_>>();

        if statuses.contains(&OrderStatus::Quarantined) {
            RuntimeLevelState::Quarantined
        } else if statuses
            .iter()
            .any(|status| matches!(status, OrderStatus::Pending | OrderStatus::Submitted))
        {
            RuntimeLevelState::Executing
        } else if statuses.contains(&OrderStatus::Filled) {
            RuntimeLevelState::Filled
        } else if self.quiet_until.contains_key(level_id)
            || cooling_down(level_id, orders, self.generated_at)
        {
            RuntimeLevelState::Cooldown
        } else {
            RuntimeLevelState::Armed
        }
    }
}

impl FeedState {
    fn health(&self, now: i64, freshness_secs: i64) -> FeedHealth {
        match self.phase {
            FeedPhase::Connecting => FeedHealth::Connecting,
            FeedPhase::Degraded => FeedHealth::Degraded,
            FeedPhase::Ended => FeedHealth::Ended,
            FeedPhase::Tick => {
                let fresh = self
                    .last_tick
                    .as_ref()
                    .is_some_and(|tick| now.saturating_sub(tick.accepted_at) < freshness_secs);
                if fresh {
                    FeedHealth::Live
                } else {
                    FeedHealth::Stale
                }
            }
        }
    }
}

pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;
    use serde_json::json;
    use tempo_agentic_domain::{ExecStep, ExecutionPlan, VenueName};
    use tempo_agentic_strategy::{OrderState, Side, Strategy, StrategyLevel};

    use super::*;

    fn pair() -> PricePair {
        PricePair::new(8453, "0xfeed")
    }

    fn tick(at: i64) -> PriceTick {
        PriceTick {
            pair: pair(),
            price_usd: 3_000.0,
            published_at: at,
        }
    }

    #[test]
    fn feed_lifecycle_is_time_based_and_recovers() {
        let runtime = RuntimeStatus::new_at(10, false, 5);
        runtime.feed_connecting(pair(), 10);
        assert_eq!(
            runtime.snapshot_at(10).feeds[0].health,
            FeedHealth::Connecting
        );

        runtime.feed_tick(&tick(11), 11);
        assert_eq!(runtime.snapshot_at(15).feeds[0].health, FeedHealth::Live);
        assert_eq!(runtime.snapshot_at(16).feeds[0].health, FeedHealth::Stale);

        runtime.feed_error(&pair(), 17);
        let degraded = runtime.snapshot_at(17).feeds.remove(0);
        assert_eq!(degraded.health, FeedHealth::Degraded);
        assert_eq!(degraded.last_error.unwrap().category, "source_error");

        runtime.feed_tick(&tick(18), 18);
        assert_eq!(runtime.snapshot_at(18).feeds[0].health, FeedHealth::Live);
        runtime.feed_ended(&pair(), 19);
        assert_eq!(runtime.snapshot_at(19).feeds[0].health, FeedHealth::Ended);
    }

    #[test]
    fn runtime_json_never_contains_provider_error_details() {
        let runtime = RuntimeStatus::new_at(10, true, 5);
        runtime.feed_error(&pair(), 11);
        let json = serde_json::to_string(&runtime.snapshot_at(11)).unwrap();
        assert!(!json.contains("http"));
        assert!(!json.contains("body"));
        assert!(json.contains("source_error"));
    }

    #[test]
    fn quiet_until_and_level_priority_use_the_same_clock_boundary() {
        let runtime = RuntimeStatus::new_at(10, false, 5);
        runtime.set_quiet_until("l-1", 20);
        assert!(runtime.is_quiet("l-1", 19));
        assert!(!runtime.is_quiet("l-1", 20));
        assert!(runtime.snapshot_at(19).quiet_until.contains_key("l-1"));
        assert!(!runtime.snapshot_at(20).quiet_until.contains_key("l-1"));

        let failed = order(
            OrderState::Failed {
                tx_hash: None,
                reason: "failed".into(),
            },
            0,
        );
        let filled = order(
            OrderState::Filled {
                tx_hash: "0x1".into(),
            },
            0,
        );
        let executing = order(
            OrderState::Submitted {
                step: ExecStep::Swap,
                amount_in: U256::ONE,
                tx_hash: "0x2".into(),
                withdraw_action_id: None,
                submitted_at: 0,
            },
            0,
        );
        let quarantined = order(
            OrderState::SwapQuarantined {
                amount_in: U256::ONE,
                tx_hash: None,
                reason: "operator needed".into(),
            },
            0,
        );

        assert_eq!(
            runtime.level_state("l-1", std::slice::from_ref(&failed), 19),
            RuntimeLevelState::Cooldown
        );
        assert_eq!(
            runtime.level_state("l-1", &[failed], 60),
            RuntimeLevelState::Armed
        );
        assert_eq!(
            runtime.level_state("l-1", std::slice::from_ref(&filled), 20),
            RuntimeLevelState::Filled
        );
        assert_eq!(
            runtime.level_state("l-1", &[filled, executing.clone()], 20),
            RuntimeLevelState::Executing
        );
        assert_eq!(
            runtime.level_state("l-1", &[executing, quarantined], 20),
            RuntimeLevelState::Quarantined
        );
    }

    fn order(state: OrderState, created_at: i64) -> Order {
        let entry = StrategyLevel {
            strategy: Strategy {
                id: "s-1".into(),
                venue: VenueName::Uniswap,
                chain: "base".into(),
                base_token: "WETH".into(),
                quote_token: "USDC".into(),
            },
            level: tempo_agentic_strategy::Level {
                id: "l-1".into(),
                strategy_id: "s-1".into(),
                side: Side::Buy,
                trigger_price_usd: 3_000.0,
                amount: U256::ONE,
                amount_decimals: 6,
                slippage_bps: 50,
            },
        };
        let mut order = Order::new(
            "o-1".into(),
            &entry,
            ExecutionPlan::Uniswap {
                chain_name: "base".into(),
                chain_id: 8453,
                input_token: "USDC".into(),
                input_amount: "1".into(),
                quote: json!({}),
            },
            created_at,
        );
        order.state = state;
        order
    }
}
