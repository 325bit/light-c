// ============================================================================
// Registry leftover scanning module
//
// [Safety statement]
//   1. Read-only scan: the scan phase never modifies any registry key/value
//   2. Hard-evidence condition: output only when all three hold — associated exe doesn't exist + non-system path + non-system process
//   3. Scope narrowing: only scan HKCR\Applications (file-association leftovers), don't touch critical system areas
//   4. Real backup: before deleting, use reg.exe export to create a recoverable .reg file
//   5. User confirmation: all deletions require explicit user selection
//
// [Why only scan HKCR\Applications]
//   Programs register file associations here at install time (e.g. "open .pdf with xxx"), and often leave residue after uninstall.
//   This is the safest and most valuable registry cleanup target — deleting the wrong thing at most loses a file association,
//   it won't affect system stability.
// ============================================================================

use chrono;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use winreg::enums::*;
use winreg::RegKey;

use super::registry_scoring::{is_definitely_safe_to_delete, PathCache, PathResolver};

// ============================================================================
// Data types
// ============================================================================

/// Scan result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryScanResult {
    pub entries: Vec<RegistryEntry>,
    pub total_count: u32,
    pub scan_duration_ms: u64,
}

/// Single leftover entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Full path under HKCR\Applications
    pub path: String,
    /// Application name (registry subkey name, e.g. "notepad.exe")
    pub name: String,
    /// Associated non-existent executable path
    pub associated_path: String,
    /// Human-readable problem description
    pub issue: String,
}

/// Deletion result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryDeleteResult {
    pub backup_path: String,
    pub deleted_count: u32,
    pub failed_entries: Vec<String>,
    pub errors: Vec<String>,
}

// ============================================================================
// Scanner
// ============================================================================

/// Registry leftover scanner
///
/// Scans all subkeys under HKCR\Applications and checks whether the executable
/// referenced by shell\open\command still exists.
pub struct RegistryScanner {
    path_cache: PathCache,
    path_resolver: PathResolver,
}

impl RegistryScanner {
    pub fn new() -> Self {
        log::info!("注册表残留扫描器已初始化");
        RegistryScanner {
            path_cache: PathCache::new(),
            path_resolver: PathResolver::new(),
        }
    }

    /// Performs the scan
    pub fn scan(&mut self) -> RegistryScanResult {
        let start_time = std::time::Instant::now();
        let mut entries = Vec::new();
        let mut seen_paths: HashSet<String> = HashSet::new(); // deduplicate by associated path

        log::info!("开始扫描注册表残留 (HKCR\\Applications)");

        let apps_key = match RegKey::predef(HKEY_CLASSES_ROOT)
            .open_subkey_with_flags("Applications", KEY_READ)
        {
            Ok(k) => k,
            Err(e) => {
                log::warn!("无法打开 HKCR\\Applications: {}", e);
                return RegistryScanResult {
                    entries: vec![],
                    total_count: 0,
                    scan_duration_ms: start_time.elapsed().as_millis() as u64,
                };
            }
        };

        for app_name in apps_key.enum_keys().filter_map(|k| k.ok()) {
            if entries.len() >= 100 {
                break;
            }

            // Read the shell\open\command default value (command-line string)
            let shell_path = format!(r"{}\shell\open\command", app_name);
            let exe_path = match apps_key.open_subkey_with_flags(&shell_path, KEY_READ) {
                Ok(cmd_key) => match cmd_key.get_value::<String, _>("") {
                    Ok(cmd) => self
                        .path_resolver
                        .extract_and_resolve(&cmd)
                        .map(|(path, _)| path.to_string_lossy().to_string()),
                    Err(_) => None,
                },
                Err(_) => None,
            };

            let exe_path = match exe_path {
                Some(p) => p,
                None => continue,
            };

            // Hard-evidence condition: path doesn't exist + non-system path + non-system process
            if !is_definitely_safe_to_delete(&exe_path, &mut self.path_cache) {
                continue;
            }

            // Deduplication: the same exe may be registered under multiple application names
            let path_lower = exe_path.to_lowercase();
            if seen_paths.contains(&path_lower) {
                continue;
            }
            seen_paths.insert(path_lower);

            entries.push(RegistryEntry {
                path: format!(r"HKEY_CLASSES_ROOT\Applications\{}", app_name),
                name: app_name,
                associated_path: exe_path.clone(),
                issue: format!("关联的可执行文件不存在: {}", exe_path),
            });
        }

        entries.sort_by(|a, b| a.name.cmp(&b.name));

        let total_count = entries.len() as u32;
        let scan_duration_ms = start_time.elapsed().as_millis() as u64;

        let (cache_hits, cache_misses) = self.path_cache.stats();
        log::info!(
            "扫描完成: {} 个残留条目, 耗时 {}ms, 缓存命中率 {:.1}%",
            total_count,
            scan_duration_ms,
            if cache_hits + cache_misses > 0 {
                (cache_hits as f64 / (cache_hits + cache_misses) as f64) * 100.0
            } else {
                0.0
            }
        );

        RegistryScanResult {
            entries,
            total_count,
            scan_duration_ms,
        }
    }
}

impl Default for RegistryScanner {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Backup
// ============================================================================

/// Registry backup manager
///
/// Uses `reg.exe export` to export a full subkey to a .reg file, recoverable by double-clicking.
///
/// Backups go to `data_dir/reg_backups/registry` (see `data_dir::registry_backup_dir`), not to the user's Documents folder:
/// the location must follow the storage directory the user picked, and it has to be visible to the data-directory
/// migration, the cleanup whitelist and the size accounting.
pub struct RegistryBackup;

impl RegistryBackup {
    /// Batch-exports registry keys to .reg files
    pub fn export_backup(entries: &[RegistryEntry], backup_dir: &Path) -> Result<PathBuf, String> {
        fs::create_dir_all(backup_dir).map_err(|e| format!("创建备份目录失败: {}", e))?;

        let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let backup_file = backup_dir.join(format!("lightc_registry_backup_{}.reg", timestamp));

        let mut file =
            File::create(&backup_file).map_err(|e| format!("创建备份文件失败: {}", e))?;

        // .reg file header
        writeln!(file, "Windows Registry Editor Version 5.00")
            .map_err(|e| format!("写入备份文件失败: {}", e))?;
        writeln!(file).map_err(|e| format!("写入备份文件失败: {}", e))?;
        writeln!(
            file,
            "; LightC 注册表备份 — {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        )
        .map_err(|e| format!("写入备份文件失败: {}", e))?;
        writeln!(file, "; 条目数: {}, 如需恢复请双击此文件", entries.len())
            .map_err(|e| format!("写入备份文件失败: {}", e))?;
        writeln!(file).map_err(|e| format!("写入备份文件失败: {}", e))?;

        for entry in entries {
            Self::export_key_via_reg_exe(&mut file, entry)?;
        }

        file.flush()
            .map_err(|e| format!("刷新备份文件失败: {}", e))?;
        log::info!("注册表备份已保存: {:?}", backup_file);

        Ok(backup_file)
    }

    /// Uses reg.exe export to export a full registry key
    fn export_key_via_reg_exe(file: &mut File, entry: &RegistryEntry) -> Result<(), String> {
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join(format!("lightc_temp_export_{}.reg", std::process::id()));

        let reg_subpath = Self::to_reg_exe_format(&entry.path)?;

        let output = Command::new("reg")
            .args(["export", &reg_subpath, &temp_file.to_string_lossy(), "/y"])
            .output()
            .map_err(|e| format!("执行 reg export 失败: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Fallback: manually record the path header
            writeln!(
                file,
                "; [回退] reg export 失败: {}. 手动记录路径",
                stderr.trim()
            )
            .map_err(|e| format!("写入备份注释失败: {}", e))?;
            writeln!(file, "[{}]", entry.path).map_err(|e| format!("写入备份路径失败: {}", e))?;
            writeln!(file, "; 此条目备份失败，请手动检查: {}", entry.issue)
                .map_err(|e| format!("写入备份注释失败: {}", e))?;
            writeln!(file).map_err(|e| format!("写入备份失败: {}", e))?;
            return Ok(());
        }

        // reg.exe outputs different encodings on different-language Windows:
        // - Chinese/Japanese/Korean Windows → UTF-16 LE + BOM (\xFF\xFE)
        // - English Windows → UTF-8 (no BOM) or ANSI
        // Read bytes and judge by BOM, compatible with all cases
        let raw_bytes = fs::read(&temp_file).map_err(|e| format!("读取临时导出文件失败: {}", e))?;
        let content = decode_reg_export(&raw_bytes)?;

        // Skip the .reg file header, append content
        let mut started = false;
        for line in content.lines() {
            if !started {
                if line.is_empty() || line.starts_with("Windows Registry Editor") {
                    continue;
                }
                started = true;
            }
            writeln!(file, "{}", line).map_err(|e| format!("写入备份内容失败: {}", e))?;
        }
        writeln!(file).map_err(|e| format!("写入备份换行失败: {}", e))?;

        let _ = fs::remove_file(&temp_file);
        Ok(())
    }

    /// Path conversion: HKEY_CLASSES_ROOT\Applications\xxx → Applications\xxx (for reg.exe)
    fn to_reg_exe_format(path: &str) -> Result<String, String> {
        if let Some(subpath) = path.strip_prefix("HKEY_CURRENT_USER\\") {
            Ok(format!("HKCU\\{}", subpath))
        } else if let Some(subpath) = path.strip_prefix("HKEY_LOCAL_MACHINE\\") {
            Ok(format!("HKLM\\{}", subpath))
        } else if let Some(subpath) = path.strip_prefix("HKEY_CLASSES_ROOT\\") {
            Ok(format!("HKCR\\{}", subpath))
        } else {
            Err(format!("无法解析注册表路径: {}", path))
        }
    }
}

// ============================================================================
// Deletion
// ============================================================================

/// Deletes a single registry entry (deletes the whole subkey)
///
/// The caller must create a backup via RegistryBackup::export_backup before calling.
pub fn delete_registry_entry(entry: &RegistryEntry) -> Result<(), String> {
    let (root_key, subpath) = parse_registry_path_components(&entry.path)?;
    let (parent_path, child_name) = split_last_component(subpath)?;

    let parent_key = root_key
        .open_subkey_with_flags(parent_path, KEY_WRITE)
        .map_err(|e| format!("打开父键失败: {}", e))?;

    parent_key
        .delete_subkey_all(child_name)
        .map_err(|e| format!("删除注册表键失败: {}", e))?;

    log::info!("已删除注册表键: {}", entry.path);
    Ok(())
}

fn parse_registry_path_components(path: &str) -> Result<(RegKey, &str), String> {
    if let Some(subpath) = path.strip_prefix("HKEY_CURRENT_USER\\") {
        Ok((RegKey::predef(HKEY_CURRENT_USER), subpath))
    } else if let Some(subpath) = path.strip_prefix("HKEY_LOCAL_MACHINE\\") {
        Ok((RegKey::predef(HKEY_LOCAL_MACHINE), subpath))
    } else if let Some(subpath) = path.strip_prefix("HKEY_CLASSES_ROOT\\") {
        Ok((RegKey::predef(HKEY_CLASSES_ROOT), subpath))
    } else {
        Err(format!("无法解析注册表路径: {}", path))
    }
}

fn split_last_component(path: &str) -> Result<(&str, &str), String> {
    path.rsplit_once('\\')
        .ok_or_else(|| format!("无法分割路径: {}", path))
}

/// Decodes the content of a .reg file exported by reg.exe
///
/// reg.exe output encoding varies with system language:
/// - CJK Windows → UTF-16 LE + BOM (0xFF 0xFE)
/// - English/other Windows → UTF-8 (with/without BOM)
/// Judge encoding by the leading BOM; without BOM, fall back to UTF-8/lossy
fn decode_reg_export(raw: &[u8]) -> Result<String, String> {
    if raw.len() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE {
        // UTF-16 LE with BOM — default output on Chinese/Japanese/Korean Windows
        let utf16: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16(&utf16).map_err(|e| format!("UTF-16 解码失败: {}", e))
    } else if raw.len() >= 3 && raw[0] == 0xEF && raw[1] == 0xBB && raw[2] == 0xBF {
        // UTF-8 with BOM
        String::from_utf8(raw[3..].to_vec()).map_err(|e| format!("UTF-8 解码失败: {}", e))
    } else {
        // No BOM, try UTF-8 first, fall back to lossy (compatible with English Windows' ANSI output)
        Ok(String::from_utf8(raw.to_vec())
            .unwrap_or_else(|_| String::from_utf8_lossy(raw).into_owned()))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scanner_creation() {
        let scanner = RegistryScanner::new();
        assert!(scanner.path_cache.stats().0 == 0);
    }

    #[test]
    fn test_to_reg_exe_format() {
        // HKCR\Applications\xxx → HKCR\Applications\xxx
        let result =
            RegistryBackup::to_reg_exe_format(r"HKEY_CLASSES_ROOT\Applications\notepad.exe");
        assert_eq!(result.unwrap(), r"HKCR\Applications\notepad.exe");
    }

    #[test]
    fn test_is_definitely_safe_with_fake_path() {
        let mut cache = PathCache::new();
        assert!(is_definitely_safe_to_delete(
            r"C:\ThisPathDoesNotExist\fake.exe",
            &mut cache
        ));
    }

    #[test]
    fn test_is_definitely_safe_system32_rejected() {
        let mut cache = PathCache::new();
        assert!(!is_definitely_safe_to_delete(
            r"C:\Windows\System32\some.exe",
            &mut cache
        ));
    }

    #[test]
    fn test_is_definitely_safe_svchost_rejected() {
        let mut cache = PathCache::new();
        assert!(!is_definitely_safe_to_delete(
            r"C:\Program Files\SomeApp\svchost.exe",
            &mut cache
        ));
    }
}
