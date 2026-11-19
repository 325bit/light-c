// ============================================================================
// Permanent delete engine - deep cleanup of uninstall leftovers
//
// ⚠️ WARNING: this module performs direct physical deletion; data cannot be recovered!
//
// [Core functionality]
// Directly use std::fs::remove_dir_all and std::fs::remove_file to permanently delete files from disk,
// rather than moving them to the recycle bin. This is to thoroughly clean up leftover data from uninstalled software.
//
// [Why check .exe files?]
// Checking whether .exe/.dll/.sys files exist in a folder before deletion is crucial, for these reasons:
// 1. Portable apps: users may run portable software placed under AppData,
//    which has no installation record but is still in active use.
// 2. Auto-start programs: some executables may reside in AppData and be configured to start automatically.
// 3. System components: DLL files of some system services may be located in scanned directories.
// 4. Driver files: .sys files are kernel drivers; deleting them by mistake may crash the system.
//
// If such files are detected, we skip the folder and mark it as "requires manual review",
// letting the user decide whether to delete it, thus avoiding accidental deletion of software in use.
//
// [Safety check protocol]
// Check 1: protection rules - the shared `path_guard` table plus the user whitelist
// Check 2: executable file check - scan for .exe/.dll/.sys files; if found, mark for manual review
//
// Note: the registry matching check is no longer performed, because the scoring engine has already confirmed the target is an uninstall leftover,
// and registry keys of uninstalled programs are themselves leftover data (zombie entry), so they must not be used to block cleanup.
//
// [Error handling]
// - All IO operations are wrapped in Result
// - When a file is locked, the shared ladder queues it for deletion on reboot
// - When permissions are insufficient, it clears protection attributes and retries
//   (it never takes ownership: an uninstall leftover is not a cache directory)
// ============================================================================

use std::path::Path;

use log::{info, warn};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use super::delete_attempt::{
    attempt_delete, AttemptPolicy, AttributeClearance, DeleteOutcome, OutcomeKind, RebootPolicy,
};
use crate::error::ErrorCode;
use crate::path_guard::{ensure_deletable, DeleteScope};
use crate::scanner::leftover_whitelist::{self, LeftoverWhitelistEntry};

// ============================================================================
// Safety check result type
// ============================================================================

/// Safety check result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SafetyCheckResult {
    /// Passed all checks, safe to delete
    Safe,
    /// A match was found in the registry (legacy variant; the check is no longer run)
    FoundInRegistry {
        matched_field: String,
        matched_value: String,
    },
    /// Executable files were found (executable check failed)
    ContainsExecutables { files: Vec<String> },
    /// The path is inside a system-protected directory, or a whitelist refused it
    InProtectedPath { reason: String },
}

impl SafetyCheckResult {
    pub fn is_safe(&self) -> bool {
        matches!(self, SafetyCheckResult::Safe)
    }

    /// Machine-readable category for the frontend. Every failing check is a
    /// protection rule, so they all map to `Protected` — the distinction that
    /// matters to the UI is carried by the variant itself, which is already
    /// serialized alongside.
    pub fn error_code(&self) -> Option<ErrorCode> {
        match self {
            SafetyCheckResult::Safe => None,
            SafetyCheckResult::FoundInRegistry { .. }
            | SafetyCheckResult::ContainsExecutables { .. }
            | SafetyCheckResult::InProtectedPath { .. } => Some(ErrorCode::Protected),
        }
    }

    pub fn display_message(&self) -> String {
        match self {
            SafetyCheckResult::Safe => "安全".to_string(),
            SafetyCheckResult::FoundInRegistry {
                matched_field,
                matched_value,
            } => {
                format!("注册表中存在匹配: {} = {}", matched_field, matched_value)
            }
            SafetyCheckResult::ContainsExecutables { files } => {
                let count = files.len();
                let preview: Vec<_> = files.iter().take(3).cloned().collect();
                if count > 3 {
                    format!("包含 {} 个可执行文件: {} 等", count, preview.join(", "))
                } else {
                    format!("包含可执行文件: {}", preview.join(", "))
                }
            }
            SafetyCheckResult::InProtectedPath { reason } => {
                format!("系统保护路径: {}", reason)
            }
        }
    }
}

/// Deletion result for a single leftover
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeftoverDeleteResult {
    /// Folder path
    pub path: String,
    /// Whether deletion succeeded
    pub success: bool,
    /// Number of files deleted
    pub deleted_files: u32,
    /// Space freed (bytes)
    pub freed_size: u64,
    /// Failure reason (free-form text, shown in tooltips and logs)
    pub failure_reason: Option<String>,
    /// Machine-readable failure category, so the UI never has to sniff `failure_reason` text.
    /// `None` on success; absent from the wire when `None` keeps old payloads compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<ErrorCode>,
    /// Whether it is marked for deletion on reboot
    pub marked_for_reboot: bool,
    /// Whether manual review is required
    pub needs_manual_review: bool,
    /// Safety check result
    pub safety_check: SafetyCheckResult,
}

/// Overall result of permanent deletion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermanentDeleteResult {
    /// Number of folders successfully deleted
    pub success_count: usize,
    /// Number of folders that failed
    pub failed_count: usize,
    /// Number requiring manual review
    pub manual_review_count: usize,
    /// Number marked for deletion on reboot
    pub reboot_pending_count: usize,
    /// Actual space freed (bytes)
    pub freed_size: u64,
    /// Detailed results for each folder
    pub details: Vec<LeftoverDeleteResult>,
    /// Deletion duration (milliseconds)
    pub duration_ms: u64,
}

// ============================================================================
// Safety check constants
//
// The protected-path table lives in `path_guard` (S3a). This module only owns
// the executable-extension list, which no other engine uses.
// ============================================================================

/// Executable file extensions (the executable check)
const EXECUTABLE_EXTENSIONS: &[&str] = &["exe", "dll", "sys", "drv", "ocx", "cpl", "scr"];

/// How deep the executable check looks.
///
/// Five levels is "the app's own entry point and its helpers". A deeper `.dll` is
/// data shipped by a portable app, and flagging it would push most of AppData
/// into manual review.
const EXECUTABLE_SCAN_DEPTH: usize = 5;

/// How deep the size accounting looks.
///
/// The number reported to the user is "how much this will free", not a filesystem
/// audit, so a pathological tree is truncated at this depth rather than walked.
const SIZE_SCAN_DEPTH: usize = 20;

/// The executable check is a sample: the UI lists at most ten names.
const EXECUTABLE_PREVIEW_LIMIT: usize = 10;

// ============================================================================
// One traversal, two answers (M8)
// ============================================================================

/// What one walk of a leftover directory found.
///
/// Before M8 the delete path walked every directory twice: once for the
/// executable check (depth 5, stopping at ten hits) and once for
/// `calculate_dir_size` (depth 20), then a third time inside the ladder if the
/// delete failed. The first two are the same walk — same tree, same order, only
/// a different depth limit and stop condition — so they are fused here.
///
/// The two limits are preserved rather than averaged: names are collected only
/// down to `EXECUTABLE_SCAN_DEPTH` and only up to `EXECUTABLE_PREVIEW_LIMIT`,
/// while the walk itself continues to `SIZE_SCAN_DEPTH` when sizes are wanted.
/// Widening the depth to 20 would have changed which directories the user is
/// told to review by hand; that is a product decision, not a traversal detail.
///
/// What is *not* fused: the ladder's own walk on rung 2 (clear attributes) and
/// rung 4 (queue for reboot). Both run only after the direct delete failed, and
/// merging them here would mean rewriting attributes across a tree that is about
/// to disappear, or building a reboot list for a path that deletes fine.
#[derive(Debug, Default)]
struct LeftoverSurvey {
    total_size: u64,
    file_count: u32,
    executables: Vec<String>,
}

/// Which answers a walk owes the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurveyScope {
    /// Executable preview only, with the early stop. Used by the standalone
    /// safety-check command, which has no use for a byte count and should not
    /// pay for a depth-20 walk.
    Executables,
    /// Executable preview plus size and file count. Used by the delete path,
    /// which reports both.
    Full,
}

fn survey_tree(path: &Path, scope: SurveyScope) -> LeftoverSurvey {
    let measure = scope == SurveyScope::Full;
    let max_depth = if measure {
        SIZE_SCAN_DEPTH
    } else {
        EXECUTABLE_SCAN_DEPTH
    };

    let mut survey = LeftoverSurvey::default();

    for entry in WalkDir::new(path)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        // The standalone check can stop as soon as the preview is full; the
        // delete path cannot, because it still owes the caller a byte count.
        if !measure && survey.executables.len() >= EXECUTABLE_PREVIEW_LIMIT {
            break;
        }

        if !entry.file_type().is_file() {
            continue;
        }

        if measure {
            if let Ok(metadata) = entry.metadata() {
                survey.total_size += metadata.len();
                survey.file_count += 1;
            }
        }

        if entry.depth() <= EXECUTABLE_SCAN_DEPTH
            && survey.executables.len() < EXECUTABLE_PREVIEW_LIMIT
        {
            if let Some(extension) = entry.path().extension() {
                let extension = extension.to_string_lossy().to_lowercase();
                if EXECUTABLE_EXTENSIONS.contains(&extension.as_str()) {
                    survey
                        .executables
                        .push(entry.file_name().to_string_lossy().to_string());
                }
            }
        }
    }

    survey
}

/// The last safety check, given the executable preview.
///
/// Note: the registry matching check is no longer performed. Calling deep
/// cleanup requires the scoring engine to have already determined the folder is
/// an uninstall leftover, and the registry keys of an uninstalled program are
/// themselves leftovers (zombie entries), so using them to block the deletion of
/// leftover folders is self-contradictory. The whitelist plus the executable
/// check already carry the safety boundary.
fn executable_verdict(allow_executables: bool, executables: &[String]) -> SafetyCheckResult {
    if !allow_executables && !executables.is_empty() {
        return SafetyCheckResult::ContainsExecutables {
            files: executables.to_vec(),
        };
    }

    SafetyCheckResult::Safe
}

// ============================================================================
// Permanent delete engine
// ============================================================================

/// Permanent delete engine
///
/// Explanation
/// This engine performs deep cleanup of uninstall leftovers. Unlike normal deletion, it physically deletes files directly from disk,
/// without going through the recycle bin, so data cannot be recovered.
///
/// To ensure safety, a "triple safety check protocol" is run before every deletion:
/// 1. Registry check: confirm the target is not part of an installed program
/// 2. Executable check: confirm the target does not contain a program in use
/// 3. Whitelist check: confirm the target is not inside a critical system path
pub struct PermanentDeleteEngine {
    /// Whether delete-on-reboot fallback is enabled
    enable_reboot_fallback: bool,
    /// Fix the whitelist snapshot when building the delete engine, so concurrent batch deletions do not need to re-read the disk for every path.
    user_whitelist: Result<Vec<LeftoverWhitelistEntry>, String>,
}

impl PermanentDeleteEngine {
    /// Create a new permanent delete engine
    pub fn new() -> Self {
        info!("永久删除引擎初始化完成");

        PermanentDeleteEngine {
            enable_reboot_fallback: true,
            user_whitelist: leftover_whitelist::list_entries(),
        }
    }

    // ========================================================================
    // Safety check protocol (whitelist + executable check)
    // ========================================================================

    /// Perform the full triple safety check
    ///
    /// Explanation
    /// This function runs the three safety checks in sequence; any failure blocks the deletion.
    /// This is the core mechanism protecting user data safety.
    pub fn perform_safety_checks(&self, path: &Path) -> SafetyCheckResult {
        self.perform_safety_checks_with(path, false)
    }

    /// Same as above, but may optionally **allow the "contains executable files" check**.
    ///
    /// This is only reached when the user explicitly clicks "Force Delete" in the results dialog:
    /// whitelist protection, system-directory protection, and validation of the leftover path itself **are never skipped** —
    /// the only check relaxed is "reject if the directory contains .exe/.dll/.sys",
    /// because the most common uninstall leftovers are directories containing programs (emulators, portable software).
    fn perform_safety_checks_with(
        &self,
        path: &Path,
        allow_executables: bool,
    ) -> SafetyCheckResult {
        if let Some(failure) = self.gate_failure(path) {
            return failure;
        }

        // A standalone check only needs the executable preview, so it walks to
        // depth 5 and stops early (M8).
        let survey = survey_tree(path, SurveyScope::Executables);
        executable_verdict(allow_executables, &survey.executables)
    }

    /// The checks that need no filesystem walk.
    ///
    /// Traversal, the shared protection table, the shell-folder rules for the
    /// `Leftovers` scope, and the user whitelist. Returns the failing verdict, or
    /// `None` when the path is still a candidate.
    ///
    /// Split out from the executable check so the delete path can run it
    /// *before* surveying the tree: a path the protection table refuses must not
    /// cost a depth-20 walk first.
    fn gate_failure(&self, path: &Path) -> Option<SafetyCheckResult> {
        let path_str = path.to_string_lossy().to_string();

        // S3a: traversal, the shared protection table, and the shell-folder rules
        // for this scope are one call now. `Leftovers` is the strictest scope: a
        // "leftover" inside Documents or on the Desktop is a scanner bug, so those
        // subtrees are refused here even though large-file cleanup may touch them.
        if let Err(error) = ensure_deletable(path, DeleteScope::Leftovers) {
            return Some(SafetyCheckResult::InProtectedPath {
                reason: error.detail.unwrap_or(error.message),
            });
        }

        // If the protection rules cannot be loaded, reject by default, so a config problem cannot weaken the permanent-delete safety boundary.
        let user_whitelist = match &self.user_whitelist {
            Ok(entries) => entries,
            Err(error) => {
                return Some(SafetyCheckResult::InProtectedPath {
                    reason: format!("user whitelist could not be loaded: {error}"),
                });
            }
        };

        if leftover_whitelist::contains_path(user_whitelist, path) {
            return Some(SafetyCheckResult::InProtectedPath {
                reason: format!("path is on the user whitelist: {path_str}"),
            });
        }

        None
    }

    // ========================================================================
    // Deletion execution
    // ========================================================================

    /// Perform permanent deletion
    ///
    /// Explanation
    /// Directories are deleted one at a time (M7). The caller already runs this on
    /// a blocking thread, so nothing here needs a thread pool to stay off the UI
    /// thread.
    ///
    /// This used to run on rayon's *global* pool, which meant a queue of
    /// `remove_dir_all` calls blocked every other rayon user in the process for as
    /// long as the delete took, and on a mechanical disk it made the head thrash
    /// between directories. The work is I/O bound, the queue is what the user
    /// selected — tens of directories, not thousands — and a fixed order makes a
    /// partial failure easier to reason about than a completion order that depends
    /// on how the pool scheduled it.
    ///
    /// Dropping the pool also drops the atomics: with `&mut` counters available the
    /// accounting is plain arithmetic, so the numbers cannot be read mid-update.
    pub fn delete_leftovers(
        &self,
        paths: Vec<String>,
        allow_executables: bool,
    ) -> PermanentDeleteResult {
        let start_time = std::time::Instant::now();

        let mut success_count = 0usize;
        let mut failed_count = 0usize;
        let mut manual_review_count = 0usize;
        let mut reboot_pending_count = 0usize;
        let mut freed_size = 0u64;
        let mut details: Vec<LeftoverDeleteResult> = Vec::with_capacity(paths.len());

        for path_str in &paths {
            let path = Path::new(path_str);

            // The cheap checks first (M8): a path the protection table refuses
            // must not pay for a walk of the tree it protects.
            if let Some(safety_check) = self.gate_failure(path) {
                failed_count += 1;
                details.push(LeftoverDeleteResult {
                    path: path_str.clone(),
                    success: false,
                    deleted_files: 0,
                    freed_size: 0,
                    failure_reason: Some(safety_check.display_message()),
                    failure_code: safety_check.error_code(),
                    marked_for_reboot: false,
                    needs_manual_review: false,
                    safety_check,
                });
                continue;
            }

            // One walk answers both questions: the executable preview the safety
            // check needs and the size / file count the result reports.
            let survey = survey_tree(path, SurveyScope::Full);
            let safety_check = executable_verdict(allow_executables, &survey.executables);

            match &safety_check {
                SafetyCheckResult::Safe => {
                    // Passed the safety check, perform the deletion
                    let (result, kind) = self.delete_single_leftover(path, &survey);

                    // Counted by the shared bucket, not by re-reading the two
                    // booleans on the result struct.
                    match kind {
                        OutcomeKind::Freed => {
                            success_count += 1;
                            freed_size += result.freed_size;
                        }
                        OutcomeKind::RebootPending => reboot_pending_count += 1,
                        OutcomeKind::Failed => failed_count += 1,
                    }

                    details.push(result);
                }
                SafetyCheckResult::ContainsExecutables { .. } => {
                    // Contains executable files, mark as requiring manual review
                    manual_review_count += 1;

                    details.push(LeftoverDeleteResult {
                        path: path_str.clone(),
                        success: false,
                        deleted_files: 0,
                        freed_size: 0,
                        failure_reason: Some(safety_check.display_message()),
                        failure_code: safety_check.error_code(),
                        marked_for_reboot: false,
                        needs_manual_review: true,
                        safety_check,
                    });
                }
                _ => {
                    // Other safety checks failed
                    failed_count += 1;

                    details.push(LeftoverDeleteResult {
                        path: path_str.clone(),
                        success: false,
                        deleted_files: 0,
                        freed_size: 0,
                        failure_reason: Some(safety_check.display_message()),
                        failure_code: safety_check.error_code(),
                        marked_for_reboot: false,
                        needs_manual_review: false,
                        safety_check,
                    });
                }
            }
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;

        info!(
            "永久删除完成: 成功 {}, 失败 {}, 待审核 {}, 待重启 {}, 释放 {} 字节, 耗时 {}ms",
            success_count,
            failed_count,
            manual_review_count,
            reboot_pending_count,
            freed_size,
            duration_ms
        );

        PermanentDeleteResult {
            success_count,
            failed_count,
            manual_review_count,
            reboot_pending_count,
            freed_size,
            details,
            duration_ms,
        }
    }

    /// Delete a single leftover directory
    ///
    /// Explanation
    /// This function performs the actual physical deletion. It first tries direct deletion,
    /// and automatically switches to the "delete on reboot" queue when locked files are encountered.
    /// Returns the bucket alongside the result, so the batch loop does not have
    /// to reconstruct it from `success` / `marked_for_reboot`.
    ///
    /// `survey` is the walk the caller already made for the safety check (M8):
    /// the size has to be measured before the delete, and measuring it here would
    /// be a second walk of the same tree.
    fn delete_single_leftover(
        &self,
        path: &Path,
        survey: &LeftoverSurvey,
    ) -> (LeftoverDeleteResult, OutcomeKind) {
        let path_str = path.to_string_lossy().to_string();
        let (total_size, file_count) = (survey.total_size, survey.file_count);

        // ====================================================================
        // WARNING: the following code performs permanent deletion; data cannot be recovered!
        // ====================================================================

        // S3b: one call into the shared ladder. Leftovers get the same two rungs
        // as junk clean — attributes (only when the refusal was about permissions,
        // because a locked tree is not an attribute problem) and the reboot queue.
        // Never ownership: an uninstall leftover under Program Files is not a
        // cache directory, and handing Administrators full control of it is not
        // something a "delete leftovers" click authorises.
        let policy = AttemptPolicy {
            clear_attributes: AttributeClearance::OnPermissionDenied,
            take_ownership: false,
            ownership_for_caches: false,
            reboot: if self.enable_reboot_fallback {
                RebootPolicy::EntryScope
            } else {
                RebootPolicy::Off
            },
        };

        let outcome = attempt_delete(path, DeleteScope::Leftovers, &policy);
        let kind = outcome.kind();

        let result = match &outcome {
            DeleteOutcome::Freed => {
                info!(
                    "成功永久删除: {} ({} 文件, {} 字节)",
                    path_str, file_count, total_size
                );

                LeftoverDeleteResult {
                    path: path_str,
                    success: true,
                    deleted_files: file_count,
                    freed_size: total_size,
                    failure_reason: None,
                    failure_code: None,
                    marked_for_reboot: false,
                    needs_manual_review: false,
                    safety_check: SafetyCheckResult::Safe,
                }
            }
            DeleteOutcome::RebootPending => {
                info!("已标记为重启后删除: {}", path_str);
                LeftoverDeleteResult {
                    path: path_str,
                    success: false,
                    deleted_files: 0,
                    freed_size: 0,
                    failure_reason: Some("已标记为重启后删除".to_string()),
                    failure_code: Some(ErrorCode::TargetInUse),
                    marked_for_reboot: true,
                    needs_manual_review: false,
                    safety_check: SafetyCheckResult::Safe,
                }
            }
            DeleteOutcome::Failed(error) => {
                warn!("删除失败: {} - {}", path_str, error.message);
                LeftoverDeleteResult {
                    path: path_str,
                    success: false,
                    deleted_files: 0,
                    freed_size: 0,
                    failure_reason: Some(format!("删除失败: {}", error.message)),
                    // S3b: the code comes from the ladder's OS-error mapping now,
                    // not from `classify_delete_failure`, which had to sniff the
                    // formatted message for "os error N" because the numeric code
                    // had already been formatted away one frame up.
                    failure_code: Some(error.code),
                    marked_for_reboot: false,
                    needs_manual_review: false,
                    safety_check: SafetyCheckResult::Safe,
                }
            }
        };

        (result, kind)
    }
}

impl Default for PermanentDeleteEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A fresh directory under the system temp dir, unique per test.
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lightc-m8-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    /// One walk has to answer both questions, with the same numbers the two
    /// walks it replaced produced.
    #[test]
    fn the_survey_counts_files_and_bytes_across_the_tree() {
        let root = scratch_dir("survey-counts");
        let nested = root.join("a").join("b");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(root.join("one.bin"), vec![0u8; 100]).expect("write");
        fs::write(nested.join("two.bin"), vec![0u8; 250]).expect("write");

        let survey = survey_tree(&root, SurveyScope::Full);
        assert_eq!(survey.total_size, 350);
        assert_eq!(survey.file_count, 2);
        assert!(survey.executables.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    /// The preview is a sample, not an inventory: the UI lists ten names.
    #[test]
    fn the_survey_stops_collecting_executables_at_the_preview_limit() {
        let root = scratch_dir("survey-cap");
        for index in 0..(EXECUTABLE_PREVIEW_LIMIT + 5) {
            fs::write(root.join(format!("app{index}.exe")), b"x").expect("write");
        }

        let survey = survey_tree(&root, SurveyScope::Full);
        assert_eq!(survey.executables.len(), EXECUTABLE_PREVIEW_LIMIT);
        // Every file is still counted: the cap applies to names, not to bytes.
        assert_eq!(survey.file_count, (EXECUTABLE_PREVIEW_LIMIT + 5) as u32);

        let _ = fs::remove_dir_all(&root);
    }

    /// The executable check stops at depth 5 and the size accounting at depth 20;
    /// fusing the walks must not widen either. A deep `.dll` is a portable app's
    /// payload, and counting it as "contains executables" would move the whole
    /// tree into manual review.
    #[test]
    fn the_survey_keeps_the_two_depth_limits_apart() {
        let root = scratch_dir("survey-depth");
        let mut deep = root.clone();
        for level in 0..(EXECUTABLE_SCAN_DEPTH + 1) {
            deep = deep.join(format!("d{level}"));
        }
        fs::create_dir_all(&deep).expect("create deep dir");
        fs::write(deep.join("payload.dll"), vec![0u8; 64]).expect("write deep");
        fs::write(root.join("entry.exe"), b"x").expect("write shallow");

        let survey = survey_tree(&root, SurveyScope::Full);
        // Only the shallow one: the deep .dll is past EXECUTABLE_SCAN_DEPTH.
        assert_eq!(survey.executables, vec!["entry.exe".to_string()]);
        // Both files are inside SIZE_SCAN_DEPTH, so both are counted.
        assert_eq!(survey.file_count, 2);
        assert_eq!(survey.total_size, 65);

        let _ = fs::remove_dir_all(&root);
    }

    /// The standalone check must not pay for the depth-20 walk it does not use.
    #[test]
    fn the_executables_scope_does_not_measure() {
        let root = scratch_dir("survey-scope");
        fs::write(root.join("entry.exe"), vec![0u8; 32]).expect("write");

        let survey = survey_tree(&root, SurveyScope::Executables);
        assert_eq!(survey.executables, vec!["entry.exe".to_string()]);
        assert_eq!(survey.total_size, 0);
        assert_eq!(survey.file_count, 0);

        let _ = fs::remove_dir_all(&root);
    }

    /// End to end: the size the result reports is the survey's, and a refused
    /// path still produces exactly one result, in the order it was requested.
    #[test]
    fn deleting_reports_one_result_per_path_in_request_order() {
        let root = scratch_dir("delete-order");
        let deletable = root.join("OldApp");
        fs::create_dir_all(&deletable).expect("create");
        fs::write(deletable.join("data.bin"), vec![0u8; 512]).expect("write");

        let refused = r"C:\Windows\System32\config".to_string();
        let paths = vec![deletable.to_string_lossy().to_string(), refused.clone()];

        let engine = PermanentDeleteEngine::new();
        let result = engine.delete_leftovers(paths, true);

        assert_eq!(result.details.len(), 2);
        assert_eq!(result.details[0].path, deletable.to_string_lossy());
        assert!(result.details[0].success);
        assert_eq!(result.details[0].freed_size, 512);
        assert_eq!(result.details[0].deleted_files, 1);
        assert_eq!(result.details[1].path, refused);
        assert!(!result.details[1].success);
        assert_eq!(result.success_count, 1);
        assert_eq!(result.failed_count, 1);
        assert_eq!(result.freed_size, 512);
        assert!(!deletable.exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn safety_failures_carry_the_protected_code() {
        assert_eq!(SafetyCheckResult::Safe.error_code(), None);
        assert_eq!(
            SafetyCheckResult::InProtectedPath {
                reason: "system path".to_string()
            }
            .error_code(),
            Some(ErrorCode::Protected)
        );
        assert_eq!(
            SafetyCheckResult::ContainsExecutables {
                files: vec!["a.exe".to_string()]
            }
            .error_code(),
            Some(ErrorCode::Protected)
        );
    }

    #[test]
    fn user_whitelist_blocks_permanent_deletion_before_other_checks() {
        let protected_path = Path::new(r"C:\Fixture\App");
        let engine = PermanentDeleteEngine {
            enable_reboot_fallback: true,
            user_whitelist: Ok(vec![LeftoverWhitelistEntry {
                path: protected_path.to_string_lossy().to_string(),
                added_at: "2026-01-01T00:00:00Z".to_string(),
            }]),
        };

        let result = engine.perform_safety_checks(Path::new(r"c:\fixture\app\cache"));
        assert!(matches!(result, SafetyCheckResult::InProtectedPath { .. }));
    }

    /// "Force Delete" only relaxes the "contains executable files" check:
    /// directories with an exe can pass, but system directories are still blocked — that is the safety boundary of that button.
    #[test]
    fn force_delete_only_skips_the_executable_check() {
        let root = std::env::temp_dir().join(format!("lightc-force-delete-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let app_dir = root.join("OldUpdater");
        fs::create_dir_all(&app_dir).expect("创建测试目录失败");
        fs::write(app_dir.join("installer.exe"), vec![0u8; 1024]).expect("写入可执行文件失败");

        let engine = PermanentDeleteEngine::new();

        // Default: contains executable files → requires manual review
        assert!(matches!(
            engine.perform_safety_checks(&app_dir),
            SafetyCheckResult::ContainsExecutables { .. }
        ));

        // After allowing: this check no longer blocks (whitelist and system-directory checks are still verified below)
        assert!(matches!(
            engine.perform_safety_checks_with(&app_dir, true),
            SafetyCheckResult::Safe
        ));

        // Allowing executables ≠ allowing the whitelist: the user whitelist still takes priority and blocks.
        // (The system-directory check is covered separately by test_protected_path_check, not repeated here.)
        let guarded = PermanentDeleteEngine {
            enable_reboot_fallback: true,
            user_whitelist: Ok(vec![LeftoverWhitelistEntry {
                path: app_dir.to_string_lossy().to_string(),
                added_at: "2026-01-01T00:00:00Z".to_string(),
            }]),
        };
        assert!(matches!(
            guarded.perform_safety_checks_with(&app_dir, true),
            SafetyCheckResult::InProtectedPath { .. }
        ));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn protected_paths_come_from_the_shared_table() {
        fn refused(path: &str) -> bool {
            ensure_deletable(Path::new(path), DeleteScope::Leftovers).is_err()
        }

        // Paths that should be protected
        assert!(refused(r"C:\Windows\System32\test"));
        assert!(refused(r"C:\Users\test\Desktop\file"));
        assert!(refused(r"C:\"));

        // Paths that should be allowed
        assert!(!refused(r"C:\Users\test\AppData\Local\SomeApp"));
    }

    /// C1 regression: bypass spellings must not slip past the protected-path check.
    #[test]
    fn bypass_spellings_are_blocked() {
        fn refused(path: &str) -> bool {
            ensure_deletable(Path::new(path), DeleteScope::Leftovers).is_err()
        }

        // Forward slashes previously matched no `\`-based prefix at all
        assert!(refused(r"C:/Windows/System32/test"));
        assert!(refused(r"C:/"));

        // Extended-length prefix previously kept `\\?\` in the string
        assert!(refused(r"\\?\C:\Windows\System32\test"));
        assert!(refused(r"\\?\C:\"));

        // Traversal paths are rejected by the guard before any check runs
        let engine = PermanentDeleteEngine::new();
        let result = engine.perform_safety_checks(Path::new(r"C:\Temp\..\Windows\System32\config"));
        assert!(matches!(result, SafetyCheckResult::InProtectedPath { .. }));
    }

    #[test]
    fn test_executable_extensions() {
        assert!(EXECUTABLE_EXTENSIONS.contains(&"exe"));
        assert!(EXECUTABLE_EXTENSIONS.contains(&"dll"));
        assert!(EXECUTABLE_EXTENSIONS.contains(&"sys"));
        assert!(!EXECUTABLE_EXTENSIONS.contains(&"txt"));
    }
}
