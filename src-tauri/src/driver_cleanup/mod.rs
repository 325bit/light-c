// ============================================================================
// Old driver cleanup module
//
// Only manages driver packages in the Driver Store via pnputil; never manipulates DriverStore files directly,
// so Windows itself handles dependency, signature, and deletion safety checks for driver packages.
// ============================================================================

use chrono::Local;
use log::{info, warn};
use quick_xml::{events::Event, Reader, XmlVersion};
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use walkdir::WalkDir;

const DRIVER_BACKUP_DIR: &str = "driver_backups";
#[cfg(target_os = "windows")]
const HIDDEN_PROCESS_FLAGS: u32 = 0x08000000 | 0x00000008;

// Deletion modifies the system Driver Store; serializing backend requests prevents two cleanup tasks from interleaving backups or deletions.
static DRIVER_DELETE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, Serialize)]
pub struct DriverPackageInfo {
    pub published_name: String,
    pub original_name: String,
    pub provider_name: String,
    pub class_name: String,
    pub driver_version: String,
    pub family_id: String,
    pub signer_name: String,
    pub driver_store_path: String,
    pub device_count: usize,
    pub active_device_count: usize,
    pub installed_device_count: usize,
    pub outranked_device_count: usize,
    pub file_count: usize,
    /// Estimated usage of the driver package directory (bytes), shown in the delete confirmation dialog; 0 on read failure means unknown.
    pub total_size: u64,
    pub status: String,
    pub actionable: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriverScanResult {
    pub is_admin: bool,
    pub packages: Vec<DriverPackageInfo>,
    pub total_count: usize,
    pub candidate_count: usize,
    pub high_confidence_count: usize,
    pub device_match_data_available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriverDeleteDetail {
    pub published_name: String,
    pub success: bool,
    pub verified_removed: bool,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriverDeleteResult {
    pub backup_directory: String,
    pub success_count: usize,
    pub failed_count: usize,
    pub needs_reboot: bool,
    pub details: Vec<DriverDeleteDetail>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriverRestoreResult {
    pub backup_directory: String,
    pub success: bool,
    pub needs_reboot: bool,
    pub message: String,
}

#[derive(Debug, Default)]
struct RawDriverPackage {
    published_name: String,
    original_name: String,
    provider_name: String,
    class_name: String,
    driver_version: String,
    family_id: String,
    signer_name: String,
    driver_package_id: String,
    device_count: usize,
    active_device_count: usize,
    file_count: usize,
}

#[derive(Debug, Default)]
struct RawDeviceMatch {
    installed_driver_name: String,
    outranked_driver_names: Vec<String>,
}

#[derive(Debug, Default)]
struct PnputilOutput {
    stdout: String,
    stderr: String,
}

pub fn scan() -> Result<DriverScanResult, String> {
    let raw_packages = enumerate_driver_packages()?;
    let (device_matches, device_match_data_available) = match enumerate_device_matches() {
        Ok(matches) => (matches, true),
        Err(error) => {
            // Device matching XML is only used to raise confidence in the determination; on failure the basic scan result is kept, avoiding any impact on old system detection.
            warn!("读取设备驱动匹配关系失败，将使用保守基础判定: {}", error);
            (Vec::new(), false)
        }
    };
    let packages = classify_packages(raw_packages, &device_matches);
    let candidate_count = packages.iter().filter(|package| package.actionable).count();
    let high_confidence_count = packages
        .iter()
        .filter(|package| package.status == "old_confirmed")
        .count();

    Ok(DriverScanResult {
        is_admin: crate::system_slim::check_admin(),
        total_count: packages.len(),
        packages,
        candidate_count,
        high_confidence_count,
        device_match_data_available,
    })
}

pub fn restore_all_backups() -> Result<DriverRestoreResult, String> {
    let _delete_guard = DRIVER_DELETE_LOCK
        .lock()
        .map_err(|_| "驱动清理锁异常，请重启 LightC 后重试".to_string())?;

    if !crate::system_slim::check_admin() {
        return Err("恢复驱动包需要管理员权限，请以管理员身份运行 LightC".to_string());
    }

    let backup_directory = crate::data_dir::get_data_dir().join(DRIVER_BACKUP_DIR);
    if !backup_directory.is_dir() {
        return Err(format!(
            "当前数据目录没有找到驱动备份目录: {}",
            backup_directory.display()
        ));
    }

    let inf_files = collect_backup_inf_files(&backup_directory)?;
    if inf_files.is_empty() {
        return Err(format!(
            "当前驱动备份目录中没有找到 INF 文件: {}",
            backup_directory.display()
        ));
    }

    // Launch pnputil only once to reduce process overhead when restoring many drivers; first enumerate to verify the directory really contains backups.
    let driver_pattern = backup_directory
        .join("*.inf")
        .to_string_lossy()
        .into_owned();
    let output = run_pnputil([
        "/add-driver".to_string(),
        driver_pattern,
        "/subdirs".to_string(),
        "/install".to_string(),
    ])?;
    let command_message = format_command_output(&output.output);
    let message = format!("已发现 {} 个备份 INF。{}", inf_files.len(), command_message);
    let output_lower = command_message.to_ascii_lowercase();
    let needs_reboot = output_lower.contains("restart")
        || output_lower.contains("reboot")
        || command_message.contains("重启");

    Ok(DriverRestoreResult {
        backup_directory: backup_directory.to_string_lossy().to_string(),
        success: output.status_success,
        needs_reboot,
        message,
    })
}

pub fn delete(published_names: Vec<String>) -> Result<DriverDeleteResult, String> {
    let _delete_guard = DRIVER_DELETE_LOCK
        .lock()
        .map_err(|_| "驱动清理锁异常，请重启 LightC 后重试".to_string())?;

    if published_names.is_empty() {
        return Err("未选择要清理的驱动包".to_string());
    }
    if !crate::system_slim::check_admin() {
        return Err("删除驱动包需要管理员权限，请以管理员身份运行 LightC".to_string());
    }

    let selected_names = normalize_published_names(&published_names)?;
    let current_scan = scan()?;
    let selected_packages = validate_selected_packages(&current_scan, &selected_names)?;
    let backup_directory = create_backup_root()?;

    // Each OEM package uses a stable directory; retrying after a failed deletion reuses the existing backup instead of creating timestamped directories endlessly.
    for package in &selected_packages {
        backup_driver_package(package, &backup_directory)?;
    }

    let mut details = Vec::with_capacity(selected_packages.len());
    let mut needs_reboot = false;
    for package in selected_packages {
        let published_name = package.published_name;
        // Use only /force: old driver packages (Outranked) are still referenced by the device driver match list, and pnputil without
        // /force refuses to delete them; when /uninstall is used together with /force, /force is ignored, and
        // /uninstall fails because "the INF is still installed for a device". No active devices were confirmed before deletion, so /force is safe.
        let command_result = run_pnputil(&["/delete-driver", published_name.as_str(), "/force"]);
        let (command_success, output_text) = match command_result {
            Ok(output) => (output.status_success, format_command_output(&output.output)),
            Err(error) => (false, error),
        };
        if output_text.to_ascii_lowercase().contains("restart")
            || output_text.to_ascii_lowercase().contains("reboot")
            || output_text.contains("重启")
        {
            needs_reboot = true;
        }

        details.push(DriverDeleteDetail {
            published_name,
            success: command_success,
            verified_removed: false,
            error_message: if command_success {
                None
            } else {
                Some(output_text)
            },
        });
    }

    // Recheck all deletion results at once, avoiding a pnputil call per package, reducing system IO and process overhead.
    match enumerate_driver_packages() {
        Ok(packages) => {
            let remaining_names = packages
                .into_iter()
                .map(|package| package.published_name.to_ascii_lowercase())
                .collect::<std::collections::HashSet<_>>();
            for detail in &mut details {
                detail.verified_removed =
                    !remaining_names.contains(&detail.published_name.to_ascii_lowercase());
                if !detail.success && detail.verified_removed {
                    // pnputil returned a failure code but the recheck confirms the package is gone; the actual result wins.
                    detail.success = true;
                    detail.error_message = None;
                } else if detail.success && !detail.verified_removed {
                    detail.success = false;
                    detail.error_message =
                        Some("pnputil 已执行，但重新检测仍发现该驱动包".to_string());
                }
            }
        }
        Err(error) => {
            // The delete command has already run; a failed recheck must not be disguised as a deletion failure — tell the user clearly to recheck.
            for detail in &mut details {
                if detail.success {
                    detail.success = false;
                    detail.error_message =
                        Some(format!("删除命令已执行，但删除后复核失败: {}", error));
                }
            }
        }
    }

    let success_count = details.iter().filter(|detail| detail.success).count();
    let failed_count = details.len() - success_count;
    info!(
        "旧驱动清理完成: 成功 {}, 失败 {}, 备份目录 {}",
        success_count,
        failed_count,
        backup_directory.display()
    );

    Ok(DriverDeleteResult {
        backup_directory: backup_directory.to_string_lossy().to_string(),
        success_count,
        failed_count,
        needs_reboot,
        details,
    })
}

pub fn backup_directory() -> Result<String, String> {
    let directory = crate::data_dir::get_data_dir().join(DRIVER_BACKUP_DIR);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("创建驱动备份目录失败 {}: {}", directory.display(), error))?;
    Ok(directory.to_string_lossy().to_string())
}

fn enumerate_driver_packages() -> Result<Vec<RawDriverPackage>, String> {
    let xml_path = temporary_xml_path();
    let xml_path_text = xml_path.to_string_lossy().to_string();
    let xml_result = (|| {
        let output = run_pnputil(&[
            "/enum-drivers",
            "/files",
            "/ids",
            "/devices",
            "/format",
            "xml",
            "/output-file",
            &xml_path_text,
        ])?;
        if !output.status_success || !xml_path.is_file() {
            return Err(format_command_output(&output.output));
        }
        parse_driver_xml(&xml_path)
    })();

    let result = match xml_result {
        Ok(packages) if !packages.is_empty() => Ok(packages),
        Ok(_) => {
            warn!("pnputil XML 枚举未返回驱动包，回退到文本枚举");
            enumerate_driver_packages_text()
        }
        Err(error) => {
            warn!("pnputil XML 枚举不可用，将回退到文本枚举: {}", error);
            enumerate_driver_packages_text()
        }
    };

    if let Err(error) = fs::remove_file(&xml_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            warn!("删除临时驱动检测文件失败 {}: {}", xml_path.display(), error);
        }
    }
    result
}

/// Enumerate driver packages using the plain-text output of the legacy pnputil, compatible with Windows versions that do not support the XML switch.
fn enumerate_driver_packages_text() -> Result<Vec<RawDriverPackage>, String> {
    let output = run_pnputil(["/enum-drivers"])?;
    if !output.status_success {
        return Err(format!(
            "枚举驱动包失败: {}",
            format_command_output(&output.output)
        ));
    }

    let packages = parse_driver_text(&output.output.stdout);
    if packages.is_empty() {
        return Err(format!(
            "枚举驱动包失败：pnputil 未返回可识别的驱动包信息。{}",
            format_command_output(&output.output)
        ));
    }
    Ok(packages)
}

fn enumerate_device_matches() -> Result<Vec<RawDeviceMatch>, String> {
    let xml_path = temporary_xml_path();
    let xml_path_text = xml_path.to_string_lossy().to_string();
    let result = (|| {
        let output = run_pnputil(&[
            "/enum-devices",
            "/drivers",
            "/format",
            "xml",
            "/output-file",
            &xml_path_text,
        ])?;
        if !output.status_success {
            return Err(format!(
                "枚举设备驱动匹配关系失败: {}",
                format_command_output(&output.output)
            ));
        }
        parse_device_matches_xml(&xml_path)
    })();

    if let Err(error) = fs::remove_file(&xml_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            warn!("删除临时设备匹配文件失败 {}: {}", xml_path.display(), error);
        }
    }
    result
}

fn classify_packages(
    raw_packages: Vec<RawDriverPackage>,
    device_matches: &[RawDeviceMatch],
) -> Vec<DriverPackageInfo> {
    let mut family_versions: HashMap<String, Vec<Vec<u64>>> = HashMap::new();
    let mut installed_device_counts: HashMap<String, usize> = HashMap::new();
    let mut outranked_device_counts: HashMap<String, usize> = HashMap::new();
    for device in device_matches {
        let installed_name = device.installed_driver_name.to_ascii_lowercase();
        if is_published_driver_name(&installed_name) {
            *installed_device_counts.entry(installed_name).or_default() += 1;
        }
        for driver_name in &device.outranked_driver_names {
            let normalized_name = driver_name.to_ascii_lowercase();
            if is_published_driver_name(&normalized_name) {
                *outranked_device_counts.entry(normalized_name).or_default() += 1;
            }
        }
    }
    for package in &raw_packages {
        if let (Some(version), false) = (
            parse_driver_version(&package.driver_version),
            package.family_id.trim().is_empty(),
        ) {
            family_versions
                .entry(package.family_id.to_ascii_lowercase())
                .or_default()
                .push(version);
        }
    }

    raw_packages
        .into_iter()
        .map(|package| {
            let driver_store_path = build_driver_store_path(&package.driver_package_id);
            let parsed_version = parse_driver_version(&package.driver_version);
            let has_newer_version = parsed_version.as_ref().is_some_and(|current_version| {
                family_versions
                    .get(&package.family_id.to_ascii_lowercase())
                    .into_iter()
                    .flatten()
                    .any(|candidate| {
                        compare_versions(candidate, current_version) == Ordering::Greater
                    })
            });
            let published_name = package.published_name.to_ascii_lowercase();
            let installed_device_count = installed_device_counts
                .get(&published_name)
                .copied()
                .unwrap_or_default();
            let outranked_device_count = outranked_device_counts
                .get(&published_name)
                .copied()
                .unwrap_or_default();
            let high_confidence_old = installed_device_count == 0
                && outranked_device_count > 0
                && package.active_device_count == 0;
            let (status, actionable, reason) = if package.active_device_count > 0 {
                (
                    "in_use",
                    false,
                    format!(
                        "有 {} 个设备处于活动状态，不能删除正在使用的驱动包",
                        package.active_device_count
                    ),
                )
            } else if high_confidence_old {
                (
                    "old_confirmed",
                    true,
                    format!(
                        "在 {} 个设备的匹配列表中被更高排名驱动替代，当前没有设备使用此包",
                        outranked_device_count
                    ),
                )
            } else if package.family_id.trim().is_empty() || parsed_version.is_none() {
                (
                    "unknown",
                    true,
                    "未关联设备，但版本信息不完整，请确认后再删除".to_string(),
                )
            } else if has_newer_version {
                (
                    "recommended",
                    true,
                    "未关联设备，且同一驱动族存在更新版本".to_string(),
                )
            } else {
                (
                    "no_newer_version",
                    true,
                    "未关联设备，未发现更高版本；删除后将从 Driver Store 移除该包".to_string(),
                )
            };

            // A driver package's usage cannot be reported directly by pnputil, so it is measured from the actual file sizes in the driver directory,
            // falling back to 0 when the directory is unreadable; the frontend treats that as "unknown", avoiding fake data in the dialog.
            // Must be computed before driver_store_path is moved into the struct.
            let total_size = sum_driver_package_size(&driver_store_path);

            DriverPackageInfo {
                published_name: package.published_name,
                original_name: package.original_name,
                provider_name: package.provider_name,
                class_name: package.class_name,
                driver_version: package.driver_version,
                family_id: package.family_id,
                signer_name: package.signer_name,
                driver_store_path,
                device_count: package.device_count,
                active_device_count: package.active_device_count,
                installed_device_count,
                outranked_device_count,
                file_count: package.file_count,
                total_size,
                status: status.to_string(),
                actionable,
                reason,
            }
        })
        .collect()
}

/// Calculate the usage of a single driver package directory; Windows hard-links some files to other system locations,
/// so this is only an estimate for display, with no deduplication; read failures are treated as 0.
fn sum_driver_package_size(driver_store_path: &str) -> u64 {
    if driver_store_path.is_empty() {
        return 0;
    }
    WalkDir::new(driver_store_path)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

fn is_published_driver_name(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    let number_part = lower
        .strip_prefix("oem")
        .and_then(|value| value.strip_suffix(".inf"));
    number_part.is_some_and(|value| {
        !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
    })
}

fn validate_selected_packages(
    scan_result: &DriverScanResult,
    selected_names: &[String],
) -> Result<Vec<DriverPackageInfo>, String> {
    let packages_by_name = scan_result
        .packages
        .iter()
        .map(|package| (package.published_name.to_ascii_lowercase(), package))
        .collect::<HashMap<_, _>>();
    let mut selected_packages = Vec::with_capacity(selected_names.len());
    for name in selected_names {
        let Some(package) = packages_by_name.get(name) else {
            return Err(format!("驱动包 {} 已不存在，请重新检测", name));
        };
        if !package.actionable {
            return Err(format!(
                "驱动包 {} 当前不满足安全清理条件: {}",
                name, package.reason
            ));
        }
        selected_packages.push((*package).clone());
    }
    Ok(selected_packages)
}

fn normalize_published_names(names: &[String]) -> Result<Vec<String>, String> {
    let mut normalized = Vec::with_capacity(names.len());
    for name in names {
        let trimmed = name.trim();
        let lower = trimmed.to_ascii_lowercase();
        let number_part = lower
            .strip_prefix("oem")
            .and_then(|value| value.strip_suffix(".inf"));
        let is_valid = lower.starts_with("oem")
            && lower.ends_with(".inf")
            && number_part.is_some_and(|value| {
                !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
            });
        if !is_valid {
            return Err(format!("非法驱动包标识: {}", name));
        }
        if !normalized.contains(&lower) {
            normalized.push(lower);
        }
    }
    Ok(normalized)
}

fn backup_driver_package(package: &DriverPackageInfo, backup_root: &Path) -> Result<(), String> {
    let backup_directory = backup_root.join(&package.published_name);
    if has_backup_inf(&backup_directory, &package.original_name) {
        info!(
            "复用已有驱动备份: {} -> {}",
            package.published_name,
            backup_directory.display()
        );
        return Ok(());
    }

    fs::create_dir_all(&backup_directory).map_err(|error| {
        format!(
            "创建驱动 {} 的备份目录失败 {}: {}",
            package.published_name,
            backup_directory.display(),
            error
        )
    })?;
    let output = run_pnputil(&[
        "/export-driver",
        package.published_name.as_str(),
        &backup_directory.to_string_lossy(),
    ])?;
    if output.status_success && has_backup_inf(&backup_directory, &package.original_name) {
        Ok(())
    } else {
        Err(format!(
            "备份驱动包 {} 失败或备份文件不完整: {}",
            package.published_name,
            format_command_output(&output.output)
        ))
    }
}

fn collect_backup_inf_files(backup_directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut inf_files = Vec::new();
    for entry in WalkDir::new(backup_directory).follow_links(false) {
        let entry = entry.map_err(|error| format!("读取驱动备份目录失败: {}", error))?;
        if entry.file_type().is_file()
            && entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))
        {
            inf_files.push(entry.into_path());
        }
    }
    inf_files.sort_unstable_by(|left, right| {
        left.to_string_lossy()
            .to_ascii_lowercase()
            .cmp(&right.to_string_lossy().to_ascii_lowercase())
    });
    Ok(inf_files)
}

fn create_backup_root() -> Result<PathBuf, String> {
    let directory = crate::data_dir::get_data_dir().join(DRIVER_BACKUP_DIR);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("创建驱动备份目录失败 {}: {}", directory.display(), error))?;
    Ok(directory)
}

fn has_backup_inf(directory: &Path, expected_name: &str) -> bool {
    let expected_name = expected_name.trim();
    directory.is_dir()
        && WalkDir::new(directory)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .any(|entry| {
                entry.file_type().is_file()
                    && entry
                        .path()
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("inf"))
                    && entry
                        .path()
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.eq_ignore_ascii_case(expected_name))
            })
}

fn temporary_xml_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "lightc_driver_scan_{}_{}.xml",
        std::process::id(),
        Local::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

fn parse_driver_xml(path: &Path) -> Result<Vec<RawDriverPackage>, String> {
    let mut reader = Reader::from_file(path)
        .map_err(|error| format!("读取 pnputil XML 失败 {}: {}", path.display(), error))?;
    // trim_text stays off on purpose: quick-xml 0.42 emits entity references as their own
    // `GeneralRef` events, so a value like `Realtek &amp; Co.` arrives as three events. Trimming
    // each fragment would eat the spaces around the entity and yield `Realtek&Co.`; trimming is
    // therefore done once on the assembled string instead.
    let mut buffer = Vec::new();
    let mut current_package: Option<RawDriverPackage> = None;
    let mut current_text_element: Option<String> = None;
    let mut current_text = String::new();
    let mut current_device_open = false;
    let mut packages = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let element_name = event.local_name().as_ref().to_string();
                if element_name == "Driver" {
                    current_package = Some(RawDriverPackage {
                        published_name: read_attribute(&event, "DriverName")?,
                        ..RawDriverPackage::default()
                    });
                } else if let Some(package) = current_package.as_mut() {
                    if element_name == "Device" {
                        package.device_count += 1;
                        current_device_open = true;
                    } else if element_name == "File" {
                        package.file_count += 1;
                    } else if is_driver_text_element(&element_name) {
                        current_text_element = Some(element_name.clone());
                        current_text.clear();
                    }
                }
            }
            Ok(Event::Empty(event)) => {
                let element_name = event.local_name();
                if let Some(package) = current_package.as_mut() {
                    if element_name.as_ref() == "Device" {
                        package.device_count += 1;
                        current_device_open = false;
                    } else if element_name.as_ref() == "File" {
                        package.file_count += 1;
                    }
                }
            }
            Ok(Event::Text(event)) => {
                if current_text_element.is_some() {
                    // quick-xml 0.42 dropped BytesText::decode(): text arrives as valid UTF-8
                    // with entity references already split into their own events, so only EOL
                    // normalization is left to do here.
                    current_text.push_str(&event.xml10_content());
                }
            }
            Ok(Event::GeneralRef(reference)) => {
                if current_text_element.is_some() {
                    push_general_ref(&mut current_text, reference.as_ref())?;
                }
            }
            Ok(Event::End(event)) => {
                let element_name = event.local_name();
                if let Some(text_element) = current_text_element.as_deref() {
                    if text_element == element_name.as_ref() {
                        if let Some(package) = current_package.as_mut() {
                            if text_element == "Status" && current_device_open {
                                if is_active_device_status(&current_text) {
                                    package.active_device_count += 1;
                                }
                            } else {
                                assign_driver_text(package, text_element, &current_text);
                            }
                        }
                        current_text_element = None;
                        current_text.clear();
                    }
                }
                if element_name.as_ref() == "Device" {
                    current_device_open = false;
                }
                if element_name.as_ref() == "Driver" {
                    if let Some(package) = current_package.take() {
                        packages.push(package);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("解析 pnputil XML 失败: {}", error)),
            _ => {}
        }
        buffer.clear();
    }
    Ok(packages)
}

fn parse_device_matches_xml(path: &Path) -> Result<Vec<RawDeviceMatch>, String> {
    let mut reader = Reader::from_file(path)
        .map_err(|error| format!("读取设备匹配 XML 失败 {}: {}", path.display(), error))?;
    // Same reasoning as parse_driver_xml: no per-fragment trimming, because entity references
    // arrive as separate events and trimming each fragment would drop the surrounding spaces.
    let mut buffer = Vec::new();
    let mut current_device: Option<RawDeviceMatch> = None;
    let mut current_driver_name = String::new();
    let mut current_driver_status = String::new();
    let mut current_matching_driver = false;
    let mut current_text_element: Option<String> = None;
    let mut current_text = String::new();
    let mut matches = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let element_name = event.local_name().as_ref().to_string();
                if element_name == "Device" {
                    current_device = Some(RawDeviceMatch::default());
                } else if element_name == "DriverName" {
                    let driver_name = read_attribute(&event, "DriverName")?;
                    if driver_name.is_empty() {
                        // The current driver of a device uses a text node, while the matching driver uses the DriverName attribute.
                        current_matching_driver = false;
                        current_text_element = Some("DriverName".to_string());
                        current_text.clear();
                    } else {
                        current_matching_driver = true;
                        current_driver_name = driver_name;
                        current_driver_status.clear();
                    }
                } else if element_name == "Status" {
                    current_text_element = Some("Status".to_string());
                    current_text.clear();
                }
            }
            Ok(Event::Text(event)) => {
                if current_text_element.is_some() {
                    current_text.push_str(&event.xml10_content());
                }
            }
            Ok(Event::GeneralRef(reference)) => {
                if current_text_element.is_some() {
                    push_general_ref(&mut current_text, reference.as_ref())?;
                }
            }
            Ok(Event::End(event)) => {
                let element_name = event.local_name();
                if let Some(text_element) = current_text_element.as_deref() {
                    if text_element == element_name.as_ref() {
                        if text_element == "DriverName" {
                            if current_driver_name.is_empty() {
                                current_driver_name = current_text.trim().to_string();
                            }
                        } else if text_element == "Status" {
                            current_driver_status = current_text.trim().to_string();
                            if current_matching_driver {
                                if let Some(device) = current_device.as_mut() {
                                    if current_driver_status.contains("Outranked") {
                                        device
                                            .outranked_driver_names
                                            .push(current_driver_name.clone());
                                    } else if current_driver_status.contains("BestRanked/Installed")
                                        || current_driver_status == "BestRanked"
                                    {
                                        device.installed_driver_name = current_driver_name.clone();
                                    }
                                }
                                current_driver_name.clear();
                                current_driver_status.clear();
                                current_matching_driver = false;
                            }
                        }
                        current_text_element = None;
                        current_text.clear();
                    }
                }
                if element_name.as_ref() == "DriverName"
                    && !current_matching_driver
                    && !current_driver_name.is_empty()
                {
                    if let Some(device) = current_device.as_mut() {
                        // The text DriverName under a device node is the currently installed driver; do not confuse it with the attribute nodes of match candidates.
                        device.installed_driver_name = current_driver_name.clone();
                    }
                    current_driver_name.clear();
                    current_driver_status.clear();
                }
                if element_name.as_ref() == "Device" {
                    if let Some(device) = current_device.take() {
                        matches.push(device);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("解析设备匹配 XML 失败: {}", error)),
            _ => {}
        }
        buffer.clear();
    }
    Ok(matches)
}

fn is_driver_text_element(name: &str) -> bool {
    matches!(
        name,
        "OriginalName"
            | "ProviderName"
            | "ClassName"
            | "DriverVersion"
            | "FamilyId"
            | "SignerName"
            | "DriverPackageId"
            | "Status"
    )
}

fn is_active_device_status(status: &str) -> bool {
    // Only forbid deletion when Windows explicitly reports a device as started or running, to avoid misjudging a disconnected device as in use.
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "started" | "running"
    )
}

fn assign_driver_text(package: &mut RawDriverPackage, element: &str, text: &str) {
    let target = match element {
        "OriginalName" => &mut package.original_name,
        "ProviderName" => &mut package.provider_name,
        "ClassName" => &mut package.class_name,
        "DriverVersion" => &mut package.driver_version,
        "FamilyId" => &mut package.family_id,
        "SignerName" => &mut package.signer_name,
        "DriverPackageId" => &mut package.driver_package_id,
        _ => return,
    };
    *target = text.trim().to_string();
}

/// Parse the legacy pnputil text format, extracting only basic package info; device matching is still determined by the XML capability.
fn parse_driver_text(text: &str) -> Vec<RawDriverPackage> {
    let mut packages = Vec::new();
    let mut current: Option<RawDriverPackage> = None;
    let mut reading_driver_files = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some((field, value)) = parse_text_field(trimmed) {
            if field == TextDriverField::PublishedName {
                if let Some(package) = current.take() {
                    packages.push(package);
                }
                reading_driver_files = false;
                current = Some(RawDriverPackage {
                    published_name: value,
                    ..RawDriverPackage::default()
                });
                continue;
            }

            let Some(package) = current.as_mut() else {
                continue;
            };
            reading_driver_files = field == TextDriverField::DriverFiles;
            match field {
                TextDriverField::OriginalName => package.original_name = value,
                TextDriverField::ProviderName => package.provider_name = value,
                TextDriverField::ClassName => package.class_name = value,
                TextDriverField::DriverVersion => package.driver_version = value,
                TextDriverField::SignerName => package.signer_name = value,
                TextDriverField::DriverPackageId => package.driver_package_id = value,
                TextDriverField::FamilyId => package.family_id = value,
                TextDriverField::DriverFiles => package.file_count = 0,
                TextDriverField::PublishedName => unreachable!(),
            }
            continue;
        }

        let Some(package) = current.as_mut() else {
            continue;
        };
        if !line.starts_with(' ') && !line.starts_with('\t') {
            // Keep the boundary check to avoid mistaking non-package heading text in the next segment for a driver file.
            continue;
        } else if reading_driver_files {
            // A driver file path itself may contain a drive colon, so "contains a colon" cannot be used to exclude it.
            package.file_count = package.file_count.saturating_add(1);
        }
    }

    if let Some(package) = current {
        packages.push(package);
    }
    packages
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextDriverField {
    PublishedName,
    OriginalName,
    ProviderName,
    ClassName,
    DriverVersion,
    SignerName,
    DriverPackageId,
    FamilyId,
    DriverFiles,
}

/// Parse pnputil text fields, compatible with English/Chinese systems and with spaces between field names and colons.
fn parse_text_field(line: &str) -> Option<(TextDriverField, String)> {
    let (raw_name, raw_value) = line.split_once(':')?;
    let field = match normalize_text_field_name(raw_name) {
        name if matches_any(&name, &["publishedname", "发布名称", "发布的名称"]) => {
            TextDriverField::PublishedName
        }
        name if matches_any(&name, &["originalname", "原始名称"]) => {
            TextDriverField::OriginalName
        }
        name if matches_any(&name, &["providername", "提供程序名称", "提供商名称"]) => {
            TextDriverField::ProviderName
        }
        name if matches_any(&name, &["classname", "类名"]) => TextDriverField::ClassName,
        name if matches_any(&name, &["driverversion", "驱动程序版本", "驱动版本"]) => {
            TextDriverField::DriverVersion
        }
        name if matches_any(&name, &["signername", "签名者名称", "签名名称"]) => {
            TextDriverField::SignerName
        }
        name if matches_any(&name, &["driverpackageid", "驱动程序包id", "驱动包id"]) => {
            TextDriverField::DriverPackageId
        }
        name if matches_any(&name, &["familyid", "系列id", "驱动系列id"]) => {
            TextDriverField::FamilyId
        }
        name if matches_any(&name, &["driverfiles", "驱动程序文件", "驱动文件"]) => {
            TextDriverField::DriverFiles
        }
        // Unknown fields return no result, to avoid mistaking C:\Windows\System32\xxx.sys for a field.
        _ => return None,
    };

    let value = raw_value.trim();
    if value.is_empty() && field != TextDriverField::DriverFiles {
        None
    } else {
        Some((field, value.to_string()))
    }
}

fn normalize_text_field_name(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_whitespace() && *character != '_')
        .collect::<String>()
        .to_ascii_lowercase()
}

fn matches_any(value: &str, candidates: &[&str]) -> bool {
    candidates.iter().any(|candidate| value == *candidate)
}

fn build_driver_store_path(driver_package_id: &str) -> String {
    if driver_package_id.trim().is_empty() {
        return String::new();
    }

    #[cfg(target_os = "windows")]
    {
        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        return system_root
            .join("System32")
            .join("DriverStore")
            .join("FileRepository")
            .join(driver_package_id.trim())
            .to_string_lossy()
            .to_string();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = driver_package_id;
        String::new()
    }
}

/// Append the value of an `&...;` reference to `target`.
///
/// quick-xml 0.42 reports entity and character references as standalone `GeneralRef` events
/// instead of leaving them inside the text, so they have to be resolved explicitly. Only the
/// five predefined entities and numeric character references are accepted: pnputil emits
/// nothing else, and silently dropping an unknown entity would corrupt a driver name.
fn push_general_ref(target: &mut String, reference: &str) -> Result<(), String> {
    let resolved = match reference {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        other => {
            let digits = other
                .strip_prefix('#')
                .ok_or_else(|| format!("无法识别的 XML 实体引用: &{};", other))?;
            let parsed = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16),
                None => digits.parse::<u32>(),
            };
            let code_point = parsed.map_err(|_| format!("无法解析的 XML 字符引用: &{};", other))?;
            char::from_u32(code_point).ok_or_else(|| format!("非法的 XML 字符引用: &{};", other))?
        }
    };
    target.push(resolved);
    Ok(())
}

fn read_attribute(event: &quick_xml::events::BytesStart<'_>, name: &str) -> Result<String, String> {
    for attribute in event.attributes().with_checks(false) {
        let attribute =
            attribute.map_err(|error| format!("解析 pnputil XML 属性失败: {}", error))?;
        if attribute.key.as_ref() == name {
            return attribute
                .normalized_value(XmlVersion::Implicit1_0)
                .map(|value| value.into_owned())
                .map_err(|error| format!("解析 pnputil XML 属性值失败: {}", error));
        }
    }
    Ok(String::new())
}

fn parse_driver_version(version: &str) -> Option<Vec<u64>> {
    let version_token = version.split_whitespace().last()?;
    let components = version_token
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    (!components.is_empty()).then_some(components)
}

fn compare_versions(left: &[u64], right: &[u64]) -> Ordering {
    let max_length = left.len().max(right.len());
    (0..max_length)
        .map(|index| {
            (
                left.get(index).copied().unwrap_or(0),
                right.get(index).copied().unwrap_or(0),
            )
        })
        .find_map(
            |(left_value, right_value)| match left_value.cmp(&right_value) {
                Ordering::Equal => None,
                ordering => Some(ordering),
            },
        )
        .unwrap_or(Ordering::Equal)
}

struct CommandResult {
    status_success: bool,
    output: PnputilOutput,
}

fn run_pnputil<I, S>(arguments: I) -> Result<CommandResult, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        let output = Command::new(pnputil_path())
            .args(arguments)
            // The release package is the Windows GUI subsystem; DETACHED_PROCESS forcibly detaches from the parent console,
            // with CREATE_NO_WINDOW as a fallback, to avoid pnputil creating a visible window under admin.
            .creation_flags(HIDDEN_PROCESS_FLAGS)
            .output()
            .map_err(|error| format!("启动 pnputil 失败: {}", error))?;
        return Ok(CommandResult {
            status_success: output.status.success(),
            output: PnputilOutput {
                stdout: decode_windows_output(&output.stdout),
                stderr: decode_windows_output(&output.stderr),
            },
        });
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = arguments;
        Err("旧驱动清理仅支持 Windows 系统".to_string())
    }
}

#[cfg(target_os = "windows")]
fn pnputil_path() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("pnputil.exe")
}

fn format_command_output(output: &PnputilOutput) -> String {
    let combined = format!("{} {}", output.stdout.trim(), output.stderr.trim());
    if combined.trim().is_empty() {
        "pnputil 未返回详细错误信息".to_string()
    } else if looks_like_pnputil_help(&combined) {
        "pnputil 不支持当前命令参数或 XML 输出格式，已尝试兼容模式".to_string()
    } else {
        combined.trim().to_string()
    }
}

fn looks_like_pnputil_help(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("microsoft pnp utility")
        && lower.contains("/enum-drivers")
        && (lower.contains("/format") || lower.contains("/output-file"))
}

#[cfg(target_os = "windows")]
fn decode_windows_output(raw: &[u8]) -> String {
    if raw.is_empty() {
        return String::new();
    }
    if raw.starts_with(&[0xFF, 0xFE]) {
        return String::from_utf16_lossy(
            &raw[2..]
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>(),
        );
    }
    if raw.starts_with(&[0xFE, 0xFF]) {
        return String::from_utf16_lossy(
            &raw[2..]
                .chunks_exact(2)
                .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>(),
        );
    }
    // Some Windows 10 builds redirect output without a BOM; detect UTF-16 from the NUL byte distribution first,
    // otherwise it may be mistaken for "parseable UTF-8" and the final text will contain many NULs, causing field matching to fail.
    if let Some(text) = decode_unmarked_utf16(raw) {
        return text;
    }
    if let Ok(text) = std::str::from_utf8(raw) {
        return text.to_string();
    }

    // pnputil's non-XML output usually uses the system OEM code page; call Windows NLS conversion to avoid Chinese becoming replacement characters.
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::Globalization::{
        GetOEMCP, MultiByteToWideChar, CP_ACP, CP_UTF8, MULTI_BYTE_TO_WIDE_CHAR_FLAGS,
    };
    let code_pages = [unsafe { GetOEMCP() }, CP_ACP, CP_UTF8];
    for code_page in code_pages {
        // SAFETY: raw outlives the call; passing None only queries the required length
        let wide_length =
            unsafe { MultiByteToWideChar(code_page, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), raw, None) };
        if wide_length <= 0 {
            continue;
        }
        let mut wide = vec![0u16; wide_length as usize];
        // SAFETY: wide is sized to the length reported by the first call, so the API
        // never writes past the end of the buffer
        let converted = unsafe {
            MultiByteToWideChar(
                code_page,
                MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
                raw,
                Some(&mut wide),
            )
        };
        if converted > 0 {
            return std::ffi::OsString::from_wide(&wide)
                .to_string_lossy()
                .into_owned();
        }
    }
    String::from_utf8_lossy(raw).into_owned()
}

#[cfg(target_os = "windows")]
fn decode_unmarked_utf16(raw: &[u8]) -> Option<String> {
    if raw.len() < 4 || raw.len() % 2 != 0 {
        return None;
    }

    let even_zero_count = raw.chunks_exact(2).filter(|chunk| chunk[0] == 0).count();
    let odd_zero_count = raw.chunks_exact(2).filter(|chunk| chunk[1] == 0).count();
    let pair_count = raw.len() / 2;
    let threshold = pair_count / 4;

    if odd_zero_count > even_zero_count && odd_zero_count >= threshold {
        let code_units = raw
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        return Some(String::from_utf16_lossy(&code_units));
    }

    if even_zero_count > odd_zero_count && even_zero_count >= threshold {
        let code_units = raw
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        return Some(String::from_utf16_lossy(&code_units));
    }

    None
}

#[cfg(not(target_os = "windows"))]
fn decode_windows_output(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        compare_versions, format_command_output, normalize_published_names,
        parse_device_matches_xml, parse_driver_text, parse_driver_version, parse_driver_xml,
        PnputilOutput,
    };
    use std::cmp::Ordering;
    use std::fs;
    use std::path::PathBuf;

    /// Unique temp path per call, so tests running in parallel never collide.
    fn temp_xml_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lightc-driver-{}-{}-{}.xml",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ))
    }

    #[test]
    fn parses_driver_package_xml_text_nodes() {
        // Regression guard for the quick-xml 0.38 -> 0.42 migration: 0.42 removed
        // BytesText::decode(), so text events have to be EOL-normalized and unescaped
        // explicitly. An escaped entity and a non-ASCII value are exactly what would
        // break silently if that step were dropped, and nothing else covers this path.
        let path = temp_xml_path("driver-packages");
        fs::write(
            &path,
            r#"<?xml version="1.0" encoding="utf-8"?>
<DriverPackages>
  <Driver DriverName="oem12.inf">
    <OriginalName>nvhda.inf</OriginalName>
    <ProviderName>Realtek &amp; Co.</ProviderName>
    <ClassName>音频驱动</ClassName>
    <DriverVersion>02/08/2024 6.0.1.1</DriverVersion>
    <FamilyId>0000</FamilyId>
    <SignerName>Microsoft Windows Hardware Compatibility Publisher</SignerName>
    <DriverPackageId>oem12.inf</DriverPackageId>
    <Device>
      <Status>Started</Status>
    </Device>
    <File>a.sys</File>
    <File>b.sys</File>
  </Driver>
</DriverPackages>
"#,
        )
        .expect("写入 driver XML 失败");

        let packages = parse_driver_xml(&path).expect("解析 driver XML 失败");
        let _ = fs::remove_file(&path);

        assert_eq!(packages.len(), 1);
        let package = &packages[0];
        assert_eq!(package.published_name, "oem12.inf");
        assert_eq!(package.original_name, "nvhda.inf");
        // `&amp;` must come back as a literal ampersand
        assert_eq!(package.provider_name, "Realtek & Co.");
        assert_eq!(package.class_name, "音频驱动");
        assert_eq!(package.driver_version, "02/08/2024 6.0.1.1");
        assert_eq!(package.family_id, "0000");
        assert_eq!(package.device_count, 1);
        // A device whose status is "Started" counts as in use
        assert_eq!(package.active_device_count, 1);
        assert_eq!(package.file_count, 2);
    }

    #[test]
    fn parses_device_match_xml_text_and_attribute_drivers() {
        let path = temp_xml_path("device-matches");
        fs::write(
            &path,
            r#"<?xml version="1.0" encoding="utf-8"?>
<DeviceMatches>
  <Device>
    <DriverName>oem12.inf</DriverName>
    <Status>BestRanked</Status>
    <DriverName DriverName="oem99.inf">
      <Status>Outranked</Status>
    </DriverName>
  </Device>
</DeviceMatches>
"#,
        )
        .expect("写入设备匹配 XML 失败");

        let matches = parse_device_matches_xml(&path).expect("解析设备匹配 XML 失败");
        let _ = fs::remove_file(&path);

        assert_eq!(matches.len(), 1);
        // The text-node form is the driver currently installed on the device
        assert_eq!(matches[0].installed_driver_name, "oem12.inf");
        // The attribute form under an "Outranked" status is a competing driver
        assert_eq!(
            matches[0].outranked_driver_names,
            vec!["oem99.inf".to_string()]
        );
    }

    #[test]
    fn parses_pnputil_version_with_date_prefix() {
        assert_eq!(
            parse_driver_version("02/08/2024 2406.5.5.0"),
            Some(vec![2406, 5, 5, 0])
        );
    }

    #[test]
    fn compares_versions_with_different_component_lengths() {
        assert_eq!(compare_versions(&[1, 2], &[1, 2, 0]), Ordering::Equal);
        assert_eq!(compare_versions(&[1, 3], &[1, 2, 9]), Ordering::Greater);
    }

    #[test]
    fn rejects_non_published_driver_names() {
        assert!(normalize_published_names(&["DriverStore\\oem1.inf".to_string()]).is_err());
        assert_eq!(
            normalize_published_names(&["OEM12.INF".to_string()]).unwrap(),
            vec!["oem12.inf"]
        );
    }

    #[test]
    fn parses_current_and_outranked_device_drivers() {
        let path = PathBuf::from(std::env::temp_dir()).join(format!(
            "lightc_driver_match_test_{}_{}.xml",
            std::process::id(),
            1
        ));
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<PnpUtil>
  <Device>
    <DeviceDescription>Test device</DeviceDescription>
    <DriverName>oem10.inf</DriverName>
    <MatchingDrivers>
      <DriverName DriverName="oem10.inf">
        <Status>BestRanked/Installed</Status>
      </DriverName>
      <DriverName DriverName="oem3.inf">
        <Status>Outranked</Status>
      </DriverName>
    </MatchingDrivers>
  </Device>
</PnpUtil>"#;
        fs::write(&path, xml).expect("write test XML");
        let result = parse_device_matches_xml(&path).expect("parse test XML");
        fs::remove_file(&path).expect("remove test XML");

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].installed_driver_name, "oem10.inf");
        assert_eq!(result[0].outranked_driver_names, vec!["oem3.inf"]);
    }

    #[test]
    fn parses_legacy_pnputil_text_output() {
        let text = r#"Microsoft PnP Utility

Published Name:     oem42.inf
Original Name:      example.inf
Provider Name:      Example Provider
Class Name:         System
Driver Version:     02/08/2024 1.2.3.4
Signer Name:        Example Signer
Driver Files:
    example.sys
"#;

        let packages = parse_driver_text(text);
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].published_name, "oem42.inf");
        assert_eq!(packages[0].original_name, "example.inf");
        assert_eq!(packages[0].provider_name, "Example Provider");
        assert_eq!(packages[0].file_count, 1);
    }

    #[test]
    fn parses_localized_pnputil_text_with_spaced_colons() {
        use super::{parse_text_field, TextDriverField};

        assert_eq!(
            parse_text_field("发布名称 : oem42.inf"),
            Some((TextDriverField::PublishedName, "oem42.inf".to_string()))
        );
        let text = r#"Microsoft PnP Utility
发布名称 : oem42.inf
原始名称 : example.inf
提供程序名称 : Example Provider
类名 : System
驱动程序版本 : 02/08/2024 1.2.3.4
签名者名称 : Microsoft Windows Hardware Compatibility Publisher
驱动程序文件 :
    C:\\Windows\\System32\\drivers\\example.sys
"#;

        let packages = parse_driver_text(text);
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].published_name, "oem42.inf");
        assert_eq!(packages[0].original_name, "example.inf");
        assert_eq!(packages[0].provider_name, "Example Provider");
        assert_eq!(packages[0].file_count, 1);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn decodes_unmarked_utf16_pnputil_output() {
        use super::decode_windows_output;

        let raw = "Published Name:     oem42.inf\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            decode_windows_output(&raw),
            "Published Name:     oem42.inf\r\n"
        );
    }

    #[test]
    fn shortens_pnputil_help_output() {
        let output = PnputilOutput {
            stdout: "Microsoft PnP Utility\n/enum-drivers\n/format".to_string(),
            stderr: String::new(),
        };
        assert_eq!(
            format_command_output(&output),
            "pnputil 不支持当前命令参数或 XML 输出格式，已尝试兼容模式"
        );
    }
}
