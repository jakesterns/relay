//! Disk-budget planning for the recording folder: a total-size cap with
//! oldest-first pruning, and a free-space floor below which recording stops.
//! Pure — the caller supplies the file list and free space and applies the
//! plan (delete these, then maybe stop).

use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct DiskBudget {
    /// Total bytes the recording folder may hold (0 = uncapped).
    pub cap_bytes: u64,
    /// Recording stops rather than let free space fall below this.
    pub free_floor_bytes: u64,
}

impl Default for DiskBudget {
    /// The agreed budget: 50 GB cap, 10 GB free-space floor.
    fn default() -> Self {
        Self { cap_bytes: 50 * 1024 * 1024 * 1024, free_floor_bytes: 10 * 1024 * 1024 * 1024 }
    }
}

/// One finished recording on disk.
#[derive(Debug, Clone)]
pub struct RecordingFile {
    pub path: PathBuf,
    pub bytes: u64,
    /// Seconds since the epoch (mtime); older = pruned first.
    pub modified_secs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BudgetPlan {
    /// Files to delete, oldest first.
    pub delete: Vec<PathBuf>,
    /// True when recording must stop: even after pruning, free space would
    /// sit below the floor.
    pub stop: bool,
}

/// Decide what to prune and whether recording can continue. `files` are the
/// finished recordings (the caller must exclude the file currently being
/// written); `free_bytes` is the volume's current free space.
pub fn plan(budget: &DiskBudget, files: &[RecordingFile], free_bytes: u64) -> BudgetPlan {
    let mut sorted: Vec<&RecordingFile> = files.iter().collect();
    sorted.sort_by_key(|f| f.modified_secs);

    let mut total: u64 = sorted.iter().map(|f| f.bytes).sum();
    let mut delete = Vec::new();
    let mut reclaimed = 0u64;
    if budget.cap_bytes > 0 {
        for f in &sorted {
            if total <= budget.cap_bytes {
                break;
            }
            delete.push(f.path.clone());
            total -= f.bytes;
            reclaimed += f.bytes;
        }
    }
    // Pruning frees space; stop only if the floor is still unreachable.
    let stop = free_bytes.saturating_add(reclaimed) < budget.free_floor_bytes;
    BudgetPlan { delete, stop }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    fn file(name: &str, gb: u64, age: u64) -> RecordingFile {
        RecordingFile { path: PathBuf::from(name), bytes: gb * GB, modified_secs: age }
    }

    #[test]
    fn under_cap_and_floor_nothing_happens() {
        let p = plan(&DiskBudget::default(), &[file("a.mp4", 20, 1)], 100 * GB);
        assert_eq!(p, BudgetPlan::default());
    }

    #[test]
    fn prunes_oldest_first_until_under_cap() {
        let files =
            [file("new.mp4", 20, 300), file("oldest.mp4", 20, 100), file("mid.mp4", 20, 200)];
        let p = plan(&DiskBudget::default(), &files, 100 * GB);
        // 60 GB total, 50 GB cap → dropping the 20 GB oldest suffices.
        assert_eq!(p.delete, vec![PathBuf::from("oldest.mp4")]);
        assert!(!p.stop);

        let files = [file("a.mp4", 30, 1), file("b.mp4", 30, 2), file("c.mp4", 30, 3)];
        let p = plan(&DiskBudget::default(), &files, 100 * GB);
        assert_eq!(p.delete, vec![PathBuf::from("a.mp4"), PathBuf::from("b.mp4")]);
    }

    #[test]
    fn stops_when_floor_unreachable_even_after_pruning() {
        // 5 GB free, floor 10 GB, nothing to prune → stop.
        let p = plan(&DiskBudget::default(), &[], 5 * GB);
        assert!(p.stop);

        // 5 GB free but pruning reclaims 60 GB → keep going.
        let files = [file("a.mp4", 60, 1), file("b.mp4", 40, 2)];
        let p = plan(&DiskBudget::default(), &files, 5 * GB);
        assert_eq!(p.delete, vec![PathBuf::from("a.mp4")]);
        assert!(!p.stop);
    }

    #[test]
    fn zero_cap_means_uncapped_but_floor_still_applies() {
        let budget = DiskBudget { cap_bytes: 0, free_floor_bytes: 10 * GB };
        let files = [file("a.mp4", 500, 1)];
        let p = plan(&budget, &files, 50 * GB);
        assert!(p.delete.is_empty());
        assert!(!p.stop);
        let p = plan(&budget, &files, 9 * GB);
        assert!(p.stop, "uncapped never prunes, so a low floor stops recording");
    }
}
