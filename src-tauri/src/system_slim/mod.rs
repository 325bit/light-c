// ============================================================================
// System slim module
// Handles hibernation file management, WinSxS component cleanup, and virtual memory migration guidance
// ============================================================================

use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    LazyLock, RwLock,
};
use tauri::{AppHandle, Emitter, Manager, Window};

// ============================================================================
// Data structures
// ============================================================================

/// System slim item state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlimItemStatus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub warning: String,
    pub status_text: String,
    pub enabled: bool,
    pub size: u64,
    pub actionable: bool,
    pub action_text: String,
    /// Whether this item is still being analyzed in the background (currently only the two WinSxS items can be true).
    ///
    /// Note: DISM analysis can easily take tens of seconds to several minutes; it must not block the whole check ——
    /// Mark this item as "analyzing" on the first screen, then fill in the result via the `system-slim:winsxs-ready` event when analysis finishes.
    #[serde(default)]
    pub analyzing: bool,
    /// Reclaimable breakdown of the component store (only the winsxs item carries this).
    ///
    /// Note: `size` only gives a total; from the UI users cannot tell how much of the 6.9 GB is superseded old components
    /// and how much is cached temporary data, so the two DISM-analysis items are exposed separately and rendered on separate lines by the frontend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<WinsxsBreakdown>,
}

/// Items pushed after background WinSxS analysis completes (event name `system-slim:winsxs-ready`).
///
/// Carry only these two items; the frontend just replaces the entries with the same id; the whole `SystemSlimStatus` is not sent,
/// so the backend does not need to re-check hibernation / virtual memory / search index just to emit an event.
#[derive(Debug, Clone, Serialize)]
pub struct WinsxsItemsPayload {
    pub items: Vec<SlimItemStatus>,
}

/// DISM analysis progress (event name `system-slim:winsxs-progress`).
///
/// A separate event and a separate type; do not mix it with the cleanup `system-slim:progress` ——
/// Both share the same item_id; mixing them would cause cross-talk where "cleanup progress is overwritten by analysis progress".
#[derive(Debug, Clone, Serialize)]
pub struct WinsxsAnalyzeProgress {
    pub percent: u8,
    pub elapsed_ms: u64,
}

/// System slim status summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSlimStatus {
    pub is_admin: bool,
    pub items: Vec<SlimItemStatus>,
    pub total_reclaimable: u64,
}

/// Slim operation progress event (event name `system-slim:progress`).
///
/// Note: the hibernation toggle, component cleanup, and baseline compaction can each take tens of seconds to over ten minutes. The frontend uses this to show
/// the stage, percentage, and elapsed time, so users clearly know "it is still running and roughly which step it is at", instead of staring at an unchanging button.
#[derive(Debug, Clone, Serialize)]
pub struct SlimOperationProgress {
    /// Operation item id, corresponding to SlimItemStatus.id; the frontend uses it to locate which card is running.
    pub item_id: String,
    /// Stage identifier: preparing / running / waiting / done / error.
    pub phase: String,
    /// User-facing stage description in Chinese.
    pub message: String,
    /// 0-100; returns 0 when it cannot be estimated, and the frontend shows an indeterminate progress bar.
    pub percent: u8,
    /// running / done / error。
    pub status: String,
    /// Elapsed time (ms), so users can judge whether to keep waiting.
    pub elapsed_ms: u64,
}

/// Push slim progress uniformly; a send failure is only logged and does not affect the operation itself.
fn emit_slim_progress(
    window: &Window,
    item_id: &str,
    phase: &str,
    message: &str,
    percent: u8,
    status: &str,
    started_at: std::time::Instant,
) {
    let progress = SlimOperationProgress {
        item_id: item_id.to_string(),
        phase: phase.to_string(),
        message: message.to_string(),
        percent,
        status: status.to_string(),
        elapsed_ms: started_at.elapsed().as_millis() as u64,
    };
    if let Err(error) = window.emit("system-slim:progress", &progress) {
        warn!("发送系统瘦身进度失败: {}", error);
    }
}

/// Maximum wait time for a single DISM analysis.
///
/// Analysis has been moved to the background with results pushed via events, so this value no longer decides "how long the user waits";
/// it is only used to prevent a stuck DISM from holding `WINSXS_ANALYZE_RUNNING` forever.
///
/// Measured locally (2026-11-10, from `Analyze: begin WinSxS tree walk` to session end in CBS.log)
/// One `analyzecomponentstore` run takes **171 seconds**, nearly 6 times the old 30-second timeout ——
/// That was exactly why the old version "timed out on every check and never got a result". Leave a generous margin of 600 seconds:
/// Killing it would waste three minutes, so it is better to wait a bit longer than give up easily.
const WINSXS_ANALYZE_TIMEOUT_SECS: u64 = 600;
/// The "freshness window" of the in-memory cache: within this period, reuse it directly without rerunning DISM.
const WINSXS_CACHE_TTL_SECS: u64 = 10 * 60;
/// Maximum age for which the disk cache can be used for "initial display".
///
/// On app restart, show the previous result first while recomputing in the background, so users do not have to wait for another DISM run just to glance at component store usage.
const WINSXS_DISK_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct WinsxsAnalyzeResult {
    reclaimable_size: u64,
    cleanup_recommended: bool,
    reclaimable_packages: u32,
    analysis_succeeded: bool,
    /// Size of the "Backup and Disabled Features" item: copies of old components replaced by newer versions and temporarily kept for rollback.
    ///
    /// Note: DISM's reclaimable estimate consists of exactly two items; only after splitting them can the UI answer
    /// "what it actually wants to clean" —— previously only a total was given, and users could not tell how this 6.9 GB was composed.
    #[serde(default)]
    backups_size: u64,
    /// Size of the "Cache and Temporary Data" item: caches and temporary copies in the component store that can be safely deleted.
    #[serde(default)]
    cache_size: u64,
}

/// Breakdown of reclaimable components (the two raw lines from DISM analysis).
///
/// Only provide numbers; the frontend renders the wording per language —— assembling Chinese text on the backend would pollute other-language UIs.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct WinsxsBreakdown {
    /// Superseded but retained copies of old components ("Backup and Disabled Features").
    pub backups: u64,
    /// Cache and temporary data.
    pub cache: u64,
    /// Number of reclaimable component packages reported by DISM.
    pub packages: u32,
}

#[derive(Debug, Clone, Copy)]
struct WinsxsAnalyzeCache {
    result: WinsxsAnalyzeResult,
    cached_at: std::time::Instant,
}

/// Disk cache contents: result + write time (Unix seconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WinsxsDiskCache {
    /// Version of the parsing logic. Increment it when recognition rules change so old caches are automatically invalidated ——
    /// After fixing Chinese-output parsing on 2026-11-10, old "0 reclaimable" conclusions still lay on disk,
    /// and without invalidation users would still see "no cleanup needed" first on restart.
    #[serde(default)]
    schema_version: u32,
    result: WinsxsAnalyzeResult,
    cached_at_unix: u64,
}

/// Current parsing-rule version, corresponding to `WinsxsDiskCache::schema_version`.
const WINSXS_CACHE_SCHEMA: u32 = 3;

static WINSXS_ANALYZE_CACHE: LazyLock<RwLock<Option<WinsxsAnalyzeCache>>> =
    LazyLock::new(|| RwLock::new(None));
static WINSXS_ANALYZE_RUNNING: AtomicBool = AtomicBool::new(false);
/// Start time of this analysis round (Unix seconds), used to detect a stuck state where the lock is held but the work should have finished long ago.
static WINSXS_ANALYZE_STARTED_AT: AtomicU64 = AtomicU64::new(0);

// ============================================================================
// Privilege check
// ============================================================================

/// Check whether the current process **really has administrator capability**.
///
/// ⚠️ Do not switch back to `shell32!IsUserAnAdmin` —— the two have different semantics: it asks "is this account a member of the Administrators group",
/// whereas here we need "can this process read $MFT and change system settings".
/// With UAC enabled, a normally launched process carries a **restricted token**: the account is an administrator but the process has no privileges,
/// so the two functions give opposite answers on such machines → the frontend hint and "whether the backend actually degrades" would disagree,
/// showing up as "features silently degrade to a slow path with no hint in the UI".
///
/// Use `TokenElevation` uniformly (the same check used for AI model deep discovery and big-file/big-directory engine selection),
/// ensuring that "the privilege state shown in the UI" and "the backend's behavior" always come from the same source of truth.
pub fn check_admin() -> bool {
    crate::scanner::big_files_engine::mft_core::is_elevated()
}

// ============================================================================
// Status check (async: avoid DISM blocking the main thread)
// ============================================================================

/// Get the system slim status.
///
/// **WinSxS no longer participates in first-screen waiting.** DISM analysis takes tens of seconds to minutes locally,
/// previously the whole `get_status` had to wait for it (released only after a 30-second timeout), so users saw "checking" spin for half a minute.
/// Now: use the cache if available (even from a previous launch); otherwise mark it as "analyzing" first,
/// and push those two items via the `system-slim:winsxs-ready` event when DISM finishes.
pub async fn get_status(window: Window, force_refresh: bool) -> SystemSlimStatus {
    let is_admin = check_admin();

    let winsxs_items = prepare_winsxs_items(&window, is_admin, force_refresh);

    // The other three items are millisecond-level (registry read + metadata); just run them concurrently.
    let (hibernation, pagefile, search_index) = tokio::join!(
        async { get_hibernation_status() },
        async { get_pagefile_status() },
        async { get_search_index_status() },
    );

    let mut items = vec![hibernation];
    items.extend(winsxs_items);
    items.push(pagefile);
    items.push(search_index);
    let total_reclaimable = items.iter().filter(|i| i.enabled).map(|i| i.size).sum();

    SystemSlimStatus {
        is_admin,
        items,
        total_reclaimable,
    }
}

/// Assemble the two WinSxS items and start background analysis when needed.
///
/// Data source priority: in-memory cache (within 10 min) → disk cache (within 24 h, across restarts) → none at all.
/// The latter two also start a background analysis, whose result overwrites the cached one when it arrives.
fn prepare_winsxs_items(
    window: &Window,
    is_admin: bool,
    force_refresh: bool,
) -> Vec<SlimItemStatus> {
    // When the user manually clicks "recheck", we must really rerun it; fobbing them off with a cache under 10 minutes old is not allowed ——
    // The old behavior of "click does nothing, the UI just looks refreshed" was caused by a direct cache hit.
    // During refresh, hold the existing conclusion as a placeholder (mark as analyzing) and overwrite when the new result arrives.
    if force_refresh && is_admin {
        let fallback = get_any_cached_winsxs_result().or_else(load_disk_winsxs_cache);
        start_winsxs_analysis(window.clone());

        return match fallback {
            Some(result) => build_winsxs_items(result, true, None),
            None => build_winsxs_items(WinsxsAnalyzeResult::default(), true, None),
        };
    }

    if let Some(result) = get_cached_winsxs_result() {
        return build_winsxs_items(result, false, None);
    }

    let stale = load_disk_winsxs_cache();

    // Without admin privileges DISM is bound to fail (error code 740); there is no need to start a process just to wait for the error.
    // But conclusions on disk from within 24 hours are still worth showing —— better than a bare "cannot read".
    if !is_admin {
        return match stale {
            Some(result) => build_winsxs_items(result, false, None),
            None => build_winsxs_items(WinsxsAnalyzeResult::default(), false, None),
        };
    }

    start_winsxs_analysis(window.clone());

    match stale {
        Some(result) => build_winsxs_items(result, true, None),
        None => build_winsxs_items(WinsxsAnalyzeResult::default(), true, None),
    }
}

/// Start a DISM analysis in the background and return immediately.
///
/// Returns `true` to mean "an analysis is indeed running now" (started this time or earlier), which the frontend uses to show the analyzing state.
fn start_winsxs_analysis(window: Window) -> bool {
    #[cfg(target_os = "windows")]
    {
        // Cleanup is exclusively using the component store; starting analyze now would only fight it for the CBS session
        if WINSXS_CLEANUP_ACTIVE.load(Ordering::SeqCst) {
            return false;
        }

        if WINSXS_ANALYZE_RUNNING.swap(true, Ordering::SeqCst) {
            let now = unix_now();
            let started = WINSXS_ANALYZE_STARTED_AT.load(Ordering::Relaxed);
            let age = now.saturating_sub(started);

            // Normal case: an analysis is indeed running (including the one just started); do not launch another.
            // Abnormal case: the thread hung or exited abnormally without unlocking, so the lock would never be released ——
            // showing up as "no response no matter how many times recheck is clicked". Past the fallback timeout, treat it as stuck and force a restart.
            if started == 0 || age <= WINSXS_ANALYZE_TIMEOUT_SECS + 60 {
                return true;
            }

            warn!(
                "上次 WinSxS 分析已持续 {} 秒仍未结束，判定为僵死并重新发起",
                age
            );
        }

        WINSXS_ANALYZE_STARTED_AT.store(unix_now(), Ordering::Relaxed);

        // Use spawn_blocking rather than spawn: DISM is a blocking process call and must not occupy an async thread.
        // Do not await here —— the result is pushed later via an event, and the scan endpoint returns early.
        // Progress must be pushed to the frontend, so carry the handle into the background thread as well
        let progress_handle = window.app_handle().clone();

        tokio::task::spawn_blocking(move || {
            let outcome = analyze_winsxs_sync(Some(progress_handle));
            // Unlock before emitting the event: the analysis itself has ended, so later checks can reuse the cache immediately
            WINSXS_ANALYZE_RUNNING.store(false, Ordering::SeqCst);

            let items = match outcome {
                Ok(result) => {
                    set_cached_winsxs_result(result);
                    build_winsxs_items(result, false, None)
                }
                Err(error) => {
                    warn!("DISM 分析失败，本次不更新组件存储占用: {}", error);
                    // Push even on failure: only when the frontend receives the event will it stop the "analyzing" spinner,
                    // otherwise it keeps spinning and users think it is still running. Include the cause so users do not have to guess.
                    build_winsxs_items(WinsxsAnalyzeResult::default(), false, Some(&error))
                }
            };

            let payload = WinsxsItemsPayload { items };
            if let Err(error) = window.emit("system-slim:winsxs-ready", &payload) {
                warn!("推送 WinSxS 分析结果失败: {}", error);
            }
        });

        true
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;
        false
    }
}

/// Get hibernation file status (detected via the registry, not dependent on powercfg encoding)
fn get_hibernation_status() -> SlimItemStatus {
    let hibernation_enabled = check_hibernation_enabled();
    let hiberfil_path = std::path::Path::new("C:\\hiberfil.sys");
    let size = if hiberfil_path.exists() {
        std::fs::metadata(hiberfil_path)
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };

    SlimItemStatus {
        id: "hibernation".to_string(),
        name: "休眠文件".to_string(),
        description: "Windows 休眠功能会在 C 盘创建与内存大小相当的 hiberfil.sys 文件".to_string(),
        warning: "关闭休眠将导致快速启动功能失效，电脑无法进入休眠状态".to_string(),
        status_text: if hibernation_enabled {
            "当前已启用休眠和快速启动相关能力".to_string()
        } else {
            "当前已关闭休眠，hiberfil.sys 不再占用系统盘空间".to_string()
        },
        enabled: hibernation_enabled,
        size,
        actionable: true,
        action_text: if hibernation_enabled {
            "关闭休眠".to_string()
        } else {
            "开启休眠".to_string()
        },
        analyzing: false,
        breakdown: None,
    }
}

/// Detect whether hibernation is enabled via the registry (fast, encoding-independent, no powercfg dependency)
fn check_hibernation_enabled() -> bool {
    #[cfg(target_os = "windows")]
    {
        use winreg::{enums::*, RegKey};

        // Primary check: the registry HibernateEnabled value (authoritative source for powercfg /hibernate).
        // After a successful read, return immediately; otherwise a stale hiberfil.sys could make "disabled" be misjudged as enabled again.
        if let Ok(hklm) = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey_with_flags(r"SYSTEM\CurrentControlSet\Control\Power", KEY_READ)
        {
            if let Ok(value) = hklm.get_value::<u32, _>("HibernateEnabled") {
                return value == 1;
            }
        }

        // Fallback: check whether hiberfil.sys exists
        std::path::Path::new("C:\\hiberfil.sys").exists()
    }

    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// Convert DISM analysis results into two UI entries.
///
/// `analyzing` being true means background analysis is still running: if a previous conclusion exists, display it first,
/// with an "analyzing" spinner added by the frontend; if there is nothing at all, say plainly that analysis is in progress rather than leaving users staring at a blank.
/// Truncate the failure reason: DISM output may include a whole usage block; keep only the first meaningful line in the UI.
fn truncate_reason(reason: &str) -> String {
    const MAX_CHARS: usize = 120;

    // Prefer the first non-empty line; DISM's "Error: 740 ..." is on the first line
    let first_line = reason
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(reason);

    if first_line.chars().count() <= MAX_CHARS {
        return first_line.to_string();
    }

    let head: String = first_line.chars().take(MAX_CHARS).collect();
    format!("{}…", head)
}

fn build_winsxs_items(
    analyze_result: WinsxsAnalyzeResult,
    analyzing: bool,
    failure: Option<&str>,
) -> Vec<SlimItemStatus> {
    let can_estimate = analyze_result.analysis_succeeded;
    let has_reclaimable = analyze_result.reclaimable_size > 0 || analyze_result.cleanup_recommended;

    let status_text = if !can_estimate {
        if analyzing {
            "首次分析通常需要 2-3 分钟，这段时间可以先处理其他项目".to_string()
        } else {
            // Surface DISM's own failure reason. Saying only "analysis failed" leaves users and developers guessing ——
            // Common causes are "another operation is in progress" (Windows Update holding the component store) or insufficient privileges.
            match failure.map(str::trim).filter(|reason| !reason.is_empty()) {
                Some(reason) => format!("未能读取组件存储信息：{}", truncate_reason(reason)),
                None => "未能读取组件存储信息（通常是没有管理员权限，或 DISM 分析失败），以管理员身份重新检测可获取准确占用".to_string(),
            }
        }
    } else if has_reclaimable {
        format!(
            "DISM 建议清理，发现 {} 个可回收组件包",
            analyze_result.reclaimable_packages
        )
    } else {
        "DISM 未发现明确可回收的组件包，当前无需执行组件清理".to_string()
    };
    // Not clickable during analysis: no conclusion yet, a click would only be rejected by the backend (or trigger a duplicate cleanup).
    let winsxs_actionable = !analyzing && (!can_estimate || has_reclaimable);

    // Only show details when sub-items are actually parsed: some DISM versions report only a total, and hard-splitting a zero out of it
    // would make the UI show "backup 0 B, cache = everything", which is more misleading than showing nothing.
    let breakdown =
        if can_estimate && (analyze_result.backups_size > 0 || analyze_result.cache_size > 0) {
            Some(WinsxsBreakdown {
                backups: analyze_result.backups_size,
                cache: analyze_result.cache_size,
                packages: analyze_result.reclaimable_packages,
            })
        } else {
            None
        };

    vec![
        SlimItemStatus {
            id: "winsxs".to_string(),
            name: "系统组件存储".to_string(),
            // This wording must withstand the question "what exactly gets deleted": state which official command is run,
            // which two kinds of data are affected, and what is explicitly untouched. The AI assistant also reads this.
            description: "调用 Windows 官方的 StartComponentCleanup：删除 WinSxS 中被新版本取代的旧组件副本，以及组件存储的缓存与临时数据；正在使用的组件、已安装的程序和更新本身都不受影响"
                .to_string(),
            warning: "清理会删除被取代的旧组件副本，因此清理后通常无法回滚到这些版本；已安装的 Windows 更新不会被卸载"
                .to_string(),
            status_text,
            enabled: has_reclaimable,
            size: analyze_result.reclaimable_size,
            // Disable the button when the check clearly finds nothing reclaimable, to avoid the illusion of "still cleanable" from users repeatedly running idempotent DISM cleanup.
            actionable: winsxs_actionable,
            action_text: if analyzing {
                "分析中…".to_string()
            } else if has_reclaimable {
                "开始清理".to_string()
            } else if !can_estimate {
                "执行清理".to_string()
            } else {
                "无需清理".to_string()
            },
            analyzing,
            breakdown,
        },
        SlimItemStatus {
            id: "winsxs_resetbase".to_string(),
            name: "深度清理（ResetBase）".to_string(),
            description:
                "在普通清理的基础上再执行 ResetBase：把所有被取代的旧组件版本连同回滚基线一起删除，包括普通清理为了能回滚而保留的那部分"
                    .to_string(),
            warning:
                "会一并删除系统更新的回滚版本：执行后，当前已安装的 Windows 更新无法卸载"
                    .to_string(),
            status_text:
                "比普通清理更彻底：普通清理只删可以安全移除的旧版本，深度清理连回滚版本一起删"
                    .to_string(),
            enabled: false,
            size: 0,
            actionable: winsxs_actionable,
            action_text: if analyzing {
                "分析中…".to_string()
            } else if winsxs_actionable {
                "深度清理".to_string()
            } else {
                "无需清理".to_string()
            },
            analyzing,
            breakdown: None,
        },
    ]
}

/// Get the in-memory conclusion, **ignoring TTL**: used as a placeholder during forced refresh so numbers do not disappear and then reappear.
fn get_any_cached_winsxs_result() -> Option<WinsxsAnalyzeResult> {
    WINSXS_ANALYZE_CACHE
        .read()
        .ok()?
        .as_ref()
        .map(|cache| cache.result)
}

fn get_cached_winsxs_result() -> Option<WinsxsAnalyzeResult> {
    let cache = WINSXS_ANALYZE_CACHE.read().ok()?;
    let cached = cache.as_ref()?;
    // The WinSxS reclaimable size does not change frequently in a short period; caching avoids repeatedly hanging on DISM analysis when the page is reopened.
    if cached.cached_at.elapsed().as_secs() <= WINSXS_CACHE_TTL_SECS {
        return Some(cached.result);
    }
    None
}

/// Record an analysis result: write to both memory and disk, so the next launch can use the disk cache as a placeholder.
///
/// Only reject results of "analysis failed". **"Nothing reclaimable" is also cached** —— that is the most common case,
/// and not caching it before meant rerunning DISM (tens of seconds) every time the check page was opened,
/// while cleanup/compaction actions themselves call `clear_cached_winsxs_size()` to invalidate it proactively, so "still showing 0 after cleanup" cannot happen.
fn set_cached_winsxs_result(result: WinsxsAnalyzeResult) {
    if !result.analysis_succeeded {
        return;
    }

    store_memory_winsxs_result(result);
    save_disk_winsxs_cache(result);
}

/// Write only the in-memory cache (not disk); used by unit tests to avoid writing files into the real data directory.
fn store_memory_winsxs_result(result: WinsxsAnalyzeResult) {
    if let Ok(mut cache) = WINSXS_ANALYZE_CACHE.write() {
        *cache = Some(WinsxsAnalyzeCache {
            result,
            cached_at: std::time::Instant::now(),
        });
    }
}

fn clear_cached_winsxs_size() {
    if let Ok(mut cache) = WINSXS_ANALYZE_CACHE.write() {
        // Cleanup changes the component store state; the next check must re-analyze rather than keep showing the old estimate.
        *cache = None;
    }
    // Invalidate the disk cache as well: otherwise users would read the pre-cleanup size after restarting.
    let path = winsxs_disk_cache_path();
    if path.exists() {
        if let Err(error) = std::fs::remove_file(&path) {
            warn!("删除 WinSxS 磁盘缓存失败: {}", error);
        }
    }
}

/// Disk cache file: follows the data directory, so portable builds and custom data directories both resolve correctly.
fn winsxs_disk_cache_path() -> std::path::PathBuf {
    crate::data_dir::get_data_dir().join("system_slim_winsxs.json")
}

fn save_disk_winsxs_cache(result: WinsxsAnalyzeResult) {
    let Some(json) = serialize_disk_cache(result, unix_now()) else {
        return;
    };
    let path = winsxs_disk_cache_path();

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Failure to write the cache only affects the next launch's result, not this one, so do not propagate the error.
    if let Err(error) = std::fs::write(&path, json) {
        warn!("写入 WinSxS 磁盘缓存失败: {}", error);
    }
}

/// Serialize the disk cache. Splitting it into a pure function lets us directly test "whether a value round-trips through save and read".
fn serialize_disk_cache(result: WinsxsAnalyzeResult, now_unix: u64) -> Option<String> {
    serde_json::to_string(&WinsxsDiskCache {
        schema_version: WINSXS_CACHE_SCHEMA,
        result,
        cached_at_unix: now_unix,
    })
    .ok()
}

/// Parse the disk cache: treat failed and expired results as absent.
fn parse_disk_cache(text: &str, now_unix: u64) -> Option<WinsxsAnalyzeResult> {
    let cache: WinsxsDiskCache = serde_json::from_str(text).ok()?;
    if cache.schema_version != WINSXS_CACHE_SCHEMA {
        return None;
    }
    if !cache.result.analysis_succeeded {
        return None;
    }
    let age = now_unix.saturating_sub(cache.cached_at_unix);
    if age > WINSXS_DISK_CACHE_TTL_SECS {
        return None;
    }
    Some(cache.result)
}

/// Read the disk cache. Only used for "showing the previous result first", so it is allowed to be much older than the in-memory cache.
fn load_disk_winsxs_cache() -> Option<WinsxsAnalyzeResult> {
    let text = std::fs::read_to_string(winsxs_disk_cache_path()).ok()?;
    parse_disk_cache(&text, unix_now())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Run dism /analyzecomponentstore synchronously (called from spawn_blocking).
///
/// DISM prints percentages as it runs; forward them to the frontend: analysis takes tens of seconds to minutes,
/// and with only a spinner in the UI users would think it is stuck.
fn analyze_winsxs_sync(handle: Option<AppHandle>) -> Result<WinsxsAnalyzeResult, String> {
    #[cfg(target_os = "windows")]
    {
        let (stdout_bytes, stderr_bytes, success) = run_dism_analyze(handle.as_ref())?;
        let stdout = decode_command_output(&stdout_bytes);
        let stderr = decode_command_output(&stderr_bytes);

        if !success {
            // DISM errors (e.g. "Error: 740 Elevated permissions are required") are written to stdout;
            // looking only at stderr yields an empty message —— the previous version got stuck debugging exactly here.
            let detail = if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            };
            return Err(format!("DISM 分析失败: {}", detail));
        }
        Ok(parse_winsxs_analyze_output(&stdout))
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = handle;
        Ok(WinsxsAnalyzeResult::default())
    }
}

/// Run DISM analysis: forward progress while also enforcing a timeout fallback.
///
/// Read stdout and stderr each in its own thread while the main thread polls `try_wait`:
/// if the main thread also reads the pipes it cannot judge the timeout; if it only polls without reading, once output exceeds the pipe buffer (64 KB)
/// the child process blocks on writing and the two sides deadlock. DISM analysis output is small, so two reader threads are the safest approach.
#[cfg(target_os = "windows")]
/// Read child-process output, forwarding DISM's percentage progress as it goes.
///
/// **Must read by bytes and split lines on `\n` ourselves**: `BufRead::lines()` requires every line to be valid UTF-8,
/// and returns `Err` on non-UTF-8 bytes (on Chinese systems DISM outputs GBK),
/// while `map_while(Result::ok)` would **silently terminate the whole read** —— showing up as "analysis succeeded, 0 reclaimable".
/// The 2026-11-10 case where "the CBS log clearly said Superseded Packages: 2, Cleanup recommended: 1,
/// yet the UI showed no cleanup needed" was exactly this chain breaking on encoding.
fn read_stream_with<F>(mut reader: impl std::io::Read, mut on_line: F) -> Vec<u8>
where
    F: FnMut(&str),
{
    let mut raw = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 8192];

    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };

        raw.extend_from_slice(&buffer[..read]);
        pending.extend_from_slice(&buffer[..read]);

        // Split lines on \n or \r ourselves; do not require the whole line to be valid UTF-8.
        //
        // ⚠️ Must recognize \r: DISM redraws its progress bar repeatedly on the same line using \r
        // (`[====10.0%====]` → `[=====11.0%=====]`), with only a single \n for the whole segment.
        // Splitting only on \n means a progress update is emitted only when the step ends —— the UI would show
        // the percentage frozen at 10% and "elapsed 0s" never refreshing, making users think it is stuck. Measured 2026-11-17.
        while let Some(pos) = pending
            .iter()
            .position(|&byte| byte == b'\n' || byte == b'\r')
        {
            let line_bytes: Vec<u8> = pending.drain(..=pos).collect();
            on_line(&String::from_utf8_lossy(&line_bytes));
        }
    }

    raw
}

/// Analysis-specific stream reader: forward DISM's percentage as `system-slim:winsxs-progress`.
fn read_stream(
    reader: impl std::io::Read,
    handle: Option<AppHandle>,
    started_at: std::time::Instant,
) -> Vec<u8> {
    let mut last_percent = 0u32;

    read_stream_with(reader, move |line| {
        if let Some(percent) = parse_dism_progress(line) {
            if percent > last_percent {
                last_percent = percent;
                if let Some(handle) = &handle {
                    // A send failure (e.g. window already closed) does not affect the analysis itself
                    let _ = handle.emit(
                        "system-slim:winsxs-progress",
                        &WinsxsAnalyzeProgress {
                            percent: percent.min(100) as u8,
                            elapsed_ms: started_at.elapsed().as_millis() as u64,
                        },
                    );
                }
            }
        }
    })
}

fn run_dism_analyze(handle: Option<&AppHandle>) -> Result<(Vec<u8>, Vec<u8>, bool), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    let started_at = std::time::Instant::now();

    let mut child = Command::new("cmd")
        .args([
            "/C",
            // `/english` makes DISM output in English: the parser only recognizes one set of wording,
            // so there is no need to maintain a keyword table for every system language (the Chinese "component store cleanup is recommended" does not match
            // "cleanup recommended", which has caused missed detections in production).
            "dism.exe /english /online /cleanup-image /analyzecomponentstore",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| format!("执行 DISM 失败: {}", e))?;

    // Record the pid: it must be terminated when cleanup starts (two DISM processes cannot operate on the component store at once)
    WINSXS_ANALYZE_PID.store(child.id(), Ordering::SeqCst);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取 DISM 输出".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取 DISM 错误输出".to_string())?;

    let progress_handle = handle.cloned();
    let stdout_thread =
        std::thread::spawn(move || read_stream(stdout, progress_handle, started_at));
    let stderr_thread = std::thread::spawn(move || read_stream(stderr, None, started_at));

    let deadline = started_at + std::time::Duration::from_secs(WINSXS_ANALYZE_TIMEOUT_SECS);
    let mut timed_out = false;

    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(error) => {
                // H2: killing only cmd.exe would leave dism.exe holding the stdout pipe,
                // so the joins below would hang; reap the whole tree instead.
                kill_process_tree(child.id());
                return Err(format!("等待 DISM 结束失败: {}", error));
            }
        }

        if std::time::Instant::now() >= deadline {
            // analyzecomponentstore is read-only, so killing it leaves no half-finished component store.
            // H2: Child::kill() only terminates the cmd.exe shell — the surviving dism.exe keeps the
            // stdout pipe open and stdout_thread.join() below would block forever. Kill the whole tree.
            kill_process_tree(child.id());
            timed_out = true;
            break;
        }

        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    let status = child
        .wait()
        .map_err(|e| format!("回收 DISM 进程失败: {}", e))?;
    WINSXS_ANALYZE_PID.store(0, Ordering::SeqCst);

    let stdout_text = stdout_thread.join().unwrap_or_default();
    let stderr_text = stderr_thread.join().unwrap_or_default();

    if timed_out {
        return Err(format!(
            "DISM 分析超过 {} 秒仍未返回",
            WINSXS_ANALYZE_TIMEOUT_SECS
        ));
    }

    Ok((stdout_text, stderr_text, status.success()))
}

fn parse_winsxs_analyze_output(output: &str) -> WinsxsAnalyzeResult {
    let mut result = WinsxsAnalyzeResult {
        analysis_succeeded: true,
        ..WinsxsAnalyzeResult::default()
    };
    // At least one key conclusion line (reclaimable package count / whether cleanup is recommended) must be recognized, otherwise parsing has failed.
    // Without this self-check, empty output would be treated as "success with 0 reclaimable" and the UI would show "no cleanup needed" —— exactly the 2026-11-10 pitfall.
    let mut saw_verdict = false;

    for line in output.lines() {
        // Unicode on purpose (L4): DISM output text (localized), not a path.
        let normalized = line.trim().to_lowercase();

        if is_winsxs_reclaimable_size_line(&normalized) {
            if let Some(size_bytes) = parse_size_from_line(&normalized) {
                result.reclaimable_size = result.reclaimable_size.saturating_add(size_bytes);
                // Also record it on the corresponding sub-item: these two lines are the only reclaimable breakdown DISM provides,
                // and the UI relies on them to explain "how this total was derived".
                if is_winsxs_backups_line(&normalized) {
                    result.backups_size = result.backups_size.saturating_add(size_bytes);
                } else if is_winsxs_cache_line(&normalized) {
                    result.cache_size = result.cache_size.saturating_add(size_bytes);
                }
            }
        }

        if normalized.contains("number of reclaimable packages")
            || normalized.contains("reclaimable packages")
            || normalized.contains("可回收的程序包")
            || normalized.contains("可回收程序包")
            || normalized.contains("可回收的组件包")
        {
            saw_verdict = true;
            if let Some(count) = parse_first_integer(&normalized) {
                result.reclaimable_packages = count;
            }
        }

        if normalized.contains("cleanup recommended")
            || normalized.contains("建议清理")
            || normalized.contains("推荐清理")
            // The real Chinese DISM wording is "component store cleanup is recommended", which does not contain the contiguous characters meaning "cleanup recommended",
            // so literal matching would miss it —— we must allow the form with words inserted in between.
            || (normalized.contains("建议") && normalized.contains("清理"))
        {
            saw_verdict = true;
            if let Some(flag) = parse_yes_no(&normalized) {
                result.cleanup_recommended = flag;
            }
        }
    }

    if !saw_verdict {
        result.analysis_succeeded = false;
    }

    result
}

/// Extract a value from "Label : Yes/No" in either Chinese or English.
///
/// Do not judge with `contains("yes")` —— in wording like "whether cleanup is recommended" that character is always present, so the conclusion would always be "cleanup recommended".
fn parse_yes_no(line: &str) -> Option<bool> {
    let value = line.rsplit(|c: char| c == ':' || c == '：').next()?.trim();

    if value.starts_with('是') || value.starts_with("yes") || value.starts_with("true") {
        return Some(true);
    }
    if value.starts_with('否') || value.starts_with("no") || value.starts_with("false") {
        return Some(false);
    }

    None
}

fn is_winsxs_reclaimable_size_line(line: &str) -> bool {
    // DISM's reclaimable estimate consists of "Backup and Disabled Features" and "Cache and Temporary Data";
    // Do not use the total WinSxS size, to avoid miscounting component-store hard-link size as freeable space.
    line.contains("backups and disabled features")
        || line.contains("cache and temporary data")
        || line.contains("备份")
        || line.contains("已禁用")
        || line.contains("缓存和临时")
}

/// "Backup and Disabled Features" line: superseded old component copies temporarily kept for rollback.
fn is_winsxs_backups_line(line: &str) -> bool {
    line.contains("backups and disabled features")
        || line.contains("备份")
        || line.contains("已禁用")
}

/// "Cache and Temporary Data" line: caches and temporary copies in the component store that can be safely deleted.
fn is_winsxs_cache_line(line: &str) -> bool {
    line.contains("cache and temporary data")
        || line.contains("缓存和临时")
        || line.contains("缓存与临时")
}

/// Parse a byte count from a size string, supporting formats like "3.50 GB", "500 MB", "12 KB"
fn parse_size_from_line(line: &str) -> Option<u64> {
    // DISM output text (localized unit strings), not a path (L4).
    let line = line.to_lowercase();
    let mut value: Option<f64> = None;
    let mut unit_multiplier = 1024.0 * 1024.0;

    let bytes_line = line.as_bytes();
    for (i, &b) in bytes_line.iter().enumerate() {
        if b.is_ascii_digit() || b == b'.' {
            // Extract the number from the current position
            let start = i;
            let mut end = i;
            while end < bytes_line.len()
                && (bytes_line[end].is_ascii_digit() || bytes_line[end] == b'.')
            {
                end += 1;
            }
            if let Ok(v) = std::str::from_utf8(&bytes_line[start..end])
                .unwrap_or("0")
                .parse::<f64>()
            {
                value = Some(v);
                // Check whether GB or MB follows this number
                let rest = &line[end..];
                if rest.trim_start().starts_with("gb") {
                    unit_multiplier = 1024.0 * 1024.0 * 1024.0;
                    break;
                } else if rest.trim_start().starts_with("mb") {
                    unit_multiplier = 1024.0 * 1024.0;
                    break;
                } else if rest.trim_start().starts_with("kb") {
                    unit_multiplier = 1024.0;
                    break;
                }
            }
        }
    }

    value.map(|v| (v * unit_multiplier) as u64)
}

fn parse_first_integer(line: &str) -> Option<u32> {
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b.is_ascii_digit() {
            let mut end = i;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            return std::str::from_utf8(&bytes[i..end])
                .ok()?
                .parse::<u32>()
                .ok();
        }
    }
    None
}

/// Get virtual memory status (supports multi-disk pagefile detection)
fn get_pagefile_status() -> SlimItemStatus {
    let pagefile_configs = get_pagefile_configs();

    // Count pagefile sizes on drive C
    let mut c_drive_size: u64 = 0;
    let mut other_drive_size: u64 = 0;
    let mut c_drive_paths: Vec<String> = Vec::new();
    let mut other_drive_paths: Vec<String> = Vec::new();

    for cfg in &pagefile_configs {
        let path_lower = cfg.path.to_ascii_lowercase();
        let actual_size = std::fs::metadata(&cfg.path)
            .map(|m| m.len())
            .unwrap_or(cfg.max_size);

        if path_lower.starts_with("c:") {
            c_drive_size += actual_size;
            c_drive_paths.push(format!("{} ({})", cfg.path, format_bytes(actual_size)));
        } else {
            other_drive_size += actual_size;
            other_drive_paths.push(format!("{} ({})", cfg.path, format_bytes(actual_size)));
        }
    }

    let is_on_c_drive = c_drive_size > 0;
    let total_size = if is_on_c_drive {
        c_drive_size
    } else {
        other_drive_size
    };

    // Build the description text
    let description = if pagefile_configs.is_empty()
        || pagefile_configs.len() == 1
            && pagefile_configs
                .first()
                .map_or(false, |c| c.path.to_ascii_lowercase().starts_with("c:"))
        || pagefile_configs.is_empty()
    {
        let loc_text = if pagefile_configs.is_empty() {
            "系统管理".to_string()
        } else {
            c_drive_paths
                .first()
                .cloned()
                .unwrap_or("系统管理".to_string())
        };
        format!(
            "当前分页文件: {}。建议将虚拟内存迁移到非系统盘以释放 C 盘空间",
            loc_text
        )
    } else {
        let all_paths: Vec<String> = c_drive_paths
            .iter()
            .chain(other_drive_paths.iter())
            .cloned()
            .collect();
        format!(
            "分页文件分布在 {} 个磁盘: {}。",
            if is_on_c_drive && other_drive_size > 0 {
                "多"
            } else {
                "1"
            },
            all_paths.join(", ")
        )
    };

    SlimItemStatus {
        id: "pagefile".to_string(),
        name: "虚拟内存".to_string(),
        description,
        warning: "虚拟内存对系统稳定性至关重要，不建议直接删除，请通过系统设置迁移到其他磁盘"
            .to_string(),
        status_text: if is_on_c_drive {
            "检测到 C 盘分页文件，可按需迁移到其他磁盘".to_string()
        } else if pagefile_configs.is_empty() {
            "当前由系统自动管理，未读取到固定分页文件配置".to_string()
        } else {
            "C 盘未检测到分页文件，无需迁移".to_string()
        },
        enabled: is_on_c_drive,
        size: total_size,
        actionable: is_on_c_drive,
        action_text: if is_on_c_drive {
            "打开系统设置".to_string()
        } else {
            "无需操作".to_string()
        },
        analyzing: false,
        breakdown: None,
    }
}

/// Pagefile configuration entry
struct PagefileConfig {
    path: String,
    #[allow(dead_code)]
    initial_size: u64,
    max_size: u64,
}

/// Read pagefile configuration from the registry (supports multiple disks)
///
/// Read `PagingFiles` (REG_MULTI_SZ) directly with winreg, no longer spawning a `reg.exe` child process:
/// Saves one process launch (measured 100-300 ms, longer with antivirus) and avoids the "decode reg's console output as
/// UTF-8" pitfall —— under a non-UTF-8 code page, Chinese paths become garbled.
fn get_pagefile_configs() -> Vec<PagefileConfig> {
    #[cfg(target_os = "windows")]
    {
        use winreg::{enums::*, RegKey};

        let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey_with_flags(
            r"System\CurrentControlSet\Control\Session Manager\Memory Management",
            KEY_READ,
        ) else {
            return vec![];
        };

        let Ok(raw) = key.get_raw_value("PagingFiles") else {
            return vec![];
        };

        parse_pagefile_entries(&decode_reg_multi_sz(&raw.bytes))
    }

    #[cfg(not(target_os = "windows"))]
    {
        vec![]
    }
}

/// Decode REG_MULTI_SZ: UTF-16LE strings separated by a single `\0`, ending with a double `\0`.
fn decode_reg_multi_sz(bytes: &[u8]) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();

    for pair in bytes.chunks_exact(2) {
        let unit = u16::from_le_bytes([pair[0], pair[1]]);
        if unit == 0 {
            // Two consecutive zeros end the list; a single zero is just a separator.
            if current.is_empty() {
                break;
            }
            items.push(std::mem::take(&mut current));
        } else {
            // Paths never contain surrogate pairs; treat any surrogate as a replacement character so the whole entry is not lost.
            current.push(char::from_u32(unit as u32).unwrap_or('\u{FFFD}'));
        }
    }

    if !current.is_empty() {
        items.push(current);
    }

    items
}

/// Parse a single pagefile config entry: `C:\pagefile.sys 2048 4096` (the last two numbers may be omitted).
///
/// A path with no numbers means "system-managed size"; in that case leave the size at 0 and let `metadata` read the actual file.
fn parse_pagefile_entries(entries: &[String]) -> Vec<PagefileConfig> {
    entries
        .iter()
        .filter_map(|entry| {
            let tokens: Vec<&str> = entry.split_whitespace().collect();
            let path = tokens.first()?.trim_end_matches(',');

            // Only accept absolute paths with a drive letter; skip empty values and placeholders like "*".
            if path.len() < 3 || &path.as_bytes()[1..3] != b":\\" {
                return None;
            }

            let size_mb = |index: usize| -> u64 {
                tokens
                    .get(index)
                    .map(|token| token.trim_end_matches(','))
                    .and_then(|token| token.parse::<u64>().ok())
                    .unwrap_or(0)
            };

            Some(PagefileConfig {
                path: path.to_string(),
                initial_size: size_mb(1) * 1024 * 1024, // MB → bytes
                max_size: size_mb(2) * 1024 * 1024,
            })
        })
        .collect()
}

/// Format a byte count as a human-readable string
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes / 1024)
    }
}

#[cfg(target_os = "windows")]
fn run_powercfg_hibernation_command(mode: &str) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // Pass hibernation config directly to powercfg.exe; going through cmd risks command parsing or code-page changes affecting the state change.
    Command::new("powercfg.exe")
        .args(["/hibernate", mode])
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| format!("执行 powercfg 失败: {}", e))
}

#[cfg(target_os = "windows")]
fn wait_for_hibernation_disabled() -> bool {
    let hiberfil_path = std::path::Path::new("C:\\hiberfil.sys");

    // powercfg may need a moment to delete the system file; polling avoids false reports from rechecking immediately after success.
    for _ in 0..20 {
        if !check_hibernation_enabled() && !hiberfil_path.exists() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    false
}

#[cfg(target_os = "windows")]
fn decode_command_output(raw: &[u8]) -> String {
    // DISM output is decoded as UTF-8 (the command is preceded by chcp 65001);
    // keep a lossy fallback here so unusual bytes do not cause the error message to be lost.
    String::from_utf8_lossy(raw).into_owned()
}

/// ID of the currently running DISM process.
///
/// Note: DISM is a separate process; if the app is force-closed it keeps running in the background, with no progress visible to users
/// and no indication that the system is still being modified. Record the pid here and terminate it before the next launch or a new cleanup,
/// to avoid leaving behind an "invisible component cleanup".
static RUNNING_DISM_PROCESS_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Exclusive marker during cleanup.
///
/// DISM's `analyzecomponentstore` and `startcomponentcleanup` cannot run at the same time:
/// the one started later gets `0x800f0806` (another operation is in progress).
/// Stop any running analysis when cleanup starts, and start no new analyses during cleanup.
static WINSXS_CLEANUP_ACTIVE: AtomicBool = AtomicBool::new(false);

/// ID of a running analyze process, which must be terminated before cleanup starts.
static WINSXS_ANALYZE_PID: AtomicU32 = AtomicU32::new(0);

/// Hold the component store exclusively during cleanup; released automatically on `Drop`, so even an early return cannot leave it set.
struct WinsxsCleanupGuard;

impl WinsxsCleanupGuard {
    fn acquire() -> Self {
        WINSXS_CLEANUP_ACTIVE.store(true, Ordering::SeqCst);
        terminate_running_analysis();
        Self
    }
}

impl Drop for WinsxsCleanupGuard {
    fn drop(&mut self) {
        WINSXS_CLEANUP_ACTIVE.store(false, Ordering::SeqCst);
    }
}

/// Terminate the running analyze process, if any.
#[cfg(target_os = "windows")]
fn terminate_running_analysis() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let pid = WINSXS_ANALYZE_PID.swap(0, Ordering::SeqCst);
    if pid == 0 {
        return;
    }

    // analyzecomponentstore is read-only, so terminating it mid-run does not damage the component store
    let _ = Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(0x08000000)
        .output();
    warn!("清理开始前结束了正在进行的 WinSxS 分析进程 (pid {})", pid);
}

/// Kill a process and its whole child tree (H2 audit fix).
///
/// The analyze DISM runs as `cmd /C dism.exe`, so the direct child is cmd.exe and
/// dism.exe is a grandchild. `Child::kill()` terminates only cmd.exe: the surviving
/// dism.exe keeps the stdout pipe write end open, the reader thread blocks in `read()`
/// forever, and the `stdout_thread.join()` right after the kill hangs — the timeout
/// meant to prevent hangs *caused* one. `taskkill /T /F` reaps the entire tree so both
/// pipes close and the joins return.
///
/// Safe for /analyzecomponentstore, which is read-only. Do NOT reuse this on the
/// /resetbase cleanup writer: force-killing DISM midway through modifying the component
/// store can leave an intermediate state needing a system rollback (see the
/// `cleanup_orphan_dism_process` doc for the full rationale).
fn kill_process_tree(pid: u32) {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let _ = Command::new("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .creation_flags(0x08000000)
        .output();
}

#[cfg(not(target_os = "windows"))]
fn terminate_running_analysis() {}

/// Terminate a leftover DISM process from a previous run, if any.
///
/// Note: only terminate the pid we started ourselves and that is still running; never affect a DISM the user launched manually from the command line.
#[cfg(target_os = "windows")]
fn terminate_previous_dism_process() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    let previous_pid = RUNNING_DISM_PROCESS_ID.swap(0, Ordering::SeqCst);
    if previous_pid == 0 {
        return;
    }

    // The process may have already ended naturally; taskkill returns non-zero for a nonexistent pid, which is not treated as an error here.
    let result = Command::new("taskkill.exe")
        .args(["/PID", &previous_pid.to_string(), "/T", "/F"])
        .creation_flags(0x08000000)
        .output();
    match result {
        Ok(output) if output.status.success() => {
            warn!("已结束后台残留的 DISM 清理进程 (pid {})", previous_pid);
        }
        Ok(_) => {}
        Err(error) => warn!("结束残留 DISM 进程失败 (pid {}): {}", previous_pid, error),
    }
}

/// Startup checkpoint: **intentionally does nothing**.
///
/// `RUNNING_DISM_PROCESS_ID` is a process-local static, so it is always 0 after a restart ——
/// we cannot obtain the pid of "the previous DISM" here; the earlier `terminate_previous_dism_process()`
/// never actually took effect (it always read 0 and returned immediately).
///
/// From another angle, killing it here is also wrong: when the user force-closes the app, DISM continues and finishes this cleanup
/// (the child is independent of the parent), so letting it finish naturally is safest; terminating DISM **midway** through modifying the component store
/// is what could leave an intermediate state requiring system rollback —— exactly what we must avoid most.
///
/// If it really hangs, the consequence is only that the next cleanup reports `0x800f0806` (another operation is in progress),
/// and the user can simply retry; it is not worth taking this risk on the startup path.
pub fn cleanup_orphan_dism_process() {
    // Intentionally empty; see the doc comment above for the rationale.
}

// ============================================================================
// Operation execution
// ============================================================================

/// Disable hibernation (executed asynchronously with progress updates).
///
/// Note: after powercfg disables hibernation, we must wait for the system to delete hiberfil.sys (a large file that may take a second or two),
/// running it synchronously would block the main thread with no UI feedback; use spawn_blocking plus progress events instead.
pub async fn disable_hibernation_with_progress(window: &Window) -> Result<String, String> {
    run_hibernation_toggle(window, false).await
}

/// Enable hibernation (executed asynchronously with progress updates).
pub async fn enable_hibernation_with_progress(window: &Window) -> Result<String, String> {
    run_hibernation_toggle(window, true).await
}

/// Shared implementation of the hibernation toggle: when disabling, poll until hiberfil.sys is released and report the wait to the frontend.
async fn run_hibernation_toggle(window: &Window, enable: bool) -> Result<String, String> {
    if !check_admin() {
        return Err("需要管理员权限才能执行此操作，请以管理员身份运行程序".to_string());
    }

    let started_at = std::time::Instant::now();
    let item_id = "hibernation";
    let item_label = if enable {
        "开启休眠"
    } else {
        "关闭休眠"
    };

    emit_slim_progress(
        window,
        item_id,
        "preparing",
        &format!("正在{}...", item_label),
        10,
        "running",
        started_at,
    );

    let progress_window = window.clone();
    let result = tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "windows")]
        {
            let mode = if enable { "on" } else { "off" };
            let output = run_powercfg_hibernation_command(mode)?;

            if !output.status.success() {
                return Err(format!(
                    "{}失败: {}",
                    item_label,
                    decode_command_output(&output.stderr).trim()
                ));
            }

            emit_slim_progress(
                &progress_window,
                item_id,
                "waiting",
                if enable {
                    "命令已执行，正在确认系统状态..."
                } else {
                    // Deleting hiberfil.sys takes time; tell users clearly what is being waited on so they do not think it is stuck.
                    "命令已执行，正在等待系统释放 hiberfil.sys（文件较大，通常需要几秒）..."
                },
                70,
                "running",
                started_at,
            );

            let confirmed = if enable {
                check_hibernation_enabled()
            } else {
                wait_for_hibernation_disabled()
            };
            Ok::<bool, String>(confirmed)
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = progress_window;
            Err("此功能仅支持 Windows 系统".to_string())
        }
    })
    .await
    .map_err(|error| format!("任务执行失败: {}", error))?
    .map(|confirmed| {
        (
            confirmed,
            if enable {
                "休眠功能已成功开启"
            } else {
                "休眠功能已成功关闭，hiberfil.sys 文件将被删除"
            },
            if enable {
                "休眠状态尚未更新，请重新检测后再试"
            } else {
                "关闭休眠命令已返回成功，但 hiberfil.sys 仍未释放，请重新检测后再试"
            },
        )
    })?;

    let (confirmed, success_message, failure_message) = result;
    if confirmed {
        emit_slim_progress(
            window,
            item_id,
            "done",
            if enable {
                "休眠已开启"
            } else {
                "休眠已关闭"
            },
            100,
            "done",
            started_at,
        );
        info!("{}完成", item_label);
        Ok(success_message.to_string())
    } else {
        emit_slim_progress(
            window,
            item_id,
            "error",
            failure_message,
            100,
            "error",
            started_at,
        );
        Err(failure_message.to_string())
    }
}

/// Clean the WinSxS component store (async, with live progress updates)
pub async fn cleanup_winsxs(window: &Window) -> Result<String, String> {
    run_winsxs_cleanup(window, false).await
}

/// Deep-clean the WinSxS component store (ResetBase removes update rollback baselines)
pub async fn cleanup_winsxs_resetbase(window: &Window) -> Result<String, String> {
    run_winsxs_cleanup(window, true).await
}

async fn run_winsxs_cleanup(window: &Window, reset_base: bool) -> Result<String, String> {
    if !check_admin() {
        return Err("需要管理员权限才能执行此操作，请以管理员身份运行程序".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};

        // Exclusively hold the component store: stop any running analysis and start no new ones until this cleanup ends
        let _cleanup_guard = WinsxsCleanupGuard::acquire();

        info!("开始清理 WinSxS 组件存储，ResetBase: {}", reset_base);

        // First terminate any leftover DISM from a previous run, so two cleanup processes cannot modify the component store at once.
        terminate_previous_dism_process();

        let started_at = std::time::Instant::now();
        let item_id = if reset_base {
            "winsxs_resetbase"
        } else {
            "winsxs"
        };
        // DISM cleanup can take from tens of seconds to over ten minutes; the first progress message must say "this step is slow", otherwise users will think it is stuck.
        emit_slim_progress(
            window,
            item_id,
            "preparing",
            if reset_base {
                "正在准备组件基线压缩。该步骤可能持续数分钟，请保持程序运行..."
            } else {
                "正在准备组件存储清理。该步骤可能持续数分钟，请保持程序运行..."
            },
            0,
            "running",
            started_at,
        );

        let handle = window.app_handle().clone();

        let result = tokio::task::spawn_blocking(move || {
            let mut dism_args = vec![
                "/english",
                "/online",
                "/cleanup-image",
                "/startcomponentcleanup",
            ];
            if reset_base {
                dism_args.push("/resetbase");
            }

            let mut child = Command::new("dism.exe")
                .args(&dism_args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(0x08000000)
                .spawn()?;

            // Record the pid so the next launch can terminate this leftover process if the app was force-closed.
            RUNNING_DISM_PROCESS_ID.store(child.id(), Ordering::SeqCst);

            let stdout = child.stdout.take().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::Other, "无法读取 DISM 输出")
            })?;
            let stderr = child.stderr.take().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::Other, "无法读取 DISM 错误输出")
            })?;

            // Read stdout in a separate thread: the main thread waits for the process to end, so the two must not compete for the same pipe.
            // Like analyze, use read_stream_with and read by bytes here —— on Chinese systems DISM outputs GBK,
            // and BufRead::lines() would fail to decode whole lines, leaving the progress bar frozen forever.
            let progress_handle = handle.clone();
            let stdout_thread = std::thread::spawn(move || {
                let mut last_progress = 0u32;
                read_stream_with(stdout, move |line| {
                    if let Some(pct) = parse_dism_progress(line) {
                        if pct > last_progress {
                            last_progress = pct;
                            let _ = progress_handle.emit(
                                "system-slim:progress",
                                &SlimOperationProgress {
                                    item_id: item_id.to_string(),
                                    phase: "running".to_string(),
                                    message: format!("正在处理组件存储: {}%", pct),
                                    percent: pct.min(100) as u8,
                                    status: "running".to_string(),
                                    elapsed_ms: started_at.elapsed().as_millis() as u64,
                                },
                            );
                        }
                    }
                })
            });
            let stderr_thread = std::thread::spawn(move || read_stream_with(stderr, |_| {}));

            let status = child.wait()?;
            // The process has ended; clear the pid so the next launch or cleanup does not try to kill a nonexistent process.
            RUNNING_DISM_PROCESS_ID.store(0, Ordering::SeqCst);

            let stdout_bytes = stdout_thread.join().unwrap_or_default();
            let stderr_bytes = stderr_thread.join().unwrap_or_default();
            let success = status.success();

            Ok::<(Vec<u8>, Vec<u8>, bool), std::io::Error>((stdout_bytes, stderr_bytes, success))
        })
        .await
        .map_err(|e| format!("任务执行失败: {}", e))?
        .map_err(|e| format!("执行 DISM 命令失败: {}", e))?;

        let (stdout_bytes, stderr_bytes, success) = result;

        if success {
            info!("WinSxS 清理完成，ResetBase: {}", reset_base);
            clear_cached_winsxs_size();
            emit_slim_progress(
                window,
                item_id,
                "done",
                if reset_base {
                    "组件基线压缩完成"
                } else {
                    "组件存储清理完成"
                },
                100,
                "done",
                started_at,
            );
            if reset_base {
                Ok("组件基线压缩完成，当前已安装的 Windows 更新将无法卸载".to_string())
            } else {
                Ok("系统组件存储清理完成".to_string())
            }
        } else {
            // Check both streams: DISM's "Error: 740 ..." goes to stdout, though some versions write to stderr.
            // Reading only one yields an empty message (the old implementation once lost an error this way).
            let stderr = decode_command_output(&stderr_bytes);
            let stdout = decode_command_output(&stdout_bytes);
            let detail = if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                stderr.trim().to_string()
            };

            emit_slim_progress(
                window,
                item_id,
                "error",
                &format!("清理失败: {}", detail),
                0,
                "error",
                started_at,
            );
            Err(format!("清理失败: {}", detail))
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("此功能仅支持 Windows 系统".to_string())
    }
}

/// Parse the progress percentage from a DISM output line
/// DISM progress line format: "[===========================85.0%==================        ]"
fn parse_dism_progress(line: &str) -> Option<u32> {
    // Look for a progress marker of the form "XX.X%"
    let bytes = line.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b.is_ascii_digit() {
            // Check whether a '%' sign follows
            let end = bytes[i..].iter().position(|&c| c == b'%').map(|p| i + p);
            if let Some(pct_pos) = end {
                let num_str = std::str::from_utf8(&bytes[i..pct_pos]).ok()?;
                if let Ok(val) = num_str.parse::<f64>() {
                    return Some(val as u32);
                }
            }
        }
    }
    None
}

/// Open the system virtual memory settings
pub fn open_virtual_memory_settings() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        info!("打开虚拟内存设置...");

        Command::new("SystemPropertiesAdvanced.exe")
            .creation_flags(0x08000000)
            .spawn()
            .map_err(|e| format!("无法打开系统设置: {}", e))?;

        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("此功能仅支持 Windows 系统".to_string())
    }
}

// ============================================================================
// Windows Search index (rebuild when Windows.db bloats, via the official SearchManager COM interface)
// ============================================================================

/// Windows Search service status
#[derive(Debug, Clone, Copy, PartialEq)]
enum SearchServiceState {
    Running,
    Stopped,
    Disabled,
    NotInstalled,
}

/// Query the Windows Search service status (Service Control Manager + registry startup type)
fn get_wsearch_service_state() -> SearchServiceState {
    use windows::core::PCWSTR;
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT,
        SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
    };

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        // Registry startup type: 4 = disabled; when disabled, the rebuild interface cannot be invoked via COM
        let start_disabled = {
            use winreg::{enums::*, RegKey};
            if let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE)
                .open_subkey_with_flags(r"SYSTEM\CurrentControlSet\Services\WSearch", KEY_READ)
            {
                key.get_value::<u32, _>("Start")
                    .map(|v| v == 4)
                    .unwrap_or(false)
            } else {
                false
            }
        };
        if start_disabled {
            return SearchServiceState::Disabled;
        }

        // SAFETY: no machine/service name is passed (local machine, services
        // database default); SC_HANDLE values are closed on every path below
        let scm = unsafe { OpenSCManagerW(PCWSTR::null(), None, SC_MANAGER_CONNECT) };
        let scm = match scm {
            Ok(handle) => handle,
            Err(_) => return SearchServiceState::NotInstalled,
        };

        let service_name: Vec<u16> = std::ffi::OsStr::new("WSearch")
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: service_name is NUL-terminated; scm is valid from the check above
        let service = unsafe {
            OpenServiceW(
                scm,
                PCWSTR::from_raw(service_name.as_ptr()),
                SERVICE_QUERY_STATUS,
            )
        };
        let service = match service {
            Ok(handle) => handle,
            Err(_) => {
                let _ = unsafe { CloseServiceHandle(scm) };
                return SearchServiceState::NotInstalled;
            }
        };

        let mut status: SERVICE_STATUS = unsafe { std::mem::zeroed() };
        // SAFETY: status is zeroed before the call and service is a valid handle
        let ok = unsafe { QueryServiceStatus(service, &mut status) };
        let _ = unsafe { CloseServiceHandle(service) };
        let _ = unsafe { CloseServiceHandle(scm) };

        if ok.is_err() {
            return SearchServiceState::Stopped;
        }
        match status.dwCurrentState {
            SERVICE_RUNNING => SearchServiceState::Running,
            _ => SearchServiceState::Stopped,
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        SearchServiceState::NotInstalled
    }
}

/// Windows Search index database path (defaults to ProgramData; the actual path follows environment variables)
fn search_index_db_path() -> Option<std::path::PathBuf> {
    let program_data =
        std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".to_string());
    let path = std::path::Path::new(&program_data)
        .join(r"Microsoft\Search\Data\Applications\Windows\Windows.db");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// Get the search index slim item status
/// Note: do not count the database size in the total reclaimable amount (size is display-only), because the index is regenerated after a rebuild.
fn get_search_index_status() -> SlimItemStatus {
    let service_state = get_wsearch_service_state();
    let db_size = search_index_db_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);

    let (actionable, action_text, status_text) = match service_state {
        SearchServiceState::Running => (
            true,
            "重建索引".to_string(),
            format!(
                "Windows Search 服务运行中，索引数据库约 {}",
                format_bytes(db_size)
            ),
        ),
        SearchServiceState::Stopped => (
            false,
            "服务未运行".to_string(),
            "Windows Search 服务已停止，启动服务后可重建索引".to_string(),
        ),
        SearchServiceState::Disabled => (
            false,
            "服务已禁用".to_string(),
            "Windows Search 服务已被禁用，无法重建索引".to_string(),
        ),
        SearchServiceState::NotInstalled => (
            false,
            "未安装搜索服务".to_string(),
            "未检测到 Windows Search 服务".to_string(),
        ),
    };

    SlimItemStatus {
        id: "search_index".to_string(),
        name: "Windows 搜索索引".to_string(),
        description:
            "Windows 搜索索引数据库（Windows.db）异常膨胀时，重建可释放数 GB 空间，重建在后台进行"
                .to_string(),
        warning:
            "重建期间搜索结果可能不完整，重建需数小时且会重新索引全部文件；膨胀问题可能再次出现"
                .to_string(),
        status_text,
        enabled: false,
        size: db_size,
        actionable,
        action_text,
        analyzing: false,
        breakdown: None,
    }
}

/// Rebuild the Windows Search index (via the official SearchManager COM interface, equivalent to the rebuild in the system's "Indexing Options")
pub fn rebuild_search_index() -> Result<String, String> {
    if !check_admin() {
        return Err("需要管理员权限才能执行此操作，请以管理员身份运行程序".to_string());
    }

    #[cfg(target_os = "windows")]
    {
        use windows::core::HSTRING;
        use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
        };
        use windows::Win32::System::Search::{
            CSearchManager, CatalogPausedReason, CatalogStatus, ISearchManager,
            CATALOG_STATUS_FULL_CRAWL, CATALOG_STATUS_RECOVERING,
        };

        info!("开始重建 Windows 搜索索引...");

        // COM initialization: S_OK(0) means initialization succeeded and needs CoUninitialize at the end;
        // S_FALSE(1) / RPC_E_CHANGED_MODE mean the thread already has COM state, so reuse it directly.
        let co_result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let mut need_uninitialize = false;
        match co_result.0 {
            0 => need_uninitialize = true,
            1 => {}
            code if code == RPC_E_CHANGED_MODE.0 => {}
            code => {
                return Err(format!("COM 初始化失败: 0x{:08X}", code as u32));
            }
        }

        // If the index is already rebuilding/recovering, return a hint directly to avoid duplicate triggers
        let mut already_rebuilding = false;
        let rebuild_result = (|| -> windows::core::Result<()> {
            // CSearchManager is the CLSID of the SearchManager coclass in SearchAPI.dll
            let manager: ISearchManager =
                unsafe { CoCreateInstance(&CSearchManager, None, CLSCTX_ALL)? };
            let catalog = unsafe { manager.GetCatalog(&HSTRING::from("SystemIndex")) }?;

            let mut status = CatalogStatus::default();
            let mut paused_reason = CatalogPausedReason::default();
            unsafe { catalog.GetCatalogStatus(&mut status, &mut paused_reason) }?;
            if status == CATALOG_STATUS_FULL_CRAWL || status == CATALOG_STATUS_RECOVERING {
                already_rebuilding = true;
                return Ok(());
            }

            unsafe { catalog.Reindex() }
        })();

        if need_uninitialize {
            unsafe { CoUninitialize() };
        }

        match rebuild_result {
            Ok(()) if already_rebuilding => Err("搜索索引正在重建中，请稍后再试".to_string()),
            Ok(()) => {
                info!("搜索索引重建已启动");
                Ok(
                    "已开始重建 Windows 搜索索引，重建在后台进行，完成后搜索结果将恢复正常"
                        .to_string(),
                )
            }
            Err(e) => Err(format!(
                "重建索引失败: {}（请确认 Windows Search 服务已启用）",
                e.message()
            )),
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("此功能仅支持 Windows 系统".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dism_progress_percentages() {
        // DISM output looks like "[==========35.5%==========          ]"; we must be able to extract the percentage.
        assert_eq!(
            parse_dism_progress("[==========35.5%==========          ]"),
            Some(35)
        );
        assert_eq!(
            parse_dism_progress("[===========================85.0%==================        ]"),
            Some(85)
        );
        assert_eq!(parse_dism_progress("[100.0%]"), Some(100));
        // Lines without a percentage (ordinary logs) must not be mistaken for progress.
        assert_eq!(
            parse_dism_progress("Deployment Image Servicing and Management tool"),
            None
        );
        assert_eq!(parse_dism_progress(""), None);
    }

    #[test]
    fn splits_carriage_return_progress_updates() {
        // DISM redraws the progress bar with \r on the same line; the whole segment has only one \n.
        // If we split only on \n, a progress update is emitted only when the whole step ends,
        // freezing both the percentage and "elapsed Ns" in the UI (observed on 2026-11-17).
        let payload = b"[===10.0%===]\r[===35.0%===]\r[===60.0%===]\r\n";
        let mut seen: Vec<u32> = Vec::new();
        read_stream_with(&payload[..], |line| {
            if let Some(pct) = parse_dism_progress(line) {
                seen.push(pct);
            }
        });
        assert_eq!(
            seen,
            vec![10, 35, 60],
            "每个 \\r 重绘都应被当成一次进度更新"
        );
    }

    #[test]
    fn parses_whole_and_fractional_percentages() {
        // Different DISM versions emit "35.5%" or "35%"; both must be parseable.
        assert_eq!(parse_dism_progress("[===== 7% =====]"), Some(7));
        assert_eq!(parse_dism_progress("[===== 7.9% =====]"), Some(7));
    }

    #[test]
    fn caches_successful_analysis_but_not_failures() {
        // Cache successful results (including "nothing reclaimable"): 0 is the most common case, and not caching means every
        // opening of the check page reruns DISM. Cleanup actions call clear_cached_winsxs_size() to invalidate proactively.
        *WINSXS_ANALYZE_CACHE.write().unwrap() = None;

        store_memory_winsxs_result(WinsxsAnalyzeResult {
            reclaimable_size: 1024,
            cleanup_recommended: true,
            reclaimable_packages: 3,
            analysis_succeeded: true,
            ..Default::default()
        });
        assert_eq!(
            get_cached_winsxs_result().map(|result| result.reclaimable_size),
            Some(1024)
        );

        store_memory_winsxs_result(WinsxsAnalyzeResult {
            reclaimable_size: 0,
            cleanup_recommended: false,
            reclaimable_packages: 0,
            analysis_succeeded: true,
            ..Default::default()
        });
        assert_eq!(
            get_cached_winsxs_result().map(|result| result.reclaimable_size),
            Some(0),
            "0 可回收也是有效结论，必须缓存"
        );

        // A failed analysis does not overwrite an existing cache: a previous conclusion is more useful than "nothing"
        store_memory_winsxs_result(WinsxsAnalyzeResult {
            reclaimable_size: 1024,
            cleanup_recommended: true,
            reclaimable_packages: 3,
            analysis_succeeded: true,
            ..Default::default()
        });
        set_cached_winsxs_result(WinsxsAnalyzeResult::default());
        assert_eq!(
            get_cached_winsxs_result().map(|result| result.reclaimable_size),
            Some(1024),
            "失败结果不能覆盖成功结果"
        );

        *WINSXS_ANALYZE_CACHE.write().unwrap() = None;
    }

    #[test]
    fn disk_cache_round_trips_and_stops_being_usable_when_stale() {
        let result = WinsxsAnalyzeResult {
            reclaimable_size: 2 * 1024 * 1024 * 1024,
            cleanup_recommended: true,
            reclaimable_packages: 7,
            analysis_succeeded: true,
            // Sub-item details must also round-trip with the disk cache
            backups_size: 1536,
            cache_size: 512,
        };

        let json = serialize_disk_cache(result, 1_000).expect("应可序列化");
        let parsed = parse_disk_cache(&json, 1_500).expect("未过期应能读回");
        assert_eq!(parsed.reclaimable_size, result.reclaimable_size);
        assert_eq!(parsed.reclaimable_packages, 7);
        // Sub-item details must round-trip: without them, the UI would again show only a total after restart
        assert_eq!(parsed.backups_size, 1536);
        assert_eq!(parsed.cache_size, 512);

        // Do not display it past the disk TTL, to avoid treating a days-old conclusion as current
        let too_old = 1_000 + WINSXS_DISK_CACHE_TTL_SECS + 1;
        assert!(
            parse_disk_cache(&json, too_old).is_none(),
            "过期缓存应被丢弃"
        );

        // A failed-analysis result is rejected even if just written
        let failed = serialize_disk_cache(WinsxsAnalyzeResult::default(), 1_000).unwrap();
        assert!(parse_disk_cache(&failed, 1_000).is_none());

        assert!(parse_disk_cache("not json", 1_000).is_none());
    }

    #[test]
    fn decodes_multi_sz_pagefile_list() {
        // REG_MULTI_SZ is UTF-16LE: single \0 between items, double \0 at the end
        let mut bytes = Vec::new();
        for entry in [r"C:\pagefile.sys 2048 4096", r"D:\pagefile.sys 1024 1024"] {
            for unit in entry.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            bytes.extend_from_slice(&0u16.to_le_bytes());
        }
        bytes.extend_from_slice(&0u16.to_le_bytes());

        let entries = decode_reg_multi_sz(&bytes);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].starts_with(r"C:\pagefile.sys"));
        assert!(entries[1].starts_with(r"D:\pagefile.sys"));

        // An empty value (all zeros) should yield an empty list, not one empty-string item
        assert!(decode_reg_multi_sz(&[0, 0, 0, 0]).is_empty());
    }

    #[test]
    fn parses_pagefile_entries_with_and_without_sizes() {
        let entries = vec![
            r"C:\pagefile.sys 2048 4096".to_string(),
            r"D:\pagefile.sys".to_string(),
            // Skip non-drive-letter paths and placeholders
            "".to_string(),
            "*".to_string(),
        ];
        let configs = parse_pagefile_entries(&entries);

        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].max_size, 4096 * 1024 * 1024);
        // No numbers = system-managed size; leave size at 0 and let the actual file provide it
        assert_eq!(configs[1].max_size, 0);
    }

    #[test]
    fn parses_english_and_chinese_analyze_reports() {
        // English (actual output form after adding /english)
        let english = "Number of Reclaimable Packages : 2\n\
                       Component Store Cleanup Recommended : Yes\n\
                           Backups and Disabled Features : 4.46 GB\n";
        let result = parse_winsxs_analyze_output(english);
        assert!(result.analysis_succeeded);
        assert_eq!(result.reclaimable_packages, 2);
        assert!(result.cleanup_recommended);
        assert!(result.reclaimable_size > 4 * 1024 * 1024 * 1024);

        // Chinese: the label is "component store cleanup is recommended", with words inserted in the middle,
        // matching only contains("cleanup recommended") would miss it —— this has shown "no cleanup needed" on real machines.
        let chinese = "可回收的程序包数 : 2\n\
                       建议执行组件存储清理 : 是\n\
                           备份和禁用的组件 : 4.46 GB\n";
        let result = parse_winsxs_analyze_output(chinese);
        assert!(result.analysis_succeeded);
        assert_eq!(result.reclaimable_packages, 2);
        assert!(
            result.cleanup_recommended,
            "「建议执行组件存储清理 : 是」必须判为建议清理"
        );
    }

    #[test]
    fn splits_reclaimable_size_into_breakdown() {
        // DISM's reclaimable estimate consists of exactly two lines: the UI must show "superseded old component copies"
        // and "cache and temporary data" separately, otherwise users see only a total and do not know what it wants to delete.
        let english = "Backups and Disabled Features : 1.00 GB\n\
                       Cache and Temporary Data : 512 MB\n\
                       Number of Reclaimable Packages : 1\n\
                       Component Store Cleanup Recommended : Yes\n";
        let result = parse_winsxs_analyze_output(english);
        assert_eq!(result.backups_size, 1024 * 1024 * 1024, "应认出备份项");
        assert_eq!(result.cache_size, 512 * 1024 * 1024, "应认出缓存项");
        assert_eq!(
            result.reclaimable_size,
            result.backups_size + result.cache_size,
            "总数必须等于两项之和，不能只统计其中一行"
        );

        // Chinese labels must be split too: counting only the total would mix two completely different kinds of data into one figure.
        let chinese = "备份和已禁用的功能 : 2.00 GB\n\
                       缓存和临时数据 : 256 MB\n\
                       可回收的程序包数 : 1\n\
                       建议执行组件存储清理 : 是\n";
        let result = parse_winsxs_analyze_output(chinese);
        assert_eq!(result.backups_size, 2 * 1024 * 1024 * 1024);
        assert_eq!(result.cache_size, 256 * 1024 * 1024);
        assert_eq!(
            result.reclaimable_size,
            result.backups_size + result.cache_size
        );
    }

    #[test]
    fn unparseable_report_is_not_reported_as_clean() {
        // Empty or error output must not be treated as "success with 0 reclaimable" —— that would show "no cleanup needed" in the UI,
        // making users think a scan was done and found nothing. It must be judged an analysis failure.
        assert!(!parse_winsxs_analyze_output("").analysis_succeeded);
        assert!(!parse_winsxs_analyze_output("Error: 740 Elevated permissions").analysis_succeeded);

        // A conclusion was recognized and it is "no": this is the real "nothing reclaimable"
        let clean = parse_winsxs_analyze_output(
            "Number of Reclaimable Packages : 0\nCleanup Recommended : No\n",
        );
        assert!(clean.analysis_succeeded);
        assert_eq!(clean.reclaimable_packages, 0);
        assert!(!clean.cleanup_recommended);
    }

    #[test]
    fn slim_progress_payload_serializes_expected_fields() {
        let progress = SlimOperationProgress {
            item_id: "winsxs".to_string(),
            phase: "running".to_string(),
            message: "正在处理组件存储: 42%".to_string(),
            percent: 42,
            status: "running".to_string(),
            elapsed_ms: 1234,
        };
        let json = serde_json::to_value(&progress).expect("进度结构应可序列化");
        // The frontend depends on these field names; changing them would break the progress bar, so they are locked here.
        for key in [
            "item_id",
            "phase",
            "message",
            "percent",
            "status",
            "elapsed_ms",
        ] {
            assert!(json.get(key).is_some(), "缺少字段: {}", key);
        }
    }
}
