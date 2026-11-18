// ============================================================================
// Scan cancellation - the single owner of every scan's cancellation flag
//
// M13 audit fix: four subsystems (deep junk, big files, hotspot, disk growth)
// each declared their own `static AtomicBool` with identical reset/cancel/load
// plumbing, so cancelling one scan kind had four spellings and a new scanner
// meant copying a fifth. The flags now live here, keyed by scan kind; scanner
// modules keep no cancellation state of their own.
// ============================================================================

use std::sync::atomic::{AtomicBool, Ordering};

/// Which scan a flag belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    /// Deep junk scan (`scanner/deep_junk.rs`)
    DeepJunk,
    /// Big file scan (`scanner/big_files.rs`)
    BigFiles,
    /// Large-directory scan (`scanner/hotspot.rs`)
    Hotspot,
    /// Full-disk growth analysis (`disk_growth/mft_scan.rs`)
    DiskGrowth,
}

impl ScanKind {
    /// Human-readable name for cancel logs.
    fn label(self) -> &'static str {
        match self {
            ScanKind::DeepJunk => "深度垃圾扫描",
            ScanKind::BigFiles => "大文件扫描",
            ScanKind::Hotspot => "大目录扫描",
            ScanKind::DiskGrowth => "全盘增长分析",
        }
    }

    fn index(self) -> usize {
        match self {
            ScanKind::DeepJunk => 0,
            ScanKind::BigFiles => 1,
            ScanKind::Hotspot => 2,
            ScanKind::DiskGrowth => 3,
        }
    }
}

/// One flag per scan kind; a scan kind never observes another kind's flag.
static FLAGS: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];

/// Clear the flag before a scan of `kind` starts.
pub fn reset(kind: ScanKind) {
    FLAGS[kind.index()].store(false, Ordering::SeqCst);
}

/// Set the flag when the user cancels the scan of `kind`.
pub fn cancel(kind: ScanKind) {
    log::info!("收到取消{}请求", kind.label());
    FLAGS[kind.index()].store(true, Ordering::SeqCst);
}

/// Whether the scan of `kind` has been cancelled.
pub fn is_cancelled(kind: ScanKind) -> bool {
    FLAGS[kind.index()].load(Ordering::SeqCst)
}

/// Direct access to the flag, for shared traversal helpers that take a
/// `&AtomicBool` (the hotspot scan passes its flag into the MFT/jwalk walkers).
pub fn flag(kind: ScanKind) -> &'static AtomicBool {
    &FLAGS[kind.index()]
}

/// `Err("扫描已取消")` when cancelled, for scan loops that propagate results.
pub fn check(kind: ScanKind) -> Result<(), String> {
    if is_cancelled(kind) {
        Err("扫描已取消".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flags are per-kind: cancelling one scan must not cancel the others.
    #[test]
    fn flags_are_independent_per_scan_kind() {
        let kinds = [
            ScanKind::DeepJunk,
            ScanKind::BigFiles,
            ScanKind::Hotspot,
            ScanKind::DiskGrowth,
        ];

        for kind in kinds {
            reset(kind);
            assert!(!is_cancelled(kind));
            assert!(check(kind).is_ok());

            cancel(kind);
            assert!(is_cancelled(kind));
            assert_eq!(check(kind).unwrap_err(), "扫描已取消");

            // The other kinds are unaffected
            for other in kinds {
                if other != kind {
                    assert!(!is_cancelled(other), "{:?} leaked into {:?}", kind, other);
                }
            }

            reset(kind);
            assert!(!is_cancelled(kind));
        }
    }

    /// The shared traversal helpers take a `&AtomicBool`, so the reference must
    /// point at the same cell `is_cancelled` reads.
    #[test]
    fn flag_reference_hits_the_same_cell() {
        let kind = ScanKind::BigFiles;
        reset(kind);
        let cell = flag(kind);
        cell.store(true, Ordering::SeqCst);
        assert!(is_cancelled(kind));
        reset(kind);
        assert!(!cell.load(Ordering::SeqCst));
    }
}
