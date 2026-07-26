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
