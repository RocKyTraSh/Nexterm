//! Remote file edit conflict detection.
//!
//! When a remote file is opened in an external editor, the local copy can go
//! stale if the file changes on the server before the edit is uploaded. We
//! snapshot (size, mtime) at download and re-check before upload — the
//! race-condition guard for "edit remote file in $EDITOR".

/// Snapshot of a remote file's identity at download time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSnapshot {
    pub size: u64,
    pub modified_unix: Option<i64>,
}

/// Result of comparing the current remote state to the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictCheck {
    /// Remote is unchanged; safe to upload.
    Safe,
    /// Remote changed since download; the UI must ask how to proceed.
    Conflict { snapshot: RemoteSnapshot, current: RemoteSnapshot },
}

/// Compare the snapshot taken at download against the current remote stat.
pub fn check_conflict(snapshot: &RemoteSnapshot, current: &RemoteSnapshot) -> ConflictCheck {
    if snapshot == current {
        ConflictCheck::Safe
    } else {
        ConflictCheck::Conflict { snapshot: snapshot.clone(), current: current.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_change() {
        let a = RemoteSnapshot { size: 100, modified_unix: Some(1000) };
        let b = RemoteSnapshot { size: 120, modified_unix: Some(1001) };
        assert_eq!(check_conflict(&a, &a), ConflictCheck::Safe);
        assert!(matches!(check_conflict(&a, &b), ConflictCheck::Conflict { .. }));
    }
}
