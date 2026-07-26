use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tempo_agentic_storage::LockFile;

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "tempo-agentic-storage-{name}-{}-{}.lock",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

// One database must not run two daemons.
#[test]
fn a_second_claim_on_the_same_database_is_refused() {
    let path = scratch("busy");
    let held = LockFile::acquire(&path).unwrap();

    let error = LockFile::acquire(&path).unwrap_err().to_string();
    assert!(
        error.contains(&std::process::id().to_string()),
        "the refusal has to name the holder so a live daemon can be told from a \
         leftover: {error}"
    );

    drop(held);
    // Releasing has to actually free it, or a restart would need manual cleanup.
    LockFile::acquire(&path).unwrap();
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_lock_sits_next_to_the_database() {
    assert_eq!(
        LockFile::path_for(&PathBuf::from("/var/lib/tempo/state.db")),
        PathBuf::from("/var/lib/tempo/state.db.lock")
    );
}
