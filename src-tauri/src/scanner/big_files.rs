// ============================================================================
// Big file scanning module
// Traverses the user-selected disks and uses a min-heap to collect the top N largest files
// ============================================================================

use serde::{Deserialize, Serialize};
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use tauri::{Emitter, Window};
use walkdir::WalkDir;

#[cfg(target_os = "windows")]
pub(crate) const CLOUD_PLACEHOLDER_ATTRIBUTES: u32 = 0x0004_0000 // FILE_ATTRIBUTE_RECALL_ON_OPEN
    | 0x0040_0000; // FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS

#[cfg(target_os = "windows")]
pub(crate) fn has_cloud_placeholder_attributes(file_attributes: u32) -> bool {
    (file_attributes & CLOUD_PLACEHOLDER_ATTRIBUTES) != 0
}

#[cfg(target_os = "windows")]
fn is_cloud_placeholder(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    // Only rely on the attribute "data must be recalled from the cloud on access", to avoid hurting other types of offline files.
    has_cloud_placeholder_attributes(metadata.file_attributes())
}

// M13 fix: the cancel flag used to be a module-local `static LARGE_FILE_SCAN_CANCELLED`;
// it now lives in `crate::scan_cancel` alongside the other three scanners' flags.

// ============================================================================
// Data structures
// ============================================================================

/// Big file scan result entry
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct LargeFileEntry {
    pub path: String,
    pub size: u64,
    pub modified: i64,
    /// Risk level (1-5), computed by backend path rules
    pub risk_level: u8,
    /// Source label (e.g. "WeChat files", "virtual machine disk", "system temp files")
    pub source_label: String,
}

impl Ord for LargeFileEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.size
            .cmp(&other.size)
            .then_with(|| self.path.cmp(&other.path))
    }
}

impl PartialOrd for LargeFileEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Scan progress event payload
#[derive(Debug, Clone, Serialize)]
pub struct LargeFileScanProgress {
    pub current_path: String,
    pub scanned_count: u64,
    pub found_count: usize,
    /// Scan engine identifier: "mft" / "walkdir"
    #[serde(default)]
    pub backend: String,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub elapsed_ms: u64,
    /// Key numbers for the current stage (segmentation / read / parse, record count, candidate count…)
    #[serde(default)]
    pub detail: String,
}

// ============================================================================
// Command entry points
// ============================================================================

/// Reset the cancellation flag
pub fn reset_cancelled() {
    crate::scan_cancel::reset(crate::scan_cancel::ScanKind::BigFiles);
}

/// Set the cancellation flag
pub fn cancel() {
    crate::scan_cancel::cancel(crate::scan_cancel::ScanKind::BigFiles);
}

pub(crate) fn is_cancelled() -> bool {
    crate::scan_cancel::is_cancelled(crate::scan_cancel::ScanKind::BigFiles)
}

/// Run the big file scan (blocking; should be called inside spawn_blocking)
pub fn scan(
    window: &Window,
    top_n: usize,
    drive_letter: char,
) -> Result<Vec<LargeFileEntry>, String> {
    #[cfg(target_os = "windows")]
    {
        use std::time::Instant;

        let drive_letter = drive_letter.to_ascii_uppercase();
        // All scan engines share the same drive-letter root path, to avoid MFT and WalkDir targeting different places after the frontend switches drives.
        let root = format!("{}:\\", drive_letter);

        log::info!("开始扫描大文件: {} (Top {})", root, top_n);

        // ========================================================================
        // Try the MFT full direct-read engine (completes in seconds with admin + NTFS)
        // ========================================================================
        {
            use crate::scanner::big_files_engine::mft_core;
            let is_admin = mft_core::is_elevated();
            let is_ntfs_drive = mft_core::is_ntfs(drive_letter);

            // Inform the frontend of MFT status via a progress event (no console in release)
            let _ = window.emit(
                "large-file-scan:progress",
                LargeFileScanProgress {
                    current_path: format!(
                        "管理员:{}, NTFS:{}, 引擎:{}",
                        if is_admin { "是" } else { "否" },
                        if is_ntfs_drive { "是" } else { "否" },
                        if is_admin && is_ntfs_drive {
                            "MFT"
                        } else {
                            "常规"
                        }
                    ),
                    scanned_count: 0,
                    found_count: 0,
                    backend: if is_admin && is_ntfs_drive {
                        "mft".into()
                    } else {
                        "walkdir".into()
                    },
                    stage: "detect".into(),
                    message: "正在检测扫描引擎".into(),
                    elapsed_ms: 0,
                    detail: String::new(),
                },
            );
            if is_admin && is_ntfs_drive {
                log::info!("[BigFiles] 尝试 MFT 全量直读引擎...");
                let _ = window.emit(
                    "large-file-scan:progress",
                    LargeFileScanProgress {
                        current_path: "正在初始化 MFT 全量扫描引擎...".into(),
                        scanned_count: 0,
                        found_count: 0,
                        backend: "mft".into(),
                        stage: "init".into(),
                        message: "正在初始化 MFT 全量扫描引擎".into(),
                        elapsed_ms: 0,
                        detail: String::new(),
                    },
                );

                match crate::scanner::big_files_engine::mft_bigfiles::scan_top_files_via_mft(
                    top_n,
                    drive_letter,
                    |progress| {
                        let _ = window.emit(
                            "large-file-scan:progress",
                            LargeFileScanProgress {
                                current_path: progress.message.clone(),
                                scanned_count: progress.processed as u64,
                                found_count: progress.found_count,
                                backend: "mft".into(),
                                stage: progress.stage.clone(),
                                message: progress.message,
                                elapsed_ms: progress.elapsed_ms,
                                detail: progress.detail,
                            },
                        );
                    },
                ) {
                    Ok(results) if !results.is_empty() => {
                        log::info!("[BigFiles] MFT 全量扫描完成，返回 {} 个文件", results.len());
                        return Ok(results);
                    }
                    Ok(_empty) => {
                        let _ = window.emit(
                            "large-file-scan:progress",
                            LargeFileScanProgress {
                                current_path: "MFT 返回空结果，降级到常规扫描...".into(),
                                scanned_count: 0,
                                found_count: 0,
                                backend: "walkdir".into(),
                                stage: "fallback".into(),
                                message: "MFT 返回空结果，降级到常规扫描".into(),
                                elapsed_ms: 0,
                                detail: String::new(),
                            },
                        );
                    }
                    Err(e) => {
                        if is_cancelled() || e.contains("扫描已取消") {
                            log::info!("[BigFiles] MFT 全量扫描被用户取消");
                            let _ = window.emit("large-file-scan:cancelled", ());
                            return Ok(Vec::new());
                        }
                        let _ = window.emit(
                            "large-file-scan:progress",
                            LargeFileScanProgress {
                                current_path: format!("MFT 失败: {}，降级到常规扫描...", e),
                                scanned_count: 0,
                                found_count: 0,
                                backend: "walkdir".into(),
                                stage: "fallback".into(),
                                message: format!("MFT 失败: {}，降级到常规扫描", e),
                                elapsed_ms: 0,
                                detail: String::new(),
                            },
                        );
                    }
                }
            }
        }

        // ========================================================================
        // Fallback: WalkDir traversal (the original approach)
        // ========================================================================
        let mut heap: BinaryHeap<Reverse<LargeFileEntry>> = BinaryHeap::new();
        let mut file_count: u64 = 0;
        let mut last_emit = Instant::now();

        for entry in WalkDir::new(&root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                if e.file_type().is_dir() {
                    let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                    if name == "$recycle.bin" || name == "system volume information" {
                        return false;
                    }
                    if name == "winsxs" {
                        if let Some(parent) = e.path().parent() {
                            if parent
                                .to_string_lossy()
                                .to_ascii_lowercase()
                                .ends_with("\\windows")
                            {
                                return false;
                            }
                        }
                    }
                }
                true
            })
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            if crate::scan_cancel::is_cancelled(crate::scan_cancel::ScanKind::BigFiles) {
                log::info!("大文件扫描被用户取消，已扫描 {} 个文件", file_count);
                let _ = window.emit("large-file-scan:cancelled", ());
                let mut results: Vec<LargeFileEntry> =
                    heap.into_iter().map(|item| item.0).collect();
                results.sort_by(|a, b| b.size.cmp(&a.size));
                return Ok(results);
            }

            let path = entry.path().to_path_buf();
            let path_str = path.to_string_lossy().to_string();

            if let Ok(metadata) = entry.metadata() {
                if is_cloud_placeholder(&metadata) {
                    continue;
                }

                let size = metadata.len();
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);

                file_count += 1;

                if last_emit.elapsed().as_millis() >= 200 || file_count % 1000 == 0 {
                    let progress = LargeFileScanProgress {
                        current_path: path_str.clone(),
                        scanned_count: file_count,
                        found_count: heap.len(),
                        backend: "walkdir".into(),
                        stage: "walkdir".into(),
                        message: format!("正在遍历 {} 盘文件", drive_letter),
                        elapsed_ms: 0,
                        detail: String::new(),
                    };
                    let _ = window.emit("large-file-scan:progress", &progress);
                    last_emit = Instant::now();
                }

                let risk_level = compute_file_risk_level(&path_str);
                let source_label = compute_source_label(&path_str);

                heap.push(Reverse(LargeFileEntry {
                    path: path_str,
                    size,
                    modified,
                    risk_level,
                    source_label,
                }));

                if heap.len() > top_n {
                    heap.pop();
                }
            }
        }

        let mut results: Vec<LargeFileEntry> = heap.into_iter().map(|item| item.0).collect();
        results.sort_by(|a, b| b.size.cmp(&a.size));

        log::info!(
            "大文件扫描完成: 扫描 {} 个文件, 返回 {} 项 (Top {})",
            file_count,
            results.len(),
            top_n,
        );
        Ok(results)
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("此功能仅支持Windows系统".to_string())
    }
}

// ============================================================================
// Risk levels & source labels
// ============================================================================

/// Compute the risk level (1-5) from a file path
///
/// 1 = safe (temp files / caches)
/// 2 = low risk (media files / downloads)
/// 3 = medium risk (databases / documents)
/// 4 = higher risk (program files / Windows outside Temp)
/// 5 = system critical (System32 / drivers / page files)
pub(crate) fn compute_file_risk_level(path: &str) -> u8 {
    let lower = path.to_ascii_lowercase();
    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");
    let file_name_lower = file_name.to_ascii_lowercase();
    let ext = file_name_lower.rsplit('.').next().unwrap_or("");

    // 5 - system critical
    if lower.contains("\\windows\\system32\\")
        || lower.contains("\\windows\\syswow64\\")
        || lower.contains("\\windows\\winsxs\\")
    {
        return 5;
    }
    let critical_files = [
        "pagefile.sys",
        "hiberfil.sys",
        "swapfile.sys",
        "ntoskrnl.exe",
        "bootmgr",
    ];
    if critical_files.contains(&file_name_lower.as_str()) {
        return 5;
    }
    // sys files only lock down core system directories (system32/syswow64/drivers);
    // .sys temp files under Temp and similar directories are not locked
    if ext == "sys"
        && (lower.contains("\\system32\\")
            || lower.contains("\\syswow64\\")
            || lower.contains("\\drivers\\"))
    {
        return 5;
    }

    // Windows update cache can be cleaned safely; must be special-cased before rule 4
    if lower.contains("\\windows\\softwaredistribution\\download\\") {
        return 1;
    }

    // 4 - higher risk
    if (lower.contains("\\program files\\") || lower.contains("\\program files (x86)\\"))
        && ["exe", "dll", "ocx", "msi"].contains(&ext)
    {
        return 4;
    }
    if lower.contains("\\windows\\") && !lower.contains("\\temp\\") {
        return 4;
    }

    // 3 - medium risk
    let data_exts = [
        "db", "sqlite", "mdf", "ldf", "accdb", "mdb", "vmdk", "vdi", "vhd", "vhdx", "doc", "docx",
        "xls", "xlsx", "ppt", "pptx", "pdf",
    ];
    if data_exts.contains(&ext) {
        return 3;
    }

    // 2 - low risk
    let media_exts = [
        "mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "m4v", "mp3", "wav", "flac",
    ];
    if media_exts.contains(&ext) {
        return 2;
    }
    if lower.contains("\\downloads\\") || lower.contains("\\desktop\\") {
        return 2;
    }

    // 1 - safe
    if lower.contains("\\temp\\") || lower.contains("\\tmp\\") || lower.contains("\\cache\\") {
        return 1;
    }
    let safe_exts = ["log", "tmp", "bak", "old", "dmp"];
    if safe_exts.contains(&ext) {
        return 1;
    }

    3
}

/// Identify the source label from a file path
pub(crate) fn compute_source_label(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("");
    let file_name_lower = file_name.to_ascii_lowercase();
    let ext = file_name_lower.rsplit('.').next().unwrap_or("");

    // Identify by extension
    if ["vmdk", "vdi", "vhd", "vhdx"].contains(&ext) {
        return "虚拟机磁盘".to_string();
    }
    if ext == "dmp" || file_name_lower == "memory.dmp" {
        return "内存转储".to_string();
    }
    if ["iso", "img"].contains(&ext) {
        return "光盘镜像".to_string();
    }
    if ["db", "sqlite", "mdf", "ldf", "accdb", "mdb"].contains(&ext) {
        return "数据库文件".to_string();
    }
    if [
        "zip", "rar", "7z", "tar", "gz", "bz2", "xz", "zst", "cab", "tar.gz",
    ]
    .contains(&ext)
    {
        return "压缩包".to_string();
    }
    if ext == "log" {
        return "日志文件".to_string();
    }

    // Identify by path pattern
    if lower.contains("\\windows\\temp\\") || lower.contains("\\appdata\\local\\temp\\") {
        return "系统临时文件".to_string();
    }
    if lower.contains("\\windows\\softwaredistribution\\") {
        return "Windows 更新缓存".to_string();
    }
    if lower.contains("\\steam\\") {
        return "Steam 游戏文件".to_string();
    }
    if lower.contains("\\wechat files\\") {
        return "微信文件".to_string();
    }
    if lower.contains("\\chrome\\") || lower.contains("\\google\\chrome\\") {
        return "Chrome 浏览器".to_string();
    }
    if lower.contains("\\edge\\") || lower.contains("\\microsoft\\edge\\") {
        return "Edge 浏览器".to_string();
    }
    if lower.contains("\\firefox\\") {
        return "Firefox 浏览器".to_string();
    }
    if lower.contains("\\downloads\\") {
        return "下载文件".to_string();
    }
    if lower.contains("\\desktop\\") {
        return "桌面文件".to_string();
    }
    if lower.contains("\\recycle") || file_name_lower.starts_with("$r") {
        return "回收站".to_string();
    }

    // Media files
    if ["mp4", "mkv", "avi", "mov", "wmv"].contains(&ext) {
        return "视频文件".to_string();
    }
    if ["mp3", "wav", "flac", "aac"].contains(&ext) {
        return "音频文件".to_string();
    }
    if ["jpg", "jpeg", "png", "gif", "bmp"].contains(&ext) {
        return "图片文件".to_string();
    }

    "未知来源".to_string()
}
