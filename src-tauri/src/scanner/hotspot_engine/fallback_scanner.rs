// ============================================================================
// Fallback scan engine - jwalk filesystem traversal + ancestor aggregation
// Fallback when MFT direct read is unavailable (not admin / not NTFS)
// Migrated from aggregate_ancestor_stats in hotspot.rs
// ============================================================================

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use jwalk::WalkDir as JWalkDir;

use crate::scanner::hotspot::{
    is_heavy_system_dir, is_hidden_by_path, FolderStats, HotspotScanner,
};

/// jwalk parallel traversal + ancestor aggregation (core scan engine)
///
/// Key advantages of jwalk over walkdir:
/// - Multi-threaded parallel directory listing, offsetting per-IO latency from Win11 Defender
/// - DirEntry caches metadata internally, so later `.metadata()` calls are zero-cost
/// - the process_read_dir callback pre-filters during the listing phase, preventing entry into huge directories like WinSxS
///
/// Each file is aggregated upward to **all** ancestor directories, ensuring every directory level's total_size includes all descendants.
///
/// Returns (root directory stats, a map of all ancestor directory stats)
pub fn aggregate_ancestor_stats(
    root: &Path,
    max_depth: u8,
    track_modified: bool,
    cancel_flag: &AtomicBool,
    ignore_system_dirs: bool,
) -> (FolderStats, HashMap<PathBuf, FolderStats>) {
    let mut root_stats = FolderStats {
        total_size: 0,
        file_count: 0,
        last_modified: 0,
    };
    let mut ancestor_map: HashMap<PathBuf, FolderStats> = HashMap::new();

    // jwalk parallel directory traversal; process_read_dir filters after listing and before recursion
    let walker = JWalkDir::new(root)
        .max_depth(max_depth as usize)
        .skip_hidden(false)
        .process_read_dir(move |_depth, _path, _state, children| {
            // Pre-filter: prevent jwalk from entering huge system directories and hidden directories
            // When the user disables system-directory filtering, WinSxS/DriverStore etc. are also allowed into the scan
            children.retain(|dir_entry_result| {
                dir_entry_result
                    .as_ref()
                    .map(|e| {
                        let p = e.path();
                        let skip_heavy = ignore_system_dirs && is_heavy_system_dir(&p);
                        !skip_heavy && !is_hidden_by_path(&p)
                    })
                    .unwrap_or(false) // Entries that fail to read cannot be entered; safely remove them
            });
        })
        .into_iter();

    for entry in walker {
        // Cancellation check
        if cancel_flag.load(Ordering::SeqCst) {
            break;
        }

        let e = match entry {
            Ok(e) => e,
            Err(_) => continue, // Silently skip errors such as permission denied
        };

        // Only process files
        if !e.file_type().is_file() {
            continue;
        }

        // jwalk's metadata() is cached and does not trigger extra syscalls
        let metadata = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let file_size = metadata.len();

        // Only collect modification time in Accurate mode
        let modified_ts = if track_modified {
            metadata
                .modified()
                .ok()
                .map(|t| HotspotScanner::system_time_to_millis(t))
                .unwrap_or(0)
        } else {
            0
        };

        // Accumulate into the root directory
        root_stats.total_size += file_size;
        root_stats.file_count += 1;
        if track_modified && modified_ts > root_stats.last_modified {
            root_stats.last_modified = modified_ts;
        }

        // Aggregate upward to all ancestor directories (ensuring each level includes the size of all descendant files)
        if let Ok(relative) = e.path().strip_prefix(root) {
            let comp_count = relative.components().count();
            let dir_ancestors = comp_count.saturating_sub(1);

            let mut current = root.to_path_buf();
            for comp in relative.components().take(dir_ancestors) {
                current.push(comp);
                let ancestor = ancestor_map
                    .entry(current.clone())
                    .or_insert_with(|| FolderStats {
                        total_size: 0,
                        file_count: 0,
                        last_modified: 0,
                    });
                ancestor.total_size += file_size;
                ancestor.file_count += 1;
                if track_modified && modified_ts > ancestor.last_modified {
                    ancestor.last_modified = modified_ts;
                }
            }
        }
    }

    (root_stats, ancestor_map)
}
