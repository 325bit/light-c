// ============================================================================
// System info module
// Gets OS version, CPU, memory, architecture, uptime, and other system information
// ============================================================================

use serde::{Deserialize, Serialize};

/// System info structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    /// Operating system name
    pub os_name: String,
    /// Operating system version
    pub os_version: String,
    /// System architecture
    pub os_arch: String,
    /// Computer name
    pub computer_name: String,
    /// User name
    pub user_name: String,
    /// CPU information
    pub cpu_info: String,
    /// Number of CPU cores
    pub cpu_cores: u32,
    /// Total memory (bytes)
    pub total_memory: u64,
    /// Available memory (bytes)
    pub available_memory: u64,
    /// System boot time (seconds)
    pub uptime_seconds: u64,
}

/// Get the system drive letter (e.g. C). Reads the %SYSTEMDRIVE% environment variable, falling back to C on failure,
/// to cover dual-boot / custom-drive setups where the system drive is not called C.
pub fn system_drive_letter() -> char {
    std::env::var("SYSTEMDRIVE")
        .ok()
        .and_then(|drive| drive.chars().find(|ch| ch.is_ascii_alphabetic()))
        .map(|ch| ch.to_ascii_uppercase())
        .unwrap_or('C')
}

/// Get the system directory (e.g. C:\Windows). Reads the %SystemRoot% environment variable, falling back to C:\Windows on failure.
pub fn system_root_dir() -> std::path::PathBuf {
    std::env::var("SystemRoot")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
}

/// Get system information
pub fn gather() -> Result<SystemInfo, String> {
    #[cfg(target_os = "windows")]
    {
        use winapi::um::sysinfoapi::{GetSystemInfo, SYSTEM_INFO};

        let os_version = get_windows_version();
        let os_arch = get_arch_string();
        let computer_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "未知".to_string());
        let user_name = std::env::var("USERNAME").unwrap_or_else(|_| "未知".to_string());
        let cpu_info = get_cpu_info();
        let cpu_cores = unsafe {
            let mut sys_info: SYSTEM_INFO = std::mem::zeroed();
            GetSystemInfo(&mut sys_info);
            sys_info.dwNumberOfProcessors
        };
        let (total_memory, available_memory) = get_memory_info();
        let uptime_seconds = unsafe { winapi::um::sysinfoapi::GetTickCount64() / 1000 };

        Ok(SystemInfo {
            os_name: "Microsoft Windows".to_string(),
            os_version,
            os_arch,
            computer_name,
            user_name,
            cpu_info,
            cpu_cores,
            total_memory,
            available_memory,
            uptime_seconds,
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("此功能仅支持Windows系统".to_string())
    }
}

fn get_arch_string() -> String {
    if cfg!(target_arch = "x86_64") {
        "x64 (64位)".to_string()
    } else if cfg!(target_arch = "x86") {
        "x86 (32位)".to_string()
    } else if cfg!(target_arch = "aarch64") {
        "ARM64".to_string()
    } else {
        std::env::consts::ARCH.to_string()
    }
}

fn get_memory_info() -> (u64, u64) {
    #[cfg(target_os = "windows")]
    {
        use winapi::um::sysinfoapi::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        unsafe {
            let mut mem_status: MEMORYSTATUSEX = std::mem::zeroed();
            mem_status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut mem_status) != 0 {
                (mem_status.ullTotalPhys, mem_status.ullAvailPhys)
            } else {
                (0, 0)
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        (0, 0)
    }
}

#[cfg(target_os = "windows")]
fn get_windows_version() -> String {
    use winreg::enums::*;
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(key) = hklm.open_subkey("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion") {
        let product_name: String = key.get_value("ProductName").unwrap_or_default();
        let display_version: String = key.get_value("DisplayVersion").unwrap_or_default();
        let current_build: String = key.get_value("CurrentBuild").unwrap_or_default();
        let ubr: u32 = key.get_value("UBR").unwrap_or(0);

        if !product_name.is_empty() {
            // Determine whether it is Windows 11 from the build number (Build 22000+)
            let build_num: u32 = current_build.parse().unwrap_or(0);
            let corrected_name = if build_num >= 22000 && product_name.contains("Windows 10") {
                product_name.replace("Windows 10", "Windows 11")
            } else {
                product_name
            };

            let version_str = if !display_version.is_empty() {
                format!(
                    "{} {} (Build {}.{})",
                    corrected_name, display_version, current_build, ubr
                )
            } else {
                format!("{} (Build {}.{})", corrected_name, current_build, ubr)
            };
            return version_str;
        }
    }

    "Windows".to_string()
}

#[cfg(target_os = "windows")]
fn get_cpu_info() -> String {
    use winreg::enums::*;
    use winreg::RegKey;

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    if let Ok(key) = hklm.open_subkey("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0") {
        let processor_name: String = key.get_value("ProcessorNameString").unwrap_or_default();
        if !processor_name.is_empty() {
            return processor_name.trim().to_string();
        }
    }

    std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "未知处理器".to_string())
}

#[cfg(not(target_os = "windows"))]
fn get_windows_version() -> String {
    "非Windows系统".to_string()
}

#[cfg(not(target_os = "windows"))]
fn get_cpu_info() -> String {
    "未知处理器".to_string()
}
