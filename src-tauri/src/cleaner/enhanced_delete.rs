// ============================================================================
// Enhanced delete engine - handles locked files and permission issues
//
// Core features:
// 1. Take Ownership - acquire file ownership to delete protected files
// 2. Delete on Reboot - use MOVEFILE_DELAY_UNTIL_REBOOT to handle locked files
// 3. Physical size calculation - return the disk space actually freed
//
// Safety mechanisms:
// - Strict whitelist path checking; Take Ownership is only performed in safe directories
// - System-critical files can never be deleted
// - Detailed deletion result feedback, including skip reasons
// ============================================================================

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::delete_attempt::{
    attempt_delete, AttemptPolicy, AttributeClearance, DeleteOutcome, OutcomeKind, RebootPolicy,
};
use crate::error::ErrorCode;
use crate::path_guard::{ensure_deletable, DeleteScope};

// ============================================================================
// Windows API bindings
// ============================================================================

#[cfg(windows)]
pub(crate) mod windows_api {
    use std::ptr;

    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceW, GetFileAttributesW, MoveFileExW, SetFileAttributesW,
        FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_SYSTEM,
        FILE_FLAGS_AND_ATTRIBUTES, MOVEFILE_DELAY_UNTIL_REBOOT,
    };

    /// Convert a Rust string to a Windows wide string
    pub fn to_wide_string(s: &str) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Mark a file for deletion on reboot
    ///
    /// # Note
    /// This function uses the Windows API MoveFileExW with the MOVEFILE_DELAY_UNTIL_REBOOT flag
    /// to mark locked files as "delete on reboot". This is the standard way Windows Storage Sense handles stubborn files.
    ///
    /// How it works:
    /// 1. Call MoveFileExW with the target path set to NULL
    /// 2. Windows records the operation in the registry key PendingFileRenameOperations
    /// 3. At the next system startup, the deletion is performed before the user logs in
    ///
    /// Safety considerations:
    /// - Only perform this operation on files that have passed safety checks
    /// - System-critical files must never use this feature
    /// - The user is shown a prompt that "a reboot is needed to complete cleanup"
    pub fn mark_for_delete_on_reboot(path: &str) -> Result<(), String> {
        let wide_path = to_wide_string(path);

        // SAFETY: wide_path is a NUL-terminated wide string living for the call;
        // a NULL target means "delete on reboot" rather than "move"
        let result = unsafe {
            MoveFileExW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                PCWSTR::null(),
                MOVEFILE_DELAY_UNTIL_REBOOT,
            )
        };

        result.map_err(|error| format!("标记重启删除失败，{error}"))
    }

    /// Remove a file's read-only, hidden, and system attributes
    pub fn remove_protection_attributes(path: &str) -> Result<(), String> {
        let wide_path = to_wide_string(path);

        // SAFETY: wide_path is NUL-terminated and lives for both calls below
        unsafe {
            let attrs = GetFileAttributesW(PCWSTR::from_raw(wide_path.as_ptr()));
            if attrs == u32::MAX {
                return Err("无法获取文件属性".to_string());
            }

            // Remove read-only, hidden, and system attributes
            let new_attrs = attrs
                & !(FILE_ATTRIBUTE_READONLY.0 | FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_SYSTEM.0);

            if new_attrs != attrs {
                SetFileAttributesW(
                    PCWSTR::from_raw(wide_path.as_ptr()),
                    FILE_FLAGS_AND_ATTRIBUTES(new_attrs),
                )
                .map_err(|_| "无法修改文件属性".to_string())?;
            }
        }

        Ok(())
    }

    /// Grant Administrators ownership and full control over one path (M6).
    ///
    /// This replaces the two `icacls` process spawns the ownership rung used to
    /// make per path — `/setowner *S-1-5-32-544` then `/grant *S-1-5-32-544:F`.
    /// Two process creations at ~15 ms each dominated the cost of deleting a
    /// cache tree: ten thousand stubborn files meant five minutes of waiting for
    /// `CreateProcess`, not for the filesystem.
    ///
    /// The escalation itself is unchanged, and the order matters: ownership is
    /// set first because an owner holds implicit `WRITE_DAC`, which is what makes
    /// the second call (the ACE) succeed on a directory whose DACL we do not
    /// control.
    ///
    /// Three things the command line used to hide:
    ///
    /// - Privileges must be *enabled*, not merely present in the token. `icacls`
    ///   calls `AdjustTokenPrivileges` itself; a process that only carries
    ///   `SeRestorePrivilege` cannot use it. `enable_ownership_privileges` does
    ///   that once per process.
    /// - `icacls /grant` on a directory applies `(OI)(CI)`, so children inherit
    ///   the ACE even without `/T`. A file gets a non-inheriting ACE. Passing
    ///   `NO_INHERITANCE` for directories would silently narrow the grant.
    /// - The well-known SID is used instead of the name `Administrators`, which
    ///   is localized on a Chinese Windows.
    pub fn grant_administrators_full_control(path: &str) -> Result<(), String> {
        enable_ownership_privileges();
        take_administrators_ownership(path)?;
        grant_full_control_ace(path)
    }

    /// Step 1: make Administrators the owner of `path`.
    ///
    /// Naming a *group* as the owner needs `SeRestorePrivilege`, i.e. an elevated
    /// process. Unelevated, this fails with `ERROR_INVALID_OWNER` (1307) — the
    /// same answer `icacls /setowner` gives, so the escalation is no stricter than
    /// the version this replaced.
    pub(super) fn take_administrators_ownership(path: &str) -> Result<(), String> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::WIN32_ERROR;
        use windows::Win32::Security::Authorization::{SetNamedSecurityInfoW, SE_FILE_OBJECT};
        use windows::Win32::Security::OWNER_SECURITY_INFORMATION;

        let administrators = AdministratorsSid::allocate()?;
        let wide_path = to_wide_string(path);

        // SAFETY: `object` is a NUL-terminated wide string that outlives the call
        // and `administrators` is a SID owned by the guard above
        let status = unsafe {
            SetNamedSecurityInfoW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION,
                Some(administrators.0),
                None,
                None,
                None,
            )
        };

        if status != WIN32_ERROR(0) {
            return Err(format!("set owner failed (win32 {})", status.0));
        }

        Ok(())
    }

    /// Step 2: add a full-control ACE for Administrators, inheriting for a
    /// directory and not inheriting for a file.
    ///
    /// Runs after step 1, so the caller owns the object and holds implicit
    /// `WRITE_DAC`. On a file this process already owns it also works unelevated,
    /// which is what the unit test relies on.
    pub(super) fn grant_full_control_ace(path: &str) -> Result<(), String> {
        use std::ffi::c_void;
        use std::path::Path;
        use std::ptr;

        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{LocalFree, HLOCAL, WIN32_ERROR};
        use windows::Win32::Security::Authorization::{
            GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
            GRANT_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
        };
        use windows::Win32::Security::{
            ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE, PSECURITY_DESCRIPTOR,
            SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        };
        use windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

        let administrators = AdministratorsSid::allocate()?;
        let wide_path = to_wide_string(path);
        let object = PCWSTR::from_raw(wide_path.as_ptr());
        let inheritance = if Path::new(path).is_dir() {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        };

        unsafe {
            let mut old_dacl: *mut ACL = ptr::null_mut();
            let mut descriptor = PSECURITY_DESCRIPTOR(ptr::null_mut());
            // SAFETY: `object` is a NUL-terminated wide string that outlives the
            // call; every out-param lives for the whole call and the returned
            // descriptor is LocalFree'd on every path below
            let read_status = GetNamedSecurityInfoW(
                object,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(&mut old_dacl),
                None,
                &mut descriptor,
            );
            if read_status != WIN32_ERROR(0) {
                return Err(format!("read the DACL failed (win32 {})", read_status.0));
            }

            let trustee = TRUSTEE_W {
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
                ptstrName: windows::core::PWSTR(administrators.0 .0 as *mut u16),
                ..Default::default()
            };

            let entry = EXPLICIT_ACCESS_W {
                grfAccessPermissions: FILE_ALL_ACCESS.0,
                grfAccessMode: GRANT_ACCESS,
                grfInheritance: inheritance,
                Trustee: trustee,
            };

            let mut new_dacl: *mut ACL = ptr::null_mut();
            // SAFETY: `old_dacl` belongs to `descriptor`; the new ACL is
            // API-allocated and freed on every path below
            let acl_status = SetEntriesInAclW(Some(&[entry]), Some(old_dacl), &mut new_dacl);
            if acl_status != WIN32_ERROR(0) {
                LocalFree(Some(HLOCAL(descriptor.0)));
                return Err(format!("build the ACE failed (win32 {})", acl_status.0));
            }

            // SAFETY: `object` is still valid; `new_dacl` is the ACL built above
            // and is copied into the object's security descriptor by this call
            let grant_status = SetNamedSecurityInfoW(
                object,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(new_dacl),
                None,
            );
            LocalFree(Some(HLOCAL(new_dacl as *mut c_void)));
            LocalFree(Some(HLOCAL(descriptor.0)));

            if grant_status != WIN32_ERROR(0) {
                return Err(format!("grant failed (win32 {})", grant_status.0));
            }
        }

        Ok(())
    }

    /// `BUILTIN\Administrators` (S-1-5-32-544), freed on drop.
    ///
    /// A guard rather than a bare `PSID` because both steps above have three
    /// early returns each, and a manual `FreeSid` at every one of them is where
    /// leaks get introduced. `windows` 0.62 exports the two RIDs as `i32` from
    /// `Win32::System::SystemServices`, a feature this crate does not enable; two
    /// local integers are cheaper than a feature.
    struct AdministratorsSid(windows::Win32::Security::PSID);

    impl AdministratorsSid {
        fn allocate() -> Result<Self, String> {
            use std::ptr;

            use windows::Win32::Security::{AllocateAndInitializeSid, PSID, SECURITY_NT_AUTHORITY};

            const SECURITY_BUILTIN_DOMAIN_RID: u32 = 32;
            const DOMAIN_ALIAS_RID_ADMINS: u32 = 544;

            let mut sid = PSID(ptr::null_mut());
            // SAFETY: out-param only; the SID is freed by `Drop`
            let built = unsafe {
                AllocateAndInitializeSid(
                    &SECURITY_NT_AUTHORITY,
                    2,
                    SECURITY_BUILTIN_DOMAIN_RID,
                    DOMAIN_ALIAS_RID_ADMINS,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    &mut sid,
                )
            };

            if built.is_err() {
                // SAFETY: `GetLastError` has no preconditions; it reads the
                // calling thread's last-error slot
                let code = unsafe { windows::Win32::Foundation::GetLastError().0 };
                return Err(format!(
                    "could not build the Administrators SID (win32 {code})"
                ));
            }

            Ok(Self(sid))
        }
    }

    impl Drop for AdministratorsSid {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from AllocateAndInitializeSid and is dropped once
            unsafe { windows::Win32::Security::FreeSid(self.0) };
        }
    }

    /// Enable the two privileges the ownership step needs, once per process.
    ///
    /// Best effort on purpose: a non-elevated process does not hold them, and
    /// `AdjustTokenPrivileges` then reports `ERROR_NOT_ALL_ASSIGNED`. That is not
    /// an error worth reporting here — the security call that follows fails with
    /// `access denied`, which names the real problem.
    fn enable_ownership_privileges() {
        use std::sync::OnceLock;

        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::Security::{
            AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES,
            SE_PRIVILEGE_ENABLED, SE_RESTORE_NAME, SE_TAKE_OWNERSHIP_NAME, TOKEN_ADJUST_PRIVILEGES,
            TOKEN_PRIVILEGES, TOKEN_QUERY,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        static ONCE: OnceLock<()> = OnceLock::new();

        ONCE.get_or_init(|| unsafe {
            let mut token = HANDLE::default();
            // SAFETY: pseudo-handle for the current process; `token` is closed below
            if OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            )
            .is_err()
            {
                return;
            }

            for name in [SE_TAKE_OWNERSHIP_NAME, SE_RESTORE_NAME] {
                let mut luid = Default::default();
                // SAFETY: `name` is a static wide string; `luid` is the out-param
                if LookupPrivilegeValueW(None, name, &mut luid).is_err() {
                    continue;
                }
                let state = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                // SAFETY: `token` is valid; `state` outlives the call
                let _ = AdjustTokenPrivileges(token, false, Some(&state), 0, None, None);
            }

            // SAFETY: `token` came from OpenProcessToken and is not used after this
            let _ = CloseHandle(token);
        });
    }

    /// Get the disk cluster size (used to calculate physical usage)
    pub fn get_cluster_size(root_path: &str) -> Option<u32> {
        let wide_path = to_wide_string(root_path);
        let mut sectors_per_cluster: u32 = 0;
        let mut bytes_per_sector: u32 = 0;
        let mut free_clusters: u32 = 0;
        let mut total_clusters: u32 = 0;

        // SAFETY: wide_path is NUL-terminated and the four u32 out-params live
        // for the whole duration of the call
        unsafe {
            GetDiskFreeSpaceW(
                PCWSTR::from_raw(wide_path.as_ptr()),
                Some(&mut sectors_per_cluster),
                Some(&mut bytes_per_sector),
                Some(&mut free_clusters),
                Some(&mut total_clusters),
            )
            .ok()
            .map(|_| sectors_per_cluster * bytes_per_sector)
        }
    }
}

// ============================================================================
// Deletion result types
// ============================================================================

/// Classification of deletion failure reasons
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DeleteFailureReason {
    /// File does not exist
    NotFound,
    /// Insufficient permissions
    PermissionDenied,
    /// File is locked (in use)
    FileLocked,
    /// System-protected file
    SystemProtected,
    /// Path outside the allowed scope
    OutOfScope,
    /// Already marked for deletion on reboot
    MarkedForReboot,
    /// Other error
    Other(String),
}

impl DeleteFailureReason {
    /// Get a user-friendly Chinese description
    pub fn display_message(&self) -> &str {
        match self {
            Self::NotFound => "文件不存在",
            Self::PermissionDenied => "权限不足",
            Self::FileLocked => "文件被系统占用",
            Self::SystemProtected => "系统保护文件",
            Self::OutOfScope => "不在清理范围内",
            Self::MarkedForReboot => "已标记重启后删除",
            Self::Other(_) => "删除失败",
        }
    }

    /// Get detailed hint text (for tooltips)
    pub fn tooltip(&self) -> &str {
        match self {
            Self::NotFound => "该文件可能已被其他程序删除",
            Self::PermissionDenied => "需要管理员权限才能删除此文件",
            Self::FileLocked => "该文件正被系统或其他程序使用，将在重启后删除",
            Self::SystemProtected => "这是系统关键文件，删除可能导致系统不稳定",
            Self::OutOfScope => "该文件不在安全清理范围内",
            Self::MarkedForReboot => "文件已标记，将在下次重启时自动删除",
            Self::Other(msg) => msg.as_str(),
        }
    }
}

/// Deletion result for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDeleteResult {
    /// File path
    pub path: String,
    /// Whether deletion succeeded
    pub success: bool,
    /// Logical size (the file's actual content size)
    pub logical_size: u64,
    /// Physical size (actual disk space occupied, cluster-aligned)
    pub physical_size: u64,
    /// Failure reason (if failed)
    pub failure_reason: Option<DeleteFailureReason>,
    /// Whether marked for deletion on reboot
    pub marked_for_reboot: bool,
}

/// Enhanced deletion result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedDeleteResult {
    /// Number of files successfully deleted
    pub success_count: usize,
    /// Number of files that failed
    pub failed_count: usize,
    /// Number of files marked for deletion on reboot
    pub reboot_pending_count: usize,
    /// Physical space actually freed (bytes)
    pub freed_physical_size: u64,
    /// Total logical size (for comparison)
    pub freed_logical_size: u64,
    /// Size of skipped files (could not be deleted due to errors)
    pub skipped_size: u64,
    /// Detailed per-file deletion results
    pub file_results: Vec<FileDeleteResult>,
    /// Whether a reboot is needed to complete cleanup
    pub needs_reboot: bool,
    /// Summary message (WeChat style)
    pub summary_message: String,
    /// Warning messages during cleanup (e.g. a downgrade caused by an expired deep-scan session); None for a normal deletion.
    pub warning: Option<String>,
}

/// Batch progress of the enhanced deletion process.
///
/// Progress only aggregates the current batch's statistics and carries no file details, to avoid frequently transferring large objects to the WebView during bulk deletion.
#[derive(Debug, Clone, Serialize)]
pub struct EnhancedDeleteProgress {
    /// Current phase; the delete engine currently uses cleaning.
    pub phase: String,
    /// Number of files processed.
    pub processed_count: usize,
    /// Total number of deduplicated files in this deletion.
    pub total_count: usize,
    /// Number of files successfully deleted.
    pub success_count: usize,
    /// Number of files that failed.
    pub failed_count: usize,
    /// Number of files marked for deletion on reboot.
    pub reboot_pending_count: usize,
    /// Physical space confirmed freed so far.
    pub freed_physical_size: u64,
    /// Time elapsed since the delete engine started.
    pub elapsed_ms: u64,
}

/// Maximum interval between progress events, so the UI keeps responding even when a single batch is slow.
const DELETE_PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
/// Normal batch progress interval, avoiding the overhead of sending an IPC event for every file.
const DELETE_PROGRESS_BATCH_SIZE: usize = 500;
/// 单一进度汇报者的唤醒周期：读原子计数，按双阈值决定是否发射（回收站删除提速方案 §5.2）。
const RECYCLE_PROGRESS_REPORTER_TICK: Duration = Duration::from_millis(100);
/// 回收站并行 worker 数上限；环境变量超出上限时截断到这里（§5.1）。
const RECYCLE_MAX_WORKERS: usize = 32;
/// 默认 worker 数的封顶值：min(逻辑核数, 8) 只是初始默认，不是已论证的最优值（真机矩阵待测）。
const RECYCLE_DEFAULT_WORKER_CAP: usize = 8;
/// 覆盖回收站批并行度的环境变量，供真机 1/2/4/8/16/32 矩阵实测（§5.1）。
const LIGHTC_RECYCLE_DELETE_WORKERS: &str = "LIGHTC_RECYCLE_DELETE_WORKERS";

impl EnhancedDeleteResult {
    pub fn new() -> Self {
        Self {
            success_count: 0,
            failed_count: 0,
            reboot_pending_count: 0,
            freed_physical_size: 0,
            freed_logical_size: 0,
            skipped_size: 0,
            file_results: Vec::new(),
            needs_reboot: false,
            summary_message: String::new(),
            warning: None,
        }
    }

    /// Count one outcome.
    ///
    /// S3c: this used to be four copies of the same three-branch bookkeeping
    /// spread over the batch loop. The rules it encodes are documented once in
    /// `delete_attempt` — in particular that a reboot-pending item is not a
    /// success and contributes nothing to the freed total.
    fn record(&mut self, kind: OutcomeKind, logical_size: u64, physical_size: u64) {
        match kind {
            OutcomeKind::Freed => {
                self.success_count += 1;
                self.freed_logical_size += logical_size;
                self.freed_physical_size += physical_size;
            }
            OutcomeKind::RebootPending => {
                self.reboot_pending_count += 1;
                self.needs_reboot = true;
            }
            OutcomeKind::Failed => {
                self.failed_count += 1;
                self.skipped_size += physical_size;
            }
        }
    }

    /// Generate a WeChat-style summary message
    pub fn generate_summary(&mut self) {
        let freed_mb = self.freed_physical_size as f64 / 1024.0 / 1024.0;
        let skipped_mb = self.skipped_size as f64 / 1024.0 / 1024.0;

        let mut parts = Vec::new();

        // Successfully freed portion
        if self.freed_physical_size > 0 {
            if freed_mb >= 1024.0 {
                parts.push(format!("成功释放 {:.1} GB", freed_mb / 1024.0));
            } else if freed_mb >= 1.0 {
                parts.push(format!("成功释放 {:.1} MB", freed_mb));
            } else {
                parts.push(format!("成功释放 {:.0} KB", freed_mb * 1024.0));
            }
        }

        // The skipped portion cannot be uniformly described as "in use by the system"; Shell API failures in the recycle bin and permission issues also land here.
        if self.skipped_size > 0 {
            if skipped_mb >= 1.0 {
                parts.push(format!("{:.1} MB 跳过", skipped_mb));
            } else {
                parts.push(format!("{:.0} KB 跳过", skipped_mb * 1024.0));
            }
        }

        // Portion pending deletion on reboot
        if self.reboot_pending_count > 0 {
            parts.push(format!(
                "{} 个文件将在重启后删除",
                self.reboot_pending_count
            ));
        }

        self.summary_message = if parts.is_empty() {
            "没有文件被清理".to_string()
        } else {
            parts.join("，")
        };
    }
}

impl Default for EnhancedDeleteResult {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Enhanced delete engine
// ============================================================================

/// Enhanced delete engine
pub struct EnhancedDeleteEngine {
    /// Disk cluster size cache
    cluster_size: u32,
    /// Cache cluster size per volume when cleaning multiple partitions, avoiding a Windows API call per file.
    cluster_sizes: Mutex<HashMap<String, u32>>,
    /// Whether deletion on reboot is enabled
    enable_reboot_delete: bool,
    /// Whether to attempt taking ownership
    enable_take_ownership: bool,
}

impl EnhancedDeleteEngine {
    /// Create a new enhanced delete engine
    pub fn new() -> Self {
        // Get the C drive cluster size, defaulting to 4096 bytes
        let cluster_size = windows_api::get_cluster_size("C:\\").unwrap_or(4096);

        Self {
            cluster_size,
            cluster_sizes: Mutex::new(HashMap::new()),
            enable_reboot_delete: true, // enabled by default, handles occupied files
            // Off by default: taking ownership rewrites an ACL the user did not
            // ask to change, so only the deep-clean entry point opts in.
            enable_take_ownership: false,
        }
    }

    /// Set whether deletion on reboot is enabled
    pub fn with_reboot_delete(mut self, enabled: bool) -> Self {
        self.enable_reboot_delete = enabled;
        self
    }

    /// Set whether taking ownership is enabled
    pub fn with_take_ownership(mut self, enabled: bool) -> Self {
        self.enable_take_ownership = enabled;
        self
    }

    /// Calculate a file's physical usage (cluster-aligned)
    ///
    /// # Note
    /// A file's actual space on disk is allocated in clusters.
    /// For example, a 1-byte file actually occupies 4KB on a disk with a 4KB cluster size.
    /// This function returns the disk space the file actually frees, not its content size.
    pub fn calculate_physical_size(&self, logical_size: u64) -> u64 {
        if logical_size == 0 {
            return 0;
        }
        let cluster_size = self.cluster_size as u64;
        ((logical_size + cluster_size - 1) / cluster_size) * cluster_size
    }

    /// Calculate physical usage by the file's partition, avoiding applying the C drive cluster size when deeply cleaning D/E drives.
    fn calculate_physical_size_for_path(&self, path: &Path, logical_size: u64) -> u64 {
        let Some(drive_root) = crate::sys_drive::drive_root_of(path) else {
            return self.calculate_physical_size(logical_size);
        };
        let cluster_size = self.cluster_size_for_drive(&drive_root);
        align_physical_size(logical_size, cluster_size)
    }

    fn cluster_size_for_drive(&self, drive_root: &str) -> u32 {
        if let Ok(cache) = self.cluster_sizes.lock() {
            if let Some(cluster_size) = cache.get(drive_root) {
                return *cluster_size;
            }
        }

        let cluster_size = windows_api::get_cluster_size(drive_root).unwrap_or(self.cluster_size);
        if let Ok(mut cache) = self.cluster_sizes.lock() {
            cache.insert(drive_root.to_string(), cluster_size);
        }
        cluster_size
    }

    /// Delete a list of files
    pub fn delete_files(&self, paths: &[String]) -> EnhancedDeleteResult {
        // Keep the original public interface so other modules can reuse the delete engine without handling progress events.
        self.delete_files_with_progress(paths, |_| {})
    }

    /// Delete a list of files and report progress in batches.
    ///
    /// The callback runs on the delete thread; the caller may only do lightweight notifications and must not perform file IO there.
    ///
    /// 回收站批次改为有界并行后（提速方案 §5.2），回调会被"单一进度汇报者
    /// 线程"与主线程先后调用，因此要求 `F: Send` 并在内部以 Mutex 串行化——
    /// 任意时刻只有一个调用方（汇报者线程在 install 内阻塞等待期间，主线程
    /// 不碰回调），锁无实际竞争。调用方的闭包捕获 AppHandle（Send + Sync），
    /// 天然满足新边界。
    pub fn delete_files_with_progress<F>(
        &self,
        paths: &[String],
        on_progress: F,
    ) -> EnhancedDeleteResult
    where
        F: FnMut(EnhancedDeleteProgress) + Send,
    {
        let mut result = EnhancedDeleteResult::new();
        let total_count = paths.len();
        let started_at = Instant::now();
        let mut processed_count = 0usize;
        let mut last_progress_at = Instant::now();

        // 回调的跨线程共享入口（§5.2）：所有发射都经这一把 Mutex 串行化。
        let on_progress = Mutex::new(on_progress);
        let emit_now = |progress: EnhancedDeleteProgress| {
            let mut callback =
                on_progress.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            callback(progress);
        };

        // Push the cleaning phase once when the delete task first enters the execution thread, so small tasks do not appear to have no progress.
        emit_now(EnhancedDeleteProgress {
            phase: "cleaning".to_string(),
            processed_count: 0,
            total_count,
            success_count: 0,
            failed_count: 0,
            reboot_pending_count: 0,
            freed_physical_size: 0,
            elapsed_ms: 0,
        });

        // Progress events carry only aggregated data, avoiding slowing down the actual cleanup during bulk deletion.
        let mut emit_progress = |processed: usize, current_result: &EnhancedDeleteResult| {
            let should_emit = processed == total_count
                || processed.saturating_sub(1) % DELETE_PROGRESS_BATCH_SIZE == 0
                || last_progress_at.elapsed() >= DELETE_PROGRESS_INTERVAL;
            if !should_emit {
                return;
            }

            emit_now(EnhancedDeleteProgress {
                phase: "cleaning".to_string(),
                processed_count: processed,
                total_count,
                success_count: current_result.success_count,
                failed_count: current_result.failed_count,
                reboot_pending_count: current_result.reboot_pending_count,
                freed_physical_size: current_result.freed_physical_size,
                elapsed_ms: started_at.elapsed().as_millis() as u64,
            });
            last_progress_at = Instant::now();
        };

        info!("增强删除引擎：开始删除 {} 个文件", paths.len());

        // Separate recycle bin paths: they are deleted entry by entry, not via SHEmptyRecycleBinW.
        // H1 fix: SHEmptyRecycleBinW empties an entire volume — selecting 3 entries wiped every
        // other item on that drive, silently and unrecoverably. The scanner only surfaces entries
        // under the current user's own SID folder, where the owning user holds Full Control, so
        // removing the $R/$I pair directly needs no SYSTEM privileges.
        // C1 fix: normalize first, so `C:/...` or `\\?\...` recycle entries are still classified correctly.
        // D3 fix: go through path_guard::compare_key (the shared "normalize then
        // ASCII-lowercase" form) instead of a second local normalize + Unicode
        // to_lowercase spelling; `$RECYCLE.BIN` case variants still match
        // because the lowercasing is applied to the whole normalized path.
        let (recycle_paths, normal_paths): (Vec<&String>, Vec<&String>) = paths
            .iter()
            .partition(|p| crate::path_guard::compare_key(Path::new(p)).contains("\\$recycle.bin"));

        // Recycle entries are deleted with bounded parallelism (回收站删除提速方案 §5.1):
        // one complete $R → $I entry is one scheduling unit, cross-drive flattened into one
        // par_iter; a bad volume or a foreign-SID entry fails alone and never drags the
        // others down. Still not SHEmptyRecycleBinW (H1): its semantics is "empty the
        // whole volume", which silently wipes entries the user did not select.
        if !recycle_paths.is_empty() {
            info!(
                "检测到 {} 个回收站条目，有界并行删除 $R/$I 对",
                recycle_paths.len()
            );

            // Sizes are read inside the worker (the paths would report 0 bytes after
            // deletion), merged into the parallel pass instead of the old serial
            // pre-scan: §8.1's baseline measured the $I read alone at 26% of the
            // whole batch, so paying it serially first would cap the speedup.
            //
            // Cluster sizes are pre-computed per volume into an immutable map:
            // cluster_size_for_drive takes two lock() calls per entry, which under
            // parallelism would reintroduce exactly the serialization this pass
            // removes (§5.1). Workers never touch the engine's mutex.
            let mut drive_cluster: HashMap<String, u32> = HashMap::new();
            for path in &recycle_paths {
                if let Some(root) = recycle_drive_root(path) {
                    // entry + clone（盘根仅 3 字符，命中路径的克隆开销可忽略）：
                    // `or_insert_with` 闭包还要借用 `root` 做查询，直接 move 会
                    // E0382；`contains_key` + `insert` 则触发 clippy::map_entry。
                    drive_cluster
                        .entry(root.clone())
                        .or_insert_with(|| self.cluster_size_for_drive(&root));
                }
            }

            let workers = resolve_recycle_worker_count();
            info!(
                "回收站批次并行度: {} workers（可用 {} 环境变量覆盖；1/2/4/8/16/32 矩阵待真机实测）",
                workers, LIGHTC_RECYCLE_DELETE_WORKERS
            );
            let pool = match rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .thread_name(|index| format!("recycle-del-{index}"))
                .build()
            {
                Ok(pool) => Some(pool),
                // §5.1: a pool-creation failure fails the batch explicitly per entry —
                // never a silent fall-back onto the global rayon pool, which scan
                // tasks share.
                Err(error) => {
                    warn!("回收站并行线程池创建失败，本批条目将显式失败: {}", error);
                    None
                }
            };

            let counters = RecycleCounters::default();
            let recycle_started = Instant::now();
            // 停止旗标放在 thread::scope 之外：声明在外层函数作用域里，
            // 汇报者线程的借用生命周期才成立（scope 内声明的局部量借用会被
            // spawn 的非 move 闭包捕获而报 E0373）。
            let stop_reporter = AtomicBool::new(false);

            let outcomes: Vec<RecycleEntryOutcome> = std::thread::scope(|scope| {
                // §5.2: single progress reporter. Workers only fetch_add the atomic
                // counters and never touch the UI callback; the reporter wakes on a
                // fixed tick, reads the counters, and emits under the existing
                // 500-entries / 500-ms dual threshold — the count threshold compares
                // against the completed snapshot of the last emission, so progress
                // always moves forward and always reaches 100% (completed =
                // succeeded + failed).
                let reporter = scope.spawn(|| {
                    let mut last_emit = Instant::now();
                    let mut last_emitted_completed = 0usize;
                    loop {
                        thread::sleep(RECYCLE_PROGRESS_REPORTER_TICK);
                        if stop_reporter.load(Ordering::Acquire) {
                            return;
                        }
                        let completed = counters.completed.load(Ordering::Relaxed);
                        let due = completed.saturating_sub(last_emitted_completed)
                            >= DELETE_PROGRESS_BATCH_SIZE
                            || (completed > 0 && last_emit.elapsed() >= DELETE_PROGRESS_INTERVAL);
                        if !due {
                            continue;
                        }
                        last_emit = Instant::now();
                        last_emitted_completed = completed;
                        emit_now(recycle_progress_snapshot(
                            &counters,
                            total_count,
                            started_at,
                        ));
                    }
                });

                let outcomes = match &pool {
                    Some(pool) => pool.install(|| {
                        recycle_paths
                            .par_iter()
                            .map(|path| self.recycle_entry_worker(path, &drive_cluster, &counters))
                            .collect::<Vec<RecycleEntryOutcome>>()
                    }),
                    None => recycle_paths
                        .iter()
                        .map(|path| self.recycle_entry_pool_unavailable(path, &counters))
                        .collect(),
                };

                stop_reporter.store(true, Ordering::Release);
                let _ = reporter.join();
                outcomes
            });

            // §5.3: phase timings are aggregated in memory from the collected
            // outcomes — no measurement I/O is added anywhere.
            let timings = summarize_recycle_timings(&outcomes);
            log_recycle_phase_timings(
                &timings,
                recycle_paths.len(),
                recycle_started.elapsed(),
                workers,
            );

            // Aggregation runs on the main thread in request order: `record`'s
            // bucket rules stay the serial version's, and file_results order is
            // the original request order (stronger than the old grouping's
            // drive-ascending order; the UI depends on nothing beyond stability).
            apply_recycle_outcomes(&mut result, outcomes, &mut processed_count);

            // §5.5: data-freed counts every entry whose $R blob left the disk,
            // including ones whose $I twin failed afterwards — those entries are
            // failures for the front end but their data is gone regardless.
            info!(
                "回收站批次: 完整成功 {} 条（{} 字节），数据已释放量 {} 字节（含 $I 删除失败的条目）",
                result.success_count, result.freed_physical_size, counters.data_freed.load(Ordering::Relaxed)
            );

            // The reporter is stopped, so force-send one corrected final progress
            // built from the ordered results — failure entries included, the UI's
            // processed count must reach total_count (§5.2).
            emit_now(EnhancedDeleteProgress {
                phase: "cleaning".to_string(),
                processed_count,
                total_count,
                success_count: result.success_count,
                failed_count: result.failed_count,
                reboot_pending_count: result.reboot_pending_count,
                freed_physical_size: result.freed_physical_size,
                elapsed_ms: started_at.elapsed().as_millis() as u64,
            });
        }

        // Delete normal files one by one
        for path in normal_paths {
            // The bucket comes back from `delete_single_file`; the loop no longer
            // reconstructs it from `failure_reason`.
            let (file_result, kind) = self.delete_single_file(path);
            result.record(kind, file_result.logical_size, file_result.physical_size);
            result.file_results.push(file_result);
            processed_count += 1;
            emit_progress(processed_count, &result);
        }

        result.generate_summary();

        info!(
            "增强删除完成: 成功 {}, 失败 {}, 待重启 {}, 释放 {} 字节",
            result.success_count,
            result.failed_count,
            result.reboot_pending_count,
            result.freed_physical_size
        );

        result
    }

    /// One recycle-bin entry = one complete scheduling unit (§3 约束 3)：尺寸读取、
    /// 校验、`$R → $I` 删除顺序全部发生在本函数内部；worker 之间除原子计数外
    /// 无共享可变状态。簇大小来自预计算的不可变 map（§5.1），不触碰
    /// `cluster_sizes` 的 Mutex——那把锁每条要 lock 两次，并行下会重新引入
    /// 串行点。
    fn recycle_entry_worker(
        &self,
        path: &str,
        drive_cluster: &HashMap<String, u32>,
        counters: &RecycleCounters,
    ) -> RecycleEntryOutcome {
        let read_started = Instant::now();
        let logical_size = self.get_file_size(Path::new(path));
        let metadata_read_ms = read_started.elapsed().as_millis() as u64;

        let drive_root = recycle_drive_root(path);
        // 缺少有效盘符的路径保持原口径：不尝试删除，物理大小按默认簇对齐，
        // 记为失败（进度总数仍然收敛）。
        let Some(drive_root) = drive_root else {
            let outcome = RecycleEntryOutcome {
                path: path.to_string(),
                logical_size,
                physical_size: self.calculate_physical_size(logical_size),
                kind: OutcomeKind::Failed,
                failure_reason: Some(DeleteFailureReason::Other(
                    "回收站路径缺少有效盘符".to_string(),
                )),
                is_dir_entry: Path::new(path).is_dir(),
                drive_root: None,
                data_freed: false,
                metadata_read_ms,
                data_delete_ms: 0,
                metadata_delete_ms: 0,
            };
            counters.record(&outcome);
            return outcome;
        };

        let physical_size = align_physical_size(
            logical_size,
            drive_cluster
                .get(&drive_root)
                .copied()
                .unwrap_or(self.cluster_size),
        );

        let deletion = delete_recycle_entry(path);
        if let Err(error) = &deletion.outcome {
            warn!("回收站条目删除失败: {}", error);
        }
        let (kind, failure_reason) = match &deletion.outcome {
            Ok(()) => (OutcomeKind::Freed, None),
            Err(error) => (
                OutcomeKind::Failed,
                Some(DeleteFailureReason::Other(error.clone())),
            ),
        };
        let outcome = RecycleEntryOutcome {
            path: path.to_string(),
            logical_size,
            physical_size,
            kind,
            failure_reason,
            is_dir_entry: deletion.is_dir_entry,
            drive_root: Some(drive_root),
            data_freed: deletion.data_freed,
            metadata_read_ms,
            data_delete_ms: deletion.data_delete_ms,
            metadata_delete_ms: deletion.metadata_delete_ms,
        };
        counters.record(&outcome);
        outcome
    }

    /// 线程池创建失败的显式失败路径（§5.1）：条目不执行任何删除动作，
    /// 也不静默回退到全局 rayon 池（避免与扫描任务争抢线程）。
    fn recycle_entry_pool_unavailable(
        &self,
        path: &str,
        counters: &RecycleCounters,
    ) -> RecycleEntryOutcome {
        let outcome = RecycleEntryOutcome {
            path: path.to_string(),
            logical_size: 0,
            physical_size: 0,
            kind: OutcomeKind::Failed,
            failure_reason: Some(DeleteFailureReason::Other(
                "回收站并行线程池创建失败，本批未执行".to_string(),
            )),
            is_dir_entry: false,
            drive_root: recycle_drive_root(path),
            data_freed: false,
            metadata_read_ms: 0,
            data_delete_ms: 0,
            metadata_delete_ms: 0,
        };
        counters.record(&outcome);
        outcome
    }

    /// Delete a single file.
    ///
    /// Returns the bucket alongside the result so the batch loop does not have to
    /// reconstruct it. It used to `match &file_result.failure_reason`, i.e. derive
    /// "was this a success" from a field that exists for the tooltip.
    fn delete_single_file(&self, path: &str) -> (FileDeleteResult, OutcomeKind) {
        let file_path = Path::new(path);

        // Get the file size
        let logical_size = self.get_file_size(file_path);
        let physical_size = self.calculate_physical_size_for_path(file_path, logical_size);

        // S3a: the traversal gate, the protection table and the scope layers are one
        // call now. `EnhancedClean` skips the junk allowlist and the
        // `\windows\` extension layer, because the deep scanner deliberately
        // surfaces installers under `C://Windows//Temp`.
        if let Err(error) = ensure_deletable(file_path, DeleteScope::EnhancedClean) {
            let reason = match error.code {
                ErrorCode::Protected => DeleteFailureReason::SystemProtected,
                _ => DeleteFailureReason::OutOfScope,
            };
            return (
                FileDeleteResult {
                    path: path.to_string(),
                    success: false,
                    logical_size,
                    physical_size,
                    failure_reason: Some(reason),
                    marked_for_reboot: false,
                },
                OutcomeKind::Failed,
            );
        }

        // Check whether the file exists
        if !file_path.exists() {
            return (
                FileDeleteResult {
                    path: path.to_string(),
                    success: false,
                    logical_size,
                    physical_size,
                    failure_reason: Some(DeleteFailureReason::NotFound),
                    marked_for_reboot: false,
                },
                OutcomeKind::Failed,
            );
        }

        // S3b: the ladder (attributes -> ownership -> reboot queue) is one call
        // now. The policy below keeps this engine's product decisions: attribute
        // clearing always, ownership only inside the whitelist and only when the
        // user allowed it (or for rebuildable caches, which the cleanup wizard
        // deletes anyway), reboot queue behind that same whitelist.
        let outcome = attempt_delete(
            file_path,
            DeleteScope::EnhancedClean,
            &self.attempt_policy(),
        );

        let result = match &outcome {
            DeleteOutcome::Freed => {
                debug!("成功删除: {}", path);
                FileDeleteResult {
                    path: path.to_string(),
                    success: true,
                    logical_size,
                    physical_size,
                    failure_reason: None,
                    marked_for_reboot: false,
                }
            }
            DeleteOutcome::RebootPending => {
                info!("文件已标记为重启删除: {}", path);
                FileDeleteResult {
                    path: path.to_string(),
                    success: false,
                    logical_size,
                    physical_size,
                    failure_reason: Some(DeleteFailureReason::MarkedForReboot),
                    marked_for_reboot: true,
                }
            }
            DeleteOutcome::Failed(error) => {
                // The code picks the reason. `Other` only carries the English
                // diagnostic for the tooltip, so no sentence is assembled here —
                // this used to be `Other(format!("权限不足：{}", message))`.
                let reason = match error.code {
                    ErrorCode::TargetInUse => DeleteFailureReason::FileLocked,
                    ErrorCode::PermissionDenied => DeleteFailureReason::PermissionDenied,
                    ErrorCode::NotFound => DeleteFailureReason::NotFound,
                    _ => DeleteFailureReason::Other(error.message.clone()),
                };
                FileDeleteResult {
                    path: path.to_string(),
                    success: false,
                    logical_size,
                    physical_size,
                    failure_reason: Some(reason),
                    marked_for_reboot: false,
                }
            }
        };

        (result, outcome.kind())
    }

    /// This engine's rung selection; see `delete_attempt::AttemptPolicy`.
    ///
    /// `ownership_for_caches` reproduces the old condition
    /// `is_safe_for_ownership && (enable_take_ownership || is_cache_path)`: the
    /// cache exemption widens the toggle, it never bypasses the whitelist.
    fn attempt_policy(&self) -> AttemptPolicy {
        AttemptPolicy {
            clear_attributes: AttributeClearance::Always,
            take_ownership: self.enable_take_ownership,
            ownership_for_caches: true,
            reboot: if self.enable_reboot_delete {
                RebootPolicy::OwnershipWhitelist
            } else {
                RebootPolicy::Off
            },
        }
    }

    /// Get the file size
    fn get_file_size(&self, path: &Path) -> u64 {
        // A recycle bin entry's displayed size comes from $I metadata; a directory must not be counted by only its direct $R children.
        if let Some(logical_size) = get_recycle_metadata_size(path) {
            return logical_size;
        }

        if path.is_file() {
            fs::metadata(path).map(|m| m.len()).unwrap_or(0)
        } else if path.is_dir() {
            self.calculate_dir_size(path)
        } else {
            0
        }
    }

    /// Calculate directory size.
    ///
    /// L5 fix: the previous version summed only the direct children (and a
    /// directory's own `metadata().len()` is 0 on NTFS), so any nested
    /// directory was reported as 0 bytes and `freed_physical_size` was
    /// systematically low. Walk the whole tree instead.
    ///
    /// Selection rationale: M8's `survey_tree` in `permanent_delete.rs` would
    /// be the preferred shared implementation, but it is private to that
    /// module and entangled with the executable-preview survey, so this slot
    /// reuses the recursive algorithm from `scanner/leftovers.rs`
    /// verbatim instead (including its no-symlink-follow `file_type()`
    /// handling). Deduplicating the two copies into one crate-level helper is
    /// left as follow-up work: `scanner/leftovers.rs` is outside this
    /// cleanup slot's file-ownership set.
    fn calculate_dir_size(&self, path: &Path) -> u64 {
        let mut size = 0u64;
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                // `file_type()` does not follow symlinks / junctions; using `is_dir()` would follow them:
                // one, it would count capacity from other drives; two, a loop pointing upward would recurse infinitely.
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    size += self.calculate_dir_size(&entry.path());
                } else if file_type.is_file() {
                    if let Ok(metadata) = entry.metadata() {
                        size += metadata.len();
                    }
                }
            }
        }
        size
    }
}

/// Read the original logical size of a recycle bin entry, so cleanup and scanning use the same measurement basis.
fn get_recycle_metadata_size(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    if !name.starts_with("$R") || name.len() <= 2 {
        return None;
    }

    let metadata_path = path.parent()?.join(format!("$I{}", &name[2..]));
    let bytes = fs::read(metadata_path).ok()?;
    if bytes.len() < 16 {
        return None;
    }

    Some(u64::from_le_bytes(bytes[8..16].try_into().ok()?))
}

/// Convert to actual occupied space using the specified volume's cluster size, avoiding wrong cluster sizes for cross-drive recycle bin statistics.
fn align_physical_size(logical_size: u64, cluster_size: u32) -> u64 {
    if logical_size == 0 || cluster_size == 0 {
        return 0;
    }

    let cluster_size = cluster_size as u64;
    ((logical_size + cluster_size - 1) / cluster_size) * cluster_size
}

/// Extract the drive root path needed by the Shell API from a recycle bin data path.
fn recycle_drive_root(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return None;
    }

    Some(format!("{}:\\", (bytes[0] as char).to_ascii_uppercase()))
}

/// Case-insensitive `strip_prefix` that never panics on a non-char boundary.
fn strip_ascii_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

/// Derive the $I metadata twin of a recycle bin entry from its $R data path.
///
/// A recycle bin entry is a pair sharing one suffix: `$R<suffix>` holds the data,
/// `$I<suffix>` holds the size / original-path / deletion-time metadata the Recycle Bin
/// UI displays. Deleting only one half leaves either invisible data or a phantom entry.
fn recycle_entry_twin_path(path: &str) -> Option<String> {
    let file_path = Path::new(path);
    let parent = file_path.parent()?;
    let file_name = file_path.file_name()?.to_string_lossy();
    let suffix = strip_ascii_prefix(&file_name, "$R")?;
    if suffix.is_empty() {
        return None;
    }
    Some(
        parent
            .join(format!("$I{}", suffix))
            .to_string_lossy()
            .into_owned(),
    )
}

/// Delete one recycle bin entry: the $R data blob plus its $I metadata twin.
///
/// H1 audit fix. The previous flow grouped the selected paths per drive and called
/// `SHEmptyRecycleBinW`, whose semantics are "empty the whole volume" — selecting 3
/// entries wiped every other item on that drive, silently and unrecoverably. The
/// scanner only surfaces entries under the current user's own SID folder, where the
/// owning user holds Full Control, so removing the pair directly needs no SYSTEM
/// privileges and leaves nothing behind for Explorer to display.
///
/// Guards, in order: `path_guard` rejects traversal and empty input;
/// `is_current_user_entry_path` pins the target to the current user's own recycle
/// data; the $I twin must live in the same directory. A tampered frontend therefore
/// cannot aim this at arbitrary files or at another account's entries.
///
/// 返回 `RecycleEntryDeletion` 而不是裸 `Result`（提速方案 §5.5）：失败可能是
/// "半途失败"——`$R` 已物理删除而 `$I` 清理失败。终态照旧如实报失败，但
/// `data_freed` 必须记录数据已不可恢复的事实，供统计口径拆分。
fn delete_recycle_entry(path: &str) -> RecycleEntryDeletion {
    let file_path = Path::new(path);
    if let Err(error) = crate::path_guard::validate_deletable(file_path) {
        return RecycleEntryDeletion::refused(error.to_string());
    }
    if !crate::scanner::recycle_bin::is_current_user_entry_path(path) {
        return RecycleEntryDeletion::refused(format!(
            "路径不是当前用户的回收站数据文件，已拒绝: {}",
            path
        ));
    }
    let Some(metadata_path) = recycle_entry_twin_path(path) else {
        return RecycleEntryDeletion::refused(format!(
            "路径不是 $R 回收站条目，已拒绝删除: {}",
            path
        ));
    };

    // Clear hidden/system/read-only attributes first: $Recycle.Bin contents carry the
    // hidden bit, and std::fs removal refuses read-only files.
    let data_started = Instant::now();
    let _ = windows_api::remove_protection_attributes(path);
    let data_result = if file_path.is_dir() {
        fs::remove_dir_all(file_path)
            .map_err(|error| format!("删除回收站数据目录失败: {}", error))
    } else {
        fs::remove_file(file_path).map_err(|error| format!("删除回收站数据文件失败: {}", error))
    };
    let data_delete_ms = data_started.elapsed().as_millis() as u64;
    if let Err(error) = data_result {
        return RecycleEntryDeletion {
            outcome: Err(error),
            is_dir_entry: file_path.is_dir(),
            data_freed: false,
            data_delete_ms,
            metadata_delete_ms: 0,
        };
    }

    // The data blob is gone; remove the metadata so Explorer stops showing the entry.
    // Deleting the data first means a metadata failure can never hide live data — worst
    // case is a phantom entry, reported honestly as a failure here.
    let metadata_started = Instant::now();
    let _ = windows_api::remove_protection_attributes(&metadata_path);
    let metadata_result = fs::remove_file(&metadata_path).map_err(|error| {
        format!(
            "回收站数据已删除但元数据清理失败 ({}): {}",
            metadata_path, error
        )
    });
    let metadata_delete_ms = metadata_started.elapsed().as_millis() as u64;
    RecycleEntryDeletion {
        outcome: metadata_result,
        is_dir_entry: file_path.is_dir(),
        data_freed: true,
        data_delete_ms,
        metadata_delete_ms,
    }
}

// ============================================================================
// Recycle-bin batch: bounded parallel deletion (回收站删除提速方案 §5.1–§5.5)
// ============================================================================

/// 单条回收站条目的终态结果 = 一个完整调度单位。
///
/// 返回结构体而不是 `Result` 是方案的硬约束（§5.5）：单条失败不得短路并行
/// collect，也不得丢失“$R 已删、$I 失败”时数据已释放的事实。聚合侧保证
/// `N_results == N_submitted` 且 `completed == succeeded + failed`。
struct RecycleEntryOutcome {
    path: String,
    logical_size: u64,
    physical_size: u64,
    kind: OutcomeKind,
    failure_reason: Option<DeleteFailureReason>,
    /// 该条目的 `$R` 是否为目录（决定 §5.3 的目录/文件计时桶）。
    is_dir_entry: bool,
    drive_root: Option<String>,
    /// `$R` 数据 blob 是否已物理删除（`$I` 失败时也为 true，§5.5 口径）。
    data_freed: bool,
    /// §5.3 三相位计时：`$I` 元数据读取 / `$R` 删除 / `$I` 删除，分开统计。
    metadata_read_ms: u64,
    data_delete_ms: u64,
    metadata_delete_ms: u64,
}

/// 一次条目删除的终态 + 相位信息（`delete_recycle_entry` 的返回值）。
struct RecycleEntryDeletion {
    outcome: Result<(), String>,
    /// `$R` 是否为目录（复用删除路径里已有的 `is_dir` 判定，不额外 stat）。
    is_dir_entry: bool,
    /// `$R` 数据 blob 是否已物理删除（`$I` 失败时也为 true，§5.5 口径）。
    data_freed: bool,
    data_delete_ms: u64,
    metadata_delete_ms: u64,
}

impl RecycleEntryDeletion {
    /// 校验拒绝：未触碰任何文件，无相位耗时。
    fn refused(message: String) -> Self {
        Self {
            outcome: Err(message),
            is_dir_entry: false,
            data_freed: false,
            data_delete_ms: 0,
            metadata_delete_ms: 0,
        }
    }
}

/// 回收站批的原子计数器（§5.2）：worker 只 `fetch_add`，单一进度汇报者只
/// `load`，两者之间无锁、无 Mutex 竞争。
struct RecycleCounters {
    completed: AtomicUsize,
    succeeded: AtomicUsize,
    failed: AtomicUsize,
    /// 完整成功（`$R` 与 `$I` 都删除）确认释放的物理字节——前端口径。
    freed_physical: AtomicU64,
    /// 数据已释放量（`$R` 删除即计入，含 `$I` 失败条目）——内部口径，§5.5。
    data_freed: AtomicU64,
}

impl Default for RecycleCounters {
    fn default() -> Self {
        Self {
            completed: AtomicUsize::new(0),
            succeeded: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            freed_physical: AtomicU64::new(0),
            data_freed: AtomicU64::new(0),
        }
    }
}

impl RecycleCounters {
    fn record(&self, outcome: &RecycleEntryOutcome) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        match outcome.kind {
            OutcomeKind::Freed => {
                self.succeeded.fetch_add(1, Ordering::Relaxed);
                self.freed_physical
                    .fetch_add(outcome.physical_size, Ordering::Relaxed);
            }
            // 回收站条目没有重启标记路径（delete_recycle_entry 不产生该终态）；
            // 防御性归入失败，保证 completed 恒等于 succeeded + failed。
            OutcomeKind::RebootPending | OutcomeKind::Failed => {
                self.failed.fetch_add(1, Ordering::Relaxed);
            }
        }
        if outcome.data_freed {
            self.data_freed
                .fetch_add(outcome.logical_size, Ordering::Relaxed);
        }
    }
}

/// 汇报者线程发射的进度快照：全部字段来自原子计数，completed 为进度分母
/// 基准（有失败条目也必须能到 100%，§5.2）。
fn recycle_progress_snapshot(
    counters: &RecycleCounters,
    total_count: usize,
    started_at: Instant,
) -> EnhancedDeleteProgress {
    EnhancedDeleteProgress {
        phase: "cleaning".to_string(),
        processed_count: counters.completed.load(Ordering::Relaxed),
        total_count,
        success_count: counters.succeeded.load(Ordering::Relaxed),
        failed_count: counters.failed.load(Ordering::Relaxed),
        reboot_pending_count: 0,
        freed_physical_size: counters.freed_physical.load(Ordering::Relaxed),
        elapsed_ms: started_at.elapsed().as_millis() as u64,
    }
}

/// 回收站批并行度解析（§5.1）。纯函数、不读环境变量，矩阵直接单测：
/// 缺失 → `min(逻辑核数, 8)`；解析失败 / `0` / 负数 → 默认值 + warn；
/// 超上限 → 截断 + warn。
fn parse_recycle_worker_count(raw: Option<&str>, logical_cpus: usize) -> usize {
    let default = logical_cpus.min(RECYCLE_DEFAULT_WORKER_CAP);
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return default;
    };
    match raw.parse::<usize>() {
        Ok(0) => {
            warn!(
                "{}=0 无效，回退默认并发度 {}",
                LIGHTC_RECYCLE_DELETE_WORKERS, default
            );
            default
        }
        Ok(count) if count > RECYCLE_MAX_WORKERS => {
            warn!(
                "{}={} 超出上限 {}，已截断",
                LIGHTC_RECYCLE_DELETE_WORKERS, count, RECYCLE_MAX_WORKERS
            );
            RECYCLE_MAX_WORKERS
        }
        Ok(count) => count,
        Err(_) => {
            warn!(
                "{}='{}' 无法解析，回退默认并发度 {}",
                LIGHTC_RECYCLE_DELETE_WORKERS, raw, default
            );
            default
        }
    }
}

fn resolve_recycle_worker_count() -> usize {
    let raw = std::env::var(LIGHTC_RECYCLE_DELETE_WORKERS).ok();
    parse_recycle_worker_count(raw.as_deref(), num_cpus::get())
}

/// 聚合在主线程按请求顺序执行（rayon 保序收集的顺序 = 提交顺序）：
/// `record` 的记账规则与串行版共用同一套，`N_results == N_submitted`。
fn apply_recycle_outcomes(
    result: &mut EnhancedDeleteResult,
    outcomes: Vec<RecycleEntryOutcome>,
    processed_count: &mut usize,
) {
    for outcome in outcomes {
        result.record(outcome.kind, outcome.logical_size, outcome.physical_size);
        result.file_results.push(FileDeleteResult {
            path: outcome.path,
            success: outcome.kind == OutcomeKind::Freed,
            logical_size: outcome.logical_size,
            physical_size: outcome.physical_size,
            failure_reason: outcome.failure_reason,
            // 回收站条目没有重启标记路径。
            marked_for_reboot: false,
        });
        *processed_count += 1;
    }
}

/// 每卷分相计时（§5.3）：条目级 `Instant` 相位，纯内存聚合，不新增测量 I/O。
/// 目录条目耗时单独成桶——它是 `remove_dir_all` 递归删除的代价，为 §8 的
/// P4（目录递归优化）决策积累数据。
#[derive(Debug, Default)]
struct RecycleDriveTiming {
    entries: usize,
    dir_entries: usize,
    /// 目录条目三相耗时之和（单条墙钟 ≈ 三相之和）。
    dir_phase_ms: u64,
    file_entries: usize,
    /// 文件条目三相耗时之和。
    file_phase_ms: u64,
    metadata_read_ms: u64,
    data_delete_ms: u64,
    metadata_delete_ms: u64,
}

fn summarize_recycle_timings(
    outcomes: &[RecycleEntryOutcome],
) -> BTreeMap<String, RecycleDriveTiming> {
    let mut timings: BTreeMap<String, RecycleDriveTiming> = BTreeMap::new();
    for outcome in outcomes {
        let entry = timings
            .entry(outcome.drive_root.clone().unwrap_or_else(|| "?".to_string()))
            .or_default();
        entry.entries += 1;
        let phase_total =
            outcome.metadata_read_ms + outcome.data_delete_ms + outcome.metadata_delete_ms;
        if outcome.is_dir_entry {
            entry.dir_entries += 1;
            entry.dir_phase_ms += phase_total;
        } else {
            entry.file_entries += 1;
            entry.file_phase_ms += phase_total;
        }
        entry.metadata_read_ms += outcome.metadata_read_ms;
        entry.data_delete_ms += outcome.data_delete_ms;
        entry.metadata_delete_ms += outcome.metadata_delete_ms;
    }
    timings
}

fn log_recycle_phase_timings(
    timings: &BTreeMap<String, RecycleDriveTiming>,
    entries: usize,
    wall: Duration,
    workers: usize,
) {
    for (drive, timing) in timings {
        info!(
            "[回收站删除耗时] {drive}: {} 条（目录 {} ≈ {}ms / 文件 {} ≈ {}ms）→ $I 读 {}ms + $R 删 {}ms + $I 删 {}ms",
            timing.entries,
            timing.dir_entries,
            timing.dir_phase_ms,
            timing.file_entries,
            timing.file_phase_ms,
            timing.metadata_read_ms,
            timing.data_delete_ms,
            timing.metadata_delete_ms,
        );
    }
    let throughput = if wall.as_millis() > 0 {
        entries as f64 * 1000.0 / wall.as_millis() as f64
    } else {
        0.0
    };
    info!(
        "[回收站删除耗时] 总计 {}ms（workers {}, {} 条目）→ 条目吞吐 {:.0}/s",
        wall.as_millis(),
        workers,
        entries,
        throughput
    );
}

impl Default for EnhancedDeleteEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// M6: the ownership step has to report a Win32 code rather than panic, and
    /// it must not report success on a path that does not exist.
    ///
    /// Both outcomes are correct for the first case: an elevated process really
    /// can take ownership of a file it owns, an unelevated one is refused with
    /// `access denied`. Asserting only "no panic" keeps the test honest about the
    /// machine it runs on (M14's lesson) while still catching a wrong call order,
    /// which fails for everyone.
    #[cfg(windows)]
    #[test]
    fn taking_ownership_either_succeeds_or_reports_a_win32_code() {
        let dir = std::env::temp_dir().join(format!("lightc-m6-grant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let file = dir.join("payload.bin");
        std::fs::write(&file, b"x").expect("write file");

        match windows_api::grant_administrators_full_control(&file.to_string_lossy()) {
            Ok(()) => {}
            Err(message) => assert!(message.contains("win32"), "message was: {message}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn taking_ownership_of_a_missing_path_fails() {
        let missing = std::env::temp_dir()
            .join(format!("lightc-m6-missing-{}.bin", std::process::id()))
            .to_string_lossy()
            .to_string();

        assert!(windows_api::grant_administrators_full_control(&missing).is_err());
    }

    /// Read a path's DACL back as SDDL, so a test can see what actually landed.
    #[cfg(windows)]
    fn dacl_sddl(path: &str) -> String {
        use windows::core::{PCWSTR, PWSTR};
        use windows::Win32::Foundation::{LocalFree, HLOCAL, WIN32_ERROR};
        use windows::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
            SDDL_REVISION_1, SE_FILE_OBJECT,
        };
        use windows::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};

        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            let mut descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
            // SAFETY: `wide` is a NUL-terminated wide string; the descriptor is
            // LocalFree'd below
            let status = GetNamedSecurityInfoW(
                PCWSTR::from_raw(wide.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                &mut descriptor,
            );
            assert_eq!(status, WIN32_ERROR(0));

            let mut text = PWSTR::null();
            let mut length = 0u32;
            // SAFETY: `descriptor` is valid; `text` receives an API-allocated
            // buffer that is LocalFree'd below
            let converted = ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                Some(&mut length),
            );
            assert!(converted.is_ok(), "SDDL conversion failed");

            // SAFETY: on success the API guarantees `length` UTF-16 units at `text`
            let sddl = String::from_utf16_lossy(std::slice::from_raw_parts(
                text.as_ptr(),
                length as usize,
            ));
            LocalFree(Some(HLOCAL(text.as_ptr() as *mut _)));
            LocalFree(Some(HLOCAL(descriptor.0)));
            sddl
        }
    }

    /// M6, the half that can be proven without elevation: the ACE step really
    /// writes an explicit Administrators entry into the DACL.
    ///
    /// Ownership is a separate call that needs `SeRestorePrivilege`; this one only
    /// needs `WRITE_DAC`, which the owner of a temp file always has.
    ///
    /// Two details make the assertion narrower than it looks:
    ///
    /// - SDDL abbreviates `BUILTIN\Administrators` as `BA`, so the SID string
    ///   never appears in the output; each ACE ends with its trustee, hence `;BA)`.
    /// - A file under `%TEMP%` already inherits an Administrators ACE from the
    ///   profile, so "the trustee appears" would pass without the code doing
    ///   anything. The assertion is on the *count*, and inherited ACEs are the ones
    ///   carrying the `ID` flag — the new entry is the one without it.
    #[cfg(windows)]
    #[test]
    fn the_full_control_ace_adds_one_explicit_entry() {
        let dir = std::env::temp_dir().join(format!("lightc-m6-ace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let file = dir.join("payload.bin");
        std::fs::write(&file, b"x").expect("write file");
        let path = file.to_string_lossy().to_string();

        let administrators_aces = |sddl: &str| sddl.matches(";BA)").count();

        let before = administrators_aces(&dacl_sddl(&path));
        windows_api::grant_full_control_ace(&path).expect("grant full control on an owned file");
        let after = dacl_sddl(&path);

        assert_eq!(
            administrators_aces(&after),
            before + 1,
            "the ACE did not land in the DACL: {after}"
        );
        // FILE_ALL_ACCESS, granted rather than denied.
        assert!(after.contains("(A;;FA;;;BA)"), "unexpected ACE: {after}");

        // A second pass must merge rather than append: `GRANT_ACCESS` replaces the
        // entry it already made, so a retried delete does not grow the DACL.
        windows_api::grant_full_control_ace(&path).expect("second grant");
        assert_eq!(administrators_aces(&dacl_sddl(&path)), before + 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The reporting contract (S3c): a reboot-pending item is not a success, its
    /// bytes are not freed, and they are not "skipped" either — they are simply
    /// not free yet. `record` is the only place that encodes this now.
    #[test]
    fn reboot_pending_is_not_counted_as_freed() {
        let mut result = EnhancedDeleteResult::new();
        result.record(OutcomeKind::Freed, 100, 4096);
        result.record(OutcomeKind::RebootPending, 200, 8192);
        result.record(OutcomeKind::Failed, 300, 12288);

        assert_eq!(result.success_count, 1);
        assert_eq!(result.reboot_pending_count, 1);
        assert_eq!(result.failed_count, 1);
        assert_eq!(result.freed_logical_size, 100);
        assert_eq!(result.freed_physical_size, 4096);
        assert!(result.needs_reboot);
        assert_eq!(result.skipped_size, 12288);
    }

    #[test]
    fn test_physical_size_calculation() {
        let engine = EnhancedDeleteEngine::new();

        // Assume a cluster size of 4096
        assert_eq!(engine.calculate_physical_size(1), 4096);
        assert_eq!(engine.calculate_physical_size(4096), 4096);
        assert_eq!(engine.calculate_physical_size(4097), 8192);
        assert_eq!(engine.calculate_physical_size(0), 0);
    }

    #[test]
    fn system_protection_comes_from_the_shared_table() {
        fn refused(path: &str) -> bool {
            ensure_deletable(Path::new(path), DeleteScope::EnhancedClean).is_err()
        }

        assert!(refused("C:\\Windows\\System32\\ntdll.dll"));
        assert!(refused("C:\\pagefile.sys"));
        assert!(!refused("C:\\Temp\\test.tmp"));

        // C1 regression: the bypass spellings must hit the protection check as well
        assert!(refused("C:/Windows/System32/ntdll.dll"));
        assert!(refused(r"\\?\C:\Windows\System32\ntdll.dll"));
        // And traversal paths are rejected outright by the guard, before any deletion
        assert!(crate::path_guard::validate_deletable(Path::new(
            r"C:\Temp\..\Windows\System32\config"
        ))
        .is_err());
    }

    #[test]
    fn test_extended_junk_paths_are_deletable() {
        let engine = EnhancedDeleteEngine::new();

        // Caches/diagnostic directories newly included in deep scanning must be neither blocked by system protection nor disallowed for elevated deletion,
        // otherwise they are found but can never be deleted.
        let deletable_paths = [
            r"C:\Windows\SystemTemp\setup.tmp",
            r"C:\Windows\LiveKernelReports\WATCHDOG\report.dmp",
            r"C:\Users\Test\AppData\Local\Microsoft\Windows\Explorer\iconcache.db",
            r"C:\Users\Test\AppData\Local\Google\Chrome\User Data\Default\Service Worker\CacheStorage\abc",
            r"C:\Users\Test\AppData\Local\NVIDIA\ComputeCache\shader.bin",
            r"C:\ProgramData\Microsoft\Windows\WER\ReportQueue\AppCrash\report.wer",
        ];

        for path in deletable_paths {
            assert!(
                ensure_deletable(Path::new(path), DeleteScope::EnhancedClean).is_ok(),
                "不应被系统保护拦截: {}",
                path
            );
            assert!(
                crate::path_guard::is_safe_for_ownership(Path::new(path)),
                "应允许提权删除: {}",
                path
            );
        }
    }

    #[test]
    fn test_recycle_drive_root() {
        // The Shell API empties by drive letter; invalid paths must be rejected rather than falling back to C drive by default.
        assert_eq!(
            recycle_drive_root(r"d:\$Recycle.Bin\item"),
            Some("D:\\".to_string())
        );
        assert_eq!(recycle_drive_root(r"not-a-windows-path"), None);
    }

    #[test]
    fn test_recycle_entry_twin_path() {
        // H1: an entry is a $R data blob + a $I metadata twin sharing one suffix
        assert_eq!(
            recycle_entry_twin_path(r"D:\$Recycle.Bin\S-1-5-21\$R00000001").as_deref(),
            Some(r"D:\$Recycle.Bin\S-1-5-21\$I00000001")
        );
        // NTFS is case-insensitive: a lowercase $r name still maps to the $I twin
        assert_eq!(
            recycle_entry_twin_path(r"D:\$Recycle.Bin\S-1-5-21\$r00000001").as_deref(),
            Some(r"D:\$Recycle.Bin\S-1-5-21\$I00000001")
        );
        // Anything that is not an $R file has no twin
        assert_eq!(recycle_entry_twin_path(r"C:\Temp\notes.txt"), None);
        assert_eq!(
            recycle_entry_twin_path(r"D:\$Recycle.Bin\S-1-5-21\$I00000001"),
            None
        );
        // A bare "$R" without the mandatory suffix is not an entry either
        assert_eq!(
            recycle_entry_twin_path(r"D:\$Recycle.Bin\S-1-5-21\$R"),
            None
        );
    }

    #[test]
    fn test_recycle_entry_deletion_guards() {
        // H1 guards: a tampered frontend cannot aim the per-entry deletion at arbitrary files.
        // Traversal input is rejected by path_guard before anything is touched.
        assert!(
            delete_recycle_entry(r"C:\$Recycle.Bin\S-1-5-21\..\..\Windows\System32\config")
                .outcome
                .is_err()
        );
        // An arbitrary user file is not a current-user recycle data entry.
        assert!(delete_recycle_entry(r"C:\Temp\definitely-not-recycle.txt")
            .outcome
            .is_err());
        // A recycle-bin path without the $R prefix (e.g. the $I metadata itself) is rejected.
        assert!(delete_recycle_entry(r"C:\$Recycle.Bin\S-1-5-21\$I00000001")
            .outcome
            .is_err());
    }

    // ========================================================================
    // 回收站批有界并行（提速方案 §5.1–§5.5）
    // ========================================================================

    fn synthetic_recycle_outcome(path: &str, kind: OutcomeKind, physical: u64) -> RecycleEntryOutcome {
        RecycleEntryOutcome {
            path: path.to_string(),
            logical_size: physical,
            physical_size: physical,
            kind,
            failure_reason: (kind == OutcomeKind::Failed)
                .then(|| DeleteFailureReason::Other("synthetic failure".to_string())),
            is_dir_entry: false,
            drive_root: Some("D:\\".to_string()),
            data_freed: kind == OutcomeKind::Freed,
            metadata_read_ms: 0,
            data_delete_ms: 0,
            metadata_delete_ms: 0,
        }
    }

    /// 并发度解析矩阵（§5.1）：缺失/解析失败/0/负数回退默认，超上限截断，
    /// 默认 = min(逻辑核数, 8) 只是初始值。
    #[test]
    fn recycle_worker_count_parses_the_override_matrix() {
        assert_eq!(parse_recycle_worker_count(None, 16), 8);
        assert_eq!(parse_recycle_worker_count(Some("4"), 16), 4);
        assert_eq!(parse_recycle_worker_count(Some(" 8 "), 16), 8);
        // 解析失败 / 零 / 负数 / 空串：回退默认。
        assert_eq!(parse_recycle_worker_count(Some("abc"), 16), 8);
        assert_eq!(parse_recycle_worker_count(Some("0"), 16), 8);
        assert_eq!(parse_recycle_worker_count(Some("-2"), 16), 8);
        assert_eq!(parse_recycle_worker_count(Some(""), 16), 8);
        // 超上限截断到 RECYCLE_MAX_WORKERS；默认 = min(逻辑核数, 封顶)。
        assert_eq!(parse_recycle_worker_count(Some("999"), 16), RECYCLE_MAX_WORKERS);
        assert_eq!(parse_recycle_worker_count(None, 2), 2);
    }

    /// §5.5 硬约束：结果条数 == 提交条数，且 file_results 保持请求顺序
    /// （rayon par_iter + collect 的保序收集由聚合侧落实）。
    #[test]
    fn recycle_outcomes_apply_in_request_order_and_conserve_counts() {
        let outcomes = vec![
            synthetic_recycle_outcome("A", OutcomeKind::Freed, 100),
            synthetic_recycle_outcome("B", OutcomeKind::Failed, 200),
            synthetic_recycle_outcome("C", OutcomeKind::Freed, 300),
        ];
        let mut result = EnhancedDeleteResult::new();
        let mut processed = 0usize;
        apply_recycle_outcomes(&mut result, outcomes, &mut processed);

        assert_eq!(processed, 3);
        assert_eq!(result.file_results.len(), 3);
        assert_eq!(result.success_count, 2);
        assert_eq!(result.failed_count, 1);
        assert_eq!(result.freed_physical_size, 400);
        let paths: Vec<_> = result.file_results.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["A", "B", "C"]);
    }

    /// §5.5 口径拆分：`$R` 已删但 `$I` 失败的条目计入 failed（不进前端的
    /// freed_physical），但 data_freed 统计必须如实计入数据已释放量。
    #[test]
    fn recycle_counters_keep_data_freed_out_of_freed_physical() {
        let counters = RecycleCounters::default();
        let outcome = RecycleEntryOutcome {
            path: "x".to_string(),
            logical_size: 512,
            physical_size: 4096,
            kind: OutcomeKind::Failed,
            failure_reason: Some(DeleteFailureReason::Other("$I 删除失败".to_string())),
            is_dir_entry: false,
            drive_root: Some("D:\\".to_string()),
            data_freed: true,
            metadata_read_ms: 1,
            data_delete_ms: 2,
            metadata_delete_ms: 3,
        };
        counters.record(&outcome);

        assert_eq!(counters.completed.load(Ordering::Relaxed), 1);
        assert_eq!(counters.succeeded.load(Ordering::Relaxed), 0);
        assert_eq!(counters.failed.load(Ordering::Relaxed), 1);
        assert_eq!(counters.freed_physical.load(Ordering::Relaxed), 0);
        assert_eq!(counters.data_freed.load(Ordering::Relaxed), 512);
    }

    /// 完整成功条目同时进两个口径：freed_physical（前端）与 data_freed（内部）。
    #[test]
    fn recycle_counters_count_full_success_in_both_buckets() {
        let counters = RecycleCounters::default();
        let outcome = synthetic_recycle_outcome("ok", OutcomeKind::Freed, 100);
        counters.record(&outcome);

        assert_eq!(counters.succeeded.load(Ordering::Relaxed), 1);
        assert_eq!(counters.freed_physical.load(Ordering::Relaxed), 100);
        assert_eq!(counters.data_freed.load(Ordering::Relaxed), 100);
    }

    /// §5.3 分桶：目录条目与文件条目的相位耗时分列，按卷归组。
    #[test]
    fn recycle_timing_summary_splits_dir_and_file_entries_per_drive() {
        let mut dir = synthetic_recycle_outcome("dir-entry", OutcomeKind::Freed, 0);
        dir.is_dir_entry = true;
        dir.drive_root = Some("D:\\".to_string());
        dir.metadata_read_ms = 1;
        dir.data_delete_ms = 2;
        dir.metadata_delete_ms = 3;
        let mut file = synthetic_recycle_outcome("file-entry", OutcomeKind::Freed, 0);
        file.is_dir_entry = false;
        file.drive_root = Some("D:\\".to_string());
        file.metadata_read_ms = 10;
        file.data_delete_ms = 20;
        file.metadata_delete_ms = 30;

        let timings = summarize_recycle_timings(&[dir, file]);
        let timing = timings.get("D:\\").expect("D 盘聚合应存在");
        assert_eq!(timing.entries, 2);
        assert_eq!(timing.dir_entries, 1);
        assert_eq!(timing.dir_phase_ms, 6);
        assert_eq!(timing.file_entries, 1);
        assert_eq!(timing.file_phase_ms, 60);
        assert_eq!(timing.metadata_read_ms, 11);
        assert_eq!(timing.data_delete_ms, 22);
        assert_eq!(timing.metadata_delete_ms, 33);
    }
}
