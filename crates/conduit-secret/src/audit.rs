use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::debug;

/// The type of operation performed on a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    Put,
    Get,
    Delete,
    Rotate,
}

/// A single audit log entry recording one secret access.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub ts: DateTime<Utc>,
    pub scope: String,
    pub id: String,
    pub action: AuditAction,
    /// Optional identifier for the caller (e.g. route ID, downstream key ID).
    pub caller: Option<String>,
}

impl AuditEntry {
    pub fn new(
        scope: impl Into<String>,
        id: impl Into<String>,
        action: AuditAction,
        caller: Option<String>,
    ) -> Self {
        Self {
            ts: Utc::now(),
            scope: scope.into(),
            id: id.into(),
            action,
            caller,
        }
    }
}

/// In-memory ring buffer audit log.  All entries are held in memory; callers
/// that need persistence should drain via `drain()` and write to their own
/// store.
///
/// The buffer is bounded by `capacity` to prevent unbounded growth in
/// long-running processes.  When full, the oldest entry is dropped.
#[derive(Debug, Clone)]
pub struct SecretAuditLog {
    entries: Arc<Mutex<std::collections::VecDeque<AuditEntry>>>,
    capacity: usize,
}

impl SecretAuditLog {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(std::collections::VecDeque::with_capacity(
                capacity,
            ))),
            capacity,
        }
    }

    /// Record an audit event.
    pub async fn record(&self, entry: AuditEntry) {
        debug!(
            scope = %entry.scope,
            id = %entry.id,
            action = ?entry.action,
            caller = ?entry.caller,
            "audit"
        );
        let mut guard = self.entries.lock().await;
        if guard.len() >= self.capacity {
            guard.pop_front();
        }
        guard.push_back(entry);
    }

    /// Convenience helper — record a secret access with the given parameters.
    pub async fn log(&self, scope: &str, id: &str, action: AuditAction, caller: Option<String>) {
        self.record(AuditEntry::new(scope, id, action, caller))
            .await;
    }

    /// Drain all buffered entries, clearing the log.
    pub async fn drain(&self) -> Vec<AuditEntry> {
        let mut guard = self.entries.lock().await;
        guard.drain(..).collect()
    }

    /// Snapshot the current entries without clearing the log.
    pub async fn snapshot(&self) -> Vec<AuditEntry> {
        let guard = self.entries.lock().await;
        guard.iter().cloned().collect()
    }

    /// Current number of entries in the buffer.
    pub async fn len(&self) -> usize {
        self.entries.lock().await.len()
    }

    /// Returns true if the buffer has no entries.
    pub async fn is_empty(&self) -> bool {
        self.entries.lock().await.is_empty()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn records_and_drains() {
        let log = SecretAuditLog::new(10);
        log.log("upstream_key", "key-001", AuditAction::Put, None)
            .await;
        log.log(
            "upstream_key",
            "key-001",
            AuditAction::Get,
            Some("route-1".into()),
        )
        .await;

        let entries = log.drain().await;
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0].action, AuditAction::Put));
        assert!(matches!(entries[1].action, AuditAction::Get));
        assert_eq!(entries[1].caller.as_deref(), Some("route-1"));

        // Drain should clear.
        assert_eq!(log.len().await, 0);
    }

    #[tokio::test]
    async fn bounded_capacity_drops_oldest() {
        let log = SecretAuditLog::new(3);
        for i in 0..5u32 {
            log.log("s", &i.to_string(), AuditAction::Get, None).await;
        }
        assert_eq!(log.len().await, 3);
        let entries = log.snapshot().await;
        // The three most-recent entries should be 2, 3, 4.
        assert_eq!(entries[0].id, "2");
        assert_eq!(entries[2].id, "4");
    }
}
