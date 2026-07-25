use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tempo_agentic_domain::{
    AuditStore, ExecuteTradeRequest, ExecutionView, MarketResearch, MarketResearchRequest,
    QuoteTradeRequest, QuoteView,
};

/// SQLite storage implementation for process sessions, quotes, and audit trails.
#[derive(Clone)]
pub struct SqliteAuditStore {
    pool: SqlitePool,
    session_id: String,
}

impl SqliteAuditStore {
    /// Opens the database, creates a new process session, and invalidates active quotes from prior runs.
    pub async fn open(path: impl AsRef<Path>, version: &str) -> Result<Self> {
        let pool = connect_pool(path.as_ref()).await?;

        let session_id = format!(
            "s-{:x}-{:x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            std::process::id()
        );
        let now = now_i64();
        let mut transaction = pool.begin().await?;
        sqlx::query(
            "UPDATE quotes SET status = 'invalidated_restart', consumed_at = ? \
             WHERE status = 'quoted'",
        )
        .bind(now)
        .execute(&mut *transaction)
        .await?;
        sqlx::query("UPDATE process_sessions SET ended_at = ? WHERE ended_at IS NULL AND id != ?")
            .bind(now)
            .bind(&session_id)
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO process_sessions(id, version, started_at) VALUES (?, ?, ?)")
            .bind(&session_id)
            .bind(version)
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;

        Ok(Self { pool, session_id })
    }

    /// Opens the database for read-only administration without creating a
    /// process session or invalidating active quotes in a running process.
    pub async fn admin(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            pool: connect_pool(path.as_ref()).await?,
            session_id: "admin".into(),
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn health(&self) -> Result<()> {
        let one: i64 = sqlx::query_scalar("SELECT 1").fetch_one(&self.pool).await?;
        if one != 1 {
            bail!("SQLite health query returned an unexpected value");
        }
        Ok(())
    }

    /// Retrieves recent audit log records including quote statuses and execution attempts.
    pub async fn recent_audit(&self, limit: u32) -> Result<Vec<Value>> {
        let rows = sqlx::query(
            "SELECT q.id, q.venue, q.chain, q.status, q.expires_at, q.created_at, \
                    a.id AS attempt_id, a.status AS attempt_status, a.error_class \
             FROM quotes q LEFT JOIN execution_attempts a ON a.quote_id = q.id \
             ORDER BY q.created_at DESC, a.id DESC LIMIT ?",
        )
        .bind(i64::from(limit.min(1_000)))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                json!({
                    "quote_id": row.get::<String, _>("id"),
                    "venue": row.get::<String, _>("venue"),
                    "chain": row.get::<String, _>("chain"),
                    "quote_status": row.get::<String, _>("status"),
                    "expires_at": row.get::<i64, _>("expires_at"),
                    "created_at": row.get::<i64, _>("created_at"),
                    "attempt_id": row.try_get::<i64, _>("attempt_id").ok(),
                    "attempt_status": row.try_get::<String, _>("attempt_status").ok(),
                    "error_class": row.try_get::<String, _>("error_class").ok(),
                })
            })
            .collect())
    }

    /// Deletes research runs and quotes created before the specified timestamp.
    pub async fn prune(&self, older_than_unix: i64) -> Result<u64> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM research_runs WHERE created_at < ?")
            .bind(older_than_unix)
            .execute(&mut *transaction)
            .await?;
        let deleted = sqlx::query("DELETE FROM quotes WHERE created_at < ?")
            .bind(older_than_unix)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
        transaction.commit().await?;
        Ok(deleted)
    }

    /// Returns the current status of a quote by its identifier.
    pub async fn quote_status(&self, quote_id: &str) -> Result<Option<String>> {
        Ok(sqlx::query_scalar("SELECT status FROM quotes WHERE id = ?")
            .bind(quote_id)
            .fetch_optional(&self.pool)
            .await?)
    }
}

#[async_trait]
impl AuditStore for SqliteAuditStore {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn record_research(
        &self,
        request: &MarketResearchRequest,
        result: &MarketResearch,
    ) -> Result<()> {
        // Persist market facts, but never vendor error bodies returned through the
        // guard reason. Those can contain infrastructure details and are not useful
        // for the durable decision trail.
        let durable_result = serde_json::json!({
            "pair": result.pair,
            "observations": result.observations,
            "guard_passed": result.guard_passed,
            "guard_reason": if result.guard_passed {
                result.guard_reason.as_str()
            } else {
                "graph_guard_failed"
            },
        });
        let inserted = sqlx::query(
            "INSERT INTO research_runs(session_id, request_json, result_json, created_at) \
             SELECT ?, ?, ?, ? WHERE EXISTS ( \
                 SELECT 1 FROM process_sessions WHERE id = ? AND ended_at IS NULL \
             )",
        )
        .bind(&self.session_id)
        .bind(serde_json::to_string(request)?)
        .bind(serde_json::to_string(&durable_result)?)
        .bind(now_i64())
        .bind(&self.session_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if inserted != 1 {
            bail!("process session is no longer active");
        }
        Ok(())
    }

    async fn record_quote(
        &self,
        request: &QuoteTradeRequest,
        quote: &QuoteView,
        plan_digest: &str,
    ) -> Result<()> {
        let inserted = sqlx::query(
            "INSERT INTO quotes( \
                id, session_id, venue, chain, request_json, view_json, plan_digest, \
                expires_at, status, created_at \
             ) SELECT ?, ?, ?, ?, ?, ?, ?, ?, 'quoted', ? WHERE EXISTS ( \
                 SELECT 1 FROM process_sessions WHERE id = ? AND ended_at IS NULL \
             )",
        )
        .bind(&quote.quote_id)
        .bind(&self.session_id)
        .bind(&quote.venue)
        .bind(&quote.chain)
        .bind(serde_json::to_string(request)?)
        .bind(serde_json::to_string(quote)?)
        .bind(plan_digest)
        .bind(i64::try_from(quote.expires_at_unix).context("quote expiry exceeds i64")?)
        .bind(now_i64())
        .bind(&self.session_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        if inserted != 1 {
            bail!("process session is no longer active");
        }
        Ok(())
    }

    async fn claim_quote(&self, request: &ExecuteTradeRequest, now: u64) -> Result<i64> {
        let now = i64::try_from(now).context("current time exceeds i64")?;
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT session_id, status, expires_at FROM quotes WHERE id = ?")
            .bind(&request.quote_id)
            .fetch_optional(&mut *transaction)
            .await?
            .with_context(|| format!("quote {} was not found", request.quote_id))?;
        let session_id: String = row.get("session_id");
        let status: String = row.get("status");
        let expires_at: i64 = row.get("expires_at");
        if session_id != self.session_id || status != "quoted" {
            bail!("quote was invalidated, consumed, or belongs to another process session");
        }
        let next_status = if now >= expires_at {
            "expired"
        } else if !request.confirmed {
            "rejected_unconfirmed"
        } else {
            "claimed"
        };
        let updated = sqlx::query(
            "UPDATE quotes SET status = ?, consumed_at = ? \
             WHERE id = ? AND status = 'quoted' AND session_id = ? AND EXISTS ( \
                 SELECT 1 FROM process_sessions WHERE id = ? AND ended_at IS NULL \
             )",
        )
        .bind(next_status)
        .bind(now)
        .bind(&request.quote_id)
        .bind(&self.session_id)
        .bind(&self.session_id)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        if updated != 1 {
            bail!("quote was claimed concurrently");
        }
        let attempt_id = sqlx::query(
            "INSERT INTO execution_attempts(quote_id, status, created_at, finished_at) \
             VALUES (?, ?, ?, ?) RETURNING id",
        )
        .bind(&request.quote_id)
        .bind(next_status)
        .bind(now)
        .bind(if next_status == "claimed" {
            None
        } else {
            Some(now)
        })
        .fetch_one(&mut *transaction)
        .await?
        .get::<i64, _>("id");
        transaction.commit().await?;
        match next_status {
            "claimed" => Ok(attempt_id),
            "expired" => bail!("quote expired; request a fresh quote"),
            _ => bail!("execution requires confirmed=true after the user reviews the quote"),
        }
    }

    async fn record_execution_success(
        &self,
        attempt_id: i64,
        result: &ExecutionView,
    ) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        for reference in &result.transactions {
            sqlx::query(
                "INSERT INTO transaction_refs(attempt_id, kind, chain, tx_id, status) \
                 VALUES (?, ?, ?, ?, 'confirmed')",
            )
            .bind(attempt_id)
            .bind(&reference.kind)
            .bind(&result.chain)
            .bind(&reference.id)
            .execute(&mut *transaction)
            .await?;
        }
        sqlx::query(
            "UPDATE execution_attempts SET status = 'confirmed', finished_at = ? WHERE id = ?",
        )
        .bind(now_i64())
        .bind(attempt_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE quotes SET status = 'confirmed' WHERE id = \
             (SELECT quote_id FROM execution_attempts WHERE id = ?)",
        )
        .bind(attempt_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn record_execution_failure(&self, attempt_id: i64) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        sqlx::query(
            "UPDATE execution_attempts SET status = 'failed', error_class = 'execution_failed', \
             finished_at = ? WHERE id = ?",
        )
        .bind(now_i64())
        .bind(attempt_id)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE quotes SET status = 'failed' WHERE id = \
             (SELECT quote_id FROM execution_attempts WHERE id = ?)",
        )
        .bind(attempt_id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }
}

fn now_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

async fn connect_pool(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create state directory {}", parent.display()))?;
    }
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .context("cannot open SQLite state")?;
    sqlx::raw_sql(include_str!("../migrations/0001_audit.sql"))
        .execute(&pool)
        .await
        .context("cannot run SQLite migrations")?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempo_agentic_domain::{QuoteTradeRequest, VenueName};

    fn path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "tempo-agentic-{name}-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn request() -> QuoteTradeRequest {
        QuoteTradeRequest {
            venue: VenueName::Uniswap,
            token_in: "USDC".into(),
            token_out: "WETH".into(),
            amount: "1".into(),
            slippage_bps: 50,
            chains: vec!["base".into()],
        }
    }

    fn quote(id: &str) -> QuoteView {
        QuoteView {
            quote_id: id.into(),
            venue: "uniswap".into(),
            chain: "base".into(),
            token_in: "USDC".into(),
            token_out: "WETH".into(),
            amount_in: "1".into(),
            expected_amount_out: "0.001".into(),
            minimum_amount_out: "0.00099".into(),
            expires_at_unix: u64::MAX / 2,
            graph_guard: "passed".into(),
            requires_confirmation: true,
        }
    }

    #[tokio::test]
    async fn claims_once_and_invalidates_quotes_on_restart() {
        let path = path("lifecycle");
        let store = SqliteAuditStore::open(&path, "test").await.unwrap();
        store
            .record_quote(&request(), &quote("q-claimed"), "digest")
            .await
            .unwrap();
        let execute = ExecuteTradeRequest {
            quote_id: "q-claimed".into(),
            confirmed: true,
        };
        assert!(store.claim_quote(&execute, 1).await.is_ok());
        assert!(store.claim_quote(&execute, 1).await.is_err());

        store
            .record_quote(&request(), &quote("q-restart"), "digest")
            .await
            .unwrap();
        let restarted = SqliteAuditStore::open(&path, "test").await.unwrap();
        assert_eq!(
            restarted
                .quote_status("q-restart")
                .await
                .unwrap()
                .as_deref(),
            Some("invalidated_restart")
        );
        assert!(
            store
                .record_quote(&request(), &quote("q-stale-session"), "digest")
                .await
                .is_err()
        );
        drop(store);
        drop(restarted);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn unconfirmed_attempt_is_durable_and_fail_closed() {
        let path = path("unconfirmed");
        let store = SqliteAuditStore::open(&path, "test").await.unwrap();
        store
            .record_quote(&request(), &quote("q-no"), "digest")
            .await
            .unwrap();
        let execute = ExecuteTradeRequest {
            quote_id: "q-no".into(),
            confirmed: false,
        };
        assert!(store.claim_quote(&execute, 1).await.is_err());
        assert_eq!(
            store.quote_status("q-no").await.unwrap().as_deref(),
            Some("rejected_unconfirmed")
        );
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(store.pool())
            .await
            .unwrap();
        assert_eq!(foreign_keys, 1);
        let _ = std::fs::remove_file(path);
    }
}
