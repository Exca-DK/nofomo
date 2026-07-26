use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tempo_agentic_storage::{
    CURRENT_SCHEMA_VERSION, LockFile, initialize_new_under_lock, open_existing_current,
};

fn path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "tempo-agentic-schema-{name}-{}-{}.db",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn cleanup(path: &Path) {
    for suffix in ["", "-wal", "-shm", ".lock"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

#[tokio::test]
async fn new_database_is_initialized_atomically_under_its_lock() {
    let database = path("current");
    let lock = LockFile::acquire(LockFile::path_for(&database)).unwrap();
    let pool = initialize_new_under_lock(&database, &lock).await.unwrap();

    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .fetch_all(&pool)
            .await
            .unwrap();
    for required in ["strategies", "levels", "orders"] {
        assert!(tables.iter().any(|table| table == required));
    }
    let level_columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('levels')")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(level_columns.iter().any(|column| column == "strategy_id"));
    assert!(!level_columns.iter().any(|column| column == "venue"));
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");

    pool.close().await;
    drop(lock);
    cleanup(&database);
}

#[tokio::test]
async fn old_and_future_versions_are_rejected_without_touching_the_database() {
    for version in [2, CURRENT_SCHEMA_VERSION + 1] {
        let database = path(&format!("version-{version}"));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&database)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::raw_sql(&format!(
            "CREATE TABLE legacy(value TEXT); PRAGMA user_version = {version};"
        ))
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
        assert_rejected_unchanged(&database).await;
        cleanup(&database);
    }
}

#[tokio::test]
async fn unknown_file_is_rejected_without_sidecars_or_changes() {
    let database = path("unknown");
    std::fs::write(&database, b"not a sqlite database").unwrap();
    assert_rejected_unchanged(&database).await;
    cleanup(&database);
}

async fn assert_rejected_unchanged(database: &Path) {
    let before = std::fs::read(database).unwrap();
    let error = open_existing_current(database)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains(&database.display().to_string()));
    assert!(error.contains("remove this development database manually"));
    assert_eq!(std::fs::read(database).unwrap(), before);
    assert!(!PathBuf::from(format!("{}-wal", database.display())).exists());
    assert!(!PathBuf::from(format!("{}-shm", database.display())).exists());
}
