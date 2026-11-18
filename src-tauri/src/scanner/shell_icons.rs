// ============================================================================
// Shell icon entry scanning and handling
//
// The "This PC" entry and the navigation pane entry use different registry mounting mechanisms, so they must be validated and operated on separately by entry type.
// ============================================================================

use chrono::Local;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{c_void, OsString};
use std::fs;
use std::io::Write;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use winapi::shared::minwindef::{FALSE, TRUE};
use winapi::shared::sddl::{self, SDDL_REVISION_1};
use winapi::um::accctrl::{
    ACCESS_MODE, DENY_ACCESS, EXPLICIT_ACCESS_W, NO_INHERITANCE, SE_REGISTRY_KEY, TRUSTEE_IS_SID,
    TRUSTEE_IS_WELL_KNOWN_GROUP,
};
use winapi::um::aclapi::{GetSecurityInfo, SetEntriesInAclW, SetSecurityInfo};
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::securitybaseapi::{AllocateAndInitializeSid, FreeSid};
use winapi::um::winbase::LocalFree;
use winapi::um::winnt::{
    ACL, DACL_SECURITY_INFORMATION, DELETE, GROUP_SECURITY_INFORMATION, KEY_ALL_ACCESS,
    KEY_CREATE_SUB_KEY, KEY_READ, KEY_SET_VALUE, KEY_WOW64_32KEY, KEY_WOW64_64KEY,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_WORLD_RID,
    SECURITY_WORLD_SID_AUTHORITY, SID_IDENTIFIER_AUTHORITY, WRITE_DAC, WRITE_OWNER,
};
use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::{RegKey, HKEY};

const MY_COMPUTER_NAMESPACE_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\MyComputer\NameSpace";
const DESKTOP_NAMESPACE_PATH: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Desktop\NameSpace";
const NAVIGATION_CLSID_PATH: &str = r"Software\Classes\CLSID";
const NAVIGATION_PIN_VALUE: &str = "System.IsPinnedToNameSpaceTree";
const SHCNE_ASSOCCHANGED: u32 = 0x0800_0000;
const SHCNF_IDLIST: u32 = 0x0000;

#[link(name = "shell32")]
extern "system" {
    fn SHChangeNotify(event_id: u32, flags: u32, item1: *const c_void, item2: *const c_void);
}
const SYSTEM_CLSIDS: &[&str] = &[
    "{20D04FE0-3AEA-1069-A2D8-08002B30309D}",
    "{450D8FBA-AD25-11D0-98A6-006008059382}",
    "{F02C1A0D-BE21-4350-88B0-7367288F2C01}",
    "{645FF040-5081-101B-9F08-00AA002F954E}",
    "{59031A47-3F72-44A7-89C5-5595FE6B30EE}",
    "{374DE290-123F-4565-9164-39C4925E467B}",
    // Linux is a Windows feature entry and should not be identified as a third-party item just because it uses the navigation-pane pinned marker.
    "{B2B4A4D1-2754-4140-A2EB-9A76D9D7CDC6}",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ShellEntryKind {
    #[serde(rename = "myComputer")]
    MyComputer,
    #[serde(rename = "navigationPane")]
    NavigationPane,
}

impl Default for ShellEntryKind {
    fn default() -> Self {
        Self::MyComputer
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellIconInfo {
    pub entry_kind: ShellEntryKind,
    pub clsid: String,
    pub name: String,
    pub application_name: Option<String>,
    pub reg_path: String,
    pub hive: String,
    pub registry_view: String,
    pub source_path: Option<String>,
    pub risk_level: String,
    pub risk_reason: String,
    pub is_system_protected: bool,
    pub is_locked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ShellIconTarget {
    pub clsid: String,
    pub hive: String,
    pub registry_view: String,
    /// Older backups do not have this field; by default they are handled with the original "This PC entry" compatibility behavior.
    #[serde(default)]
    pub entry_kind: ShellEntryKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellIconOperationResult {
    pub success: bool,
    pub message: String,
    pub backup_path: Option<String>,
    pub needs_explorer_refresh: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ShellIconOperationLog {
    timestamp: String,
    operation: String,
    target: ShellIconTarget,
    success: bool,
    message: String,
    backup_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellIconBackup {
    target: ShellIconTarget,
    backup_path: String,
    acl_sddl: Option<String>,
    /// A thorough delete locks the parent Namespace, so the original ACL must be saved for a complete restore.
    #[serde(default)]
    namespace_acl_sddl: Option<String>,
    /// The navigation pane only modifies the pinned marker; backing up the original value is enough to avoid overwriting other app registration info on restore.
    #[serde(default)]
    navigation_pin_value: Option<u32>,
    created_at: String,
}

#[derive(Debug, Clone, Copy)]
struct RegistryTargetContext {
    root: HKEY,
    view_flags: u32,
}

/// Scan all supported Hives / Registry Views, to avoid missing 32-bit software nodes by looking only at the current process view.
pub fn scan_shell_icons() -> Result<Vec<ShellIconInfo>, String> {
    let scan_targets = [
        (
            "HKCU",
            "default",
            RegistryTargetContext {
                root: HKEY_CURRENT_USER,
                view_flags: 0,
            },
        ),
        (
            "HKLM",
            "64",
            RegistryTargetContext {
                root: HKEY_LOCAL_MACHINE,
                view_flags: KEY_WOW64_64KEY,
            },
        ),
        (
            "HKLM",
            "32",
            RegistryTargetContext {
                root: HKEY_LOCAL_MACHINE,
                view_flags: KEY_WOW64_32KEY,
            },
        ),
    ];
    let mut entries = Vec::new();

    for (hive, view, context) in scan_targets {
        if let Ok(namespace_key) = RegKey::predef(context.root)
            .open_subkey_with_flags(MY_COMPUTER_NAMESPACE_PATH, KEY_READ | context.view_flags)
        {
            for raw_clsid in namespace_key.enum_keys().filter_map(Result::ok) {
                let Some(clsid) = normalize_clsid(&raw_clsid) else {
                    continue;
                };
                // A permission anomaly on one third-party node should not block the scan results of other partition views.
                match build_shell_icon_info(&namespace_key, &clsid, hive, view, context) {
                    Ok(info) if info.risk_level == "safe" || info.risk_level == "locked" => {
                        entries.push(info)
                    }
                    Ok(_) => {}
                    Err(error) => log::debug!("跳过无法读取的外壳节点 {}: {}", clsid, error),
                }
            }
        }

        // First get candidate CLSIDs from Desktop\NameSpace, then read the pinned marker, avoiding a walk of the entire Classes\CLSID.
        if let (Ok(navigation_namespace), Ok(clsid_root)) = (
            RegKey::predef(context.root)
                .open_subkey_with_flags(DESKTOP_NAMESPACE_PATH, KEY_READ | context.view_flags),
            RegKey::predef(context.root)
                .open_subkey_with_flags(NAVIGATION_CLSID_PATH, KEY_READ | context.view_flags),
        ) {
            for raw_clsid in navigation_namespace.enum_keys().filter_map(Result::ok) {
                let Some(clsid) = normalize_clsid(&raw_clsid) else {
                    continue;
                };
                match build_navigation_info(&clsid_root, &clsid, hive, view, context) {
                    Ok(Some(info)) if info.risk_level == "safe" || info.risk_level == "locked" => {
                        entries.push(info)
                    }
                    Ok(_) => {}
                    Err(error) => log::debug!("跳过无法读取的导航窗格入口 {}: {}", clsid, error),
                }
            }
        }
    }

    // The same CLSID may exist in HKCU, HKLM or different Registry Views; these are genuinely different registry nodes,
    // and must not be deduplicated for the UI, otherwise handling only one node still lets the client remount the icon from the other.
    let mut unique_entries: HashMap<String, ShellIconInfo> = HashMap::new();
    for entry in entries {
        let key = format!(
            "{}:{}:{}:{}",
            entry.hive.to_ascii_lowercase(),
            entry.registry_view.to_ascii_lowercase(),
            format!("{:?}", entry.entry_kind).to_ascii_lowercase(),
            entry.clsid.to_ascii_lowercase()
        );
        unique_entries.entry(key).or_insert(entry);
    }
    let mut entries: Vec<ShellIconInfo> = unique_entries.into_values().collect();
    entries.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    Ok(entries)
}

fn build_shell_icon_info(
    namespace_key: &RegKey,
    clsid: &str,
    hive: &str,
    registry_view: &str,
    context: RegistryTargetContext,
) -> Result<ShellIconInfo, String> {
    let namespace_item = namespace_key
        .open_subkey_with_flags(clsid, KEY_READ | context.view_flags)
        .map_err(|error| format!("打开外壳图标节点失败 {}: {}", clsid, error))?;
    let clsid_key = RegKey::predef(HKEY_CLASSES_ROOT)
        .open_subkey_with_flags(&format!(r"CLSID\{}", clsid), KEY_READ | context.view_flags)
        .ok();

    // Some software writes an empty Namespace default value; an empty value must not mask the real product name under CLSID.
    let raw_name = namespace_item
        .get_value::<String, _>("")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            clsid_key
                .as_ref()
                .and_then(|key| key.get_value::<String, _>("").ok())
                .filter(|value| !value.trim().is_empty())
        });
    let name = raw_name
        .as_deref()
        .map(resolve_indirect_string)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| clsid.to_string());
    let source_path = clsid_key.as_ref().and_then(find_source_path);
    let known_application = identify_known_application(clsid, &name, source_path.as_deref());
    let application_name = identify_application(clsid, &name, source_path.as_deref());
    let system_by_clsid = SYSTEM_CLSIDS
        .iter()
        .any(|item| item.eq_ignore_ascii_case(clsid));
    // Some cloud drives mount their Namespace node on system host DLLs like shdocvw.dll, so filtering by component path alone is not enough.
    let system_by_source =
        source_path.as_deref().is_some_and(is_system_component_path) && known_application.is_none();
    let is_system_protected = system_by_clsid || system_by_source;
    let is_locked = read_acl_sddl(&namespace_item)
        .ok()
        .flatten()
        .is_some_and(is_lightc_lock_sddl);
    let (risk_level, risk_reason) = if is_system_protected {
        (
            "protected",
            "Windows 系统外壳节点或系统组件，禁止清理".to_string(),
        )
    } else if is_locked {
        (
            "locked",
            "该节点已由 LightC 清空并限制写入，可解锁恢复".to_string(),
        )
    } else if application_name.is_none() {
        ("unknown", "无法确认关联应用，默认不展示".to_string())
    } else {
        (
            "safe",
            if source_path.is_some() {
                "已确认第三方应用关联，清理前会自动备份".to_string()
            } else {
                // Some software only registers a Namespace name, and its component path may be unreadable in other registry views or permission scopes.
                "已根据外壳图标名称确认第三方应用，组件路径暂不可读，清理前会自动备份".to_string()
            },
        )
    };

    Ok(ShellIconInfo {
        entry_kind: ShellEntryKind::MyComputer,
        clsid: clsid.to_string(),
        name,
        application_name,
        reg_path: format!(r"{}\{}\{}", hive, MY_COMPUTER_NAMESPACE_PATH, clsid),
        hive: hive.to_string(),
        registry_view: registry_view.to_string(),
        source_path,
        risk_level: risk_level.to_string(),
        risk_reason,
        is_system_protected,
        is_locked,
    })
}

/// Read the third-party CLSIDs pinned to Explorer's left navigation pane; returning None means not pinned or not suitable for display.
fn build_navigation_info(
    clsid_root: &RegKey,
    clsid: &str,
    hive: &str,
    registry_view: &str,
    context: RegistryTargetContext,
) -> Result<Option<ShellIconInfo>, String> {
    let clsid_key = clsid_root
        .open_subkey_with_flags(clsid, KEY_READ | context.view_flags)
        .map_err(|error| format!("打开导航窗格 CLSID 失败 {}: {}", clsid, error))?;
    let pin_value = clsid_key.get_value::<u32, _>(NAVIGATION_PIN_VALUE).ok();
    let is_pinned = pin_value.is_some_and(|value| value != 0);
    let target = ShellIconTarget {
        clsid: clsid.to_string(),
        hive: hive.to_string(),
        registry_view: registry_view.to_string(),
        entry_kind: ShellEntryKind::NavigationPane,
    };
    // Only bring back entries that LightC has backed up and hidden, ensuring the scan does not balloon into the entire CLSID list.
    if !is_pinned && !has_navigation_backup(&target)? {
        return Ok(None);
    }

    let raw_name = clsid_key
        .get_value::<String, _>("")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let name = raw_name
        .as_deref()
        .map(resolve_indirect_string)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| clsid.to_string());
    let source_path = find_source_path(&clsid_key);
    let known_application = identify_known_application(clsid, &name, source_path.as_deref());
    let application_name = identify_application(clsid, &name, source_path.as_deref());
    let system_by_clsid = SYSTEM_CLSIDS
        .iter()
        .any(|item| item.eq_ignore_ascii_case(clsid));
    let system_by_source =
        source_path.as_deref().is_some_and(is_system_component_path) && known_application.is_none();
    let is_system_protected = system_by_clsid || system_by_source;
    if is_system_protected || application_name.is_none() {
        return Ok(None);
    }

    Ok(Some(ShellIconInfo {
        entry_kind: ShellEntryKind::NavigationPane,
        clsid: clsid.to_string(),
        name,
        application_name,
        reg_path: format!(r"{}\{}\{}", hive, NAVIGATION_CLSID_PATH, clsid),
        hive: hive.to_string(),
        registry_view: registry_view.to_string(),
        source_path,
        risk_level: if is_pinned { "safe" } else { "locked" }.to_string(),
        risk_reason: if is_pinned {
            "已确认第三方导航窗格入口，操作前会自动备份固定状态".to_string()
        } else {
            "该导航窗格入口已由 LightC 移除，可从备份恢复".to_string()
        },
        is_system_protected: false,
        is_locked: !is_pinned,
    }))
}

/// Identify the application the user actually installed via CLSID, registry display name and component file name.
/// Only nodes whose application can be identified enter the MVP list, avoiding exposing internal nodes like RegFolder to users.
fn identify_application(
    clsid: &str,
    display_name: &str,
    source_path: Option<&str>,
) -> Option<String> {
    if let Some(application_name) = identify_known_application(clsid, display_name, source_path) {
        return Some(application_name.to_string());
    }

    let normalized_name = display_name.trim();
    let normalized_name_lower = normalized_name.to_ascii_lowercase();
    if !normalized_name.is_empty()
        && !normalized_name_lower.starts_with("clsid_")
        && !normalized_name_lower.contains("regfolder")
        && !normalized_name_lower.contains("thispc")
    {
        return Some(normalized_name.to_string());
    }
    None
}

/// Identify applications with clear product characteristics, covering cases where third parties borrow system Shell host DLLs.
fn identify_known_application(
    clsid: &str,
    display_name: &str,
    source_path: Option<&str>,
) -> Option<&'static str> {
    // Baidu Netdisk uses shdocvw.dll as a system Shell host, so the component path cannot reflect product ownership.
    if clsid.eq_ignore_ascii_case("{679F137C-3162-45DA-BE3C-2F9C3D093F64}") {
        return Some("百度网盘");
    }
    let source_name = source_path
        .and_then(|path| Path::new(path).file_stem())
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    // The GUID is only a lookup key and must not participate in app name matching, otherwise numeric fragments in ordinary CLSIDs could cause false associations.
    let haystack = format!("{} {}", display_name, source_name).to_lowercase();
    let known_applications = [
        (
            &["baidu", "baidunetdisk", "netdisk", "百度", "百度网盘"][..],
            "百度网盘",
        ),
        (&["quark", "夸克"][..], "夸克网盘"),
        (&["wps", "kingsoft", "金山"][..], "WPS"),
        (&["thunder", "xunlei", "迅雷"][..], "迅雷"),
        (&["aliyun", "阿里云"][..], "阿里云盘"),
        (&["115", "115科技"][..], "115"),
    ];
    for (markers, application_name) in known_applications {
        if markers.iter().any(|marker| haystack.contains(marker)) {
            return Some(application_name);
        }
    }
    None
}

/// Re-validate the target before any normal delete or strong lock; arbitrary registry paths from the frontend must not be trusted.
pub fn remove_shell_icon(
    target: &ShellIconTarget,
    mode: u8,
) -> Result<ShellIconOperationResult, String> {
    if mode != 1 && mode != 2 {
        return Err("不支持的清理模式".to_string());
    }
    let target = normalize_target(target)?;
    if matches!(target.entry_kind, ShellEntryKind::NavigationPane) {
        if mode == 2 {
            return Err("导航窗格入口不支持防复活锁定，只能移除固定状态".to_string());
        }
        return remove_navigation_entry(&target);
    }
    let context = target_context(&target)?;
    // First request only read and ACL-modify permissions, to stay compatible with nodes where a previous LightC deny ACE prevents requesting FullControl.
    let initial_key = open_target_key(
        &target,
        context,
        KEY_READ | WRITE_DAC | WRITE_OWNER | context.view_flags,
    )?;
    // First open the parent key with permissions to read and modify the ACL, compatible with older versions that already added a deny ACE.
    let initial_namespace_key = open_namespace_key(context, KEY_READ | WRITE_DAC | WRITE_OWNER)?;
    let info = build_shell_icon_info_from_key(&initial_key, &target, context)?;
    if info.is_system_protected || info.risk_level == "unknown" {
        return Err("该节点无法确认是第三方外壳图标，已阻止操作".to_string());
    }

    let original_acl = read_acl_sddl(&initial_key)?;
    let original_namespace_acl = read_acl_sddl(&initial_namespace_key)?;
    // Must back up before removing the old lock, so the restore function preserves the original ACL state the user saw.
    let backup = create_backup(&target, &initial_key, &initial_namespace_key, context)?;

    // Nodes already locked by an older version reject KEY_ALL_ACCESS; first restore a temporary state with no LightC deny ACE.
    let had_lightc_lock = original_acl.as_deref().is_some_and(is_lightc_lock_sddl_ref);
    if had_lightc_lock {
        remove_lightc_lock_acl(&initial_key)?;
        let unlocked_key = open_target_key(
            &target,
            context,
            KEY_READ | WRITE_DAC | WRITE_OWNER | context.view_flags,
        )?;
        if read_acl_sddl(&unlocked_key)?
            .as_deref()
            .is_some_and(is_lightc_lock_sddl_ref)
        {
            return Err("解除旧版防复活权限失败，已停止删除操作".to_string());
        }
    }
    drop(initial_key);

    // Compatible with the parent LightC lock left by older versions; temporarily remove it first, otherwise the old placeholder node cannot be deleted.
    if original_namespace_acl
        .as_deref()
        .is_some_and(is_lightc_namespace_lock_sddl_ref)
    {
        remove_lightc_lock_acl(&initial_namespace_key)?;
    }

    drop(initial_namespace_key);
    let key = match open_target_key(
        &target,
        context,
        KEY_ALL_ACCESS | WRITE_DAC | WRITE_OWNER | context.view_flags,
    ) {
        Ok(key) => key,
        Err(error) if had_lightc_lock => {
            // If reopening fails, restore the old ACL, to avoid leaving an intermediate "unlocked but operation incomplete" state.
            if let Ok(rollback_key) = open_target_key(
                &target,
                context,
                KEY_READ | WRITE_DAC | WRITE_OWNER | context.view_flags,
            ) {
                if let Some(acl) = original_acl.as_deref() {
                    let _ = restore_acl(&rollback_key, acl);
                }
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    // The parent key must be opened with delete permission before adding the deny ACL; then reuse that handle for deletion to avoid race-condition revival.
    let namespace_key = match open_namespace_key(context, KEY_ALL_ACCESS | WRITE_DAC | WRITE_OWNER)
    {
        Ok(namespace_key) => namespace_key,
        Err(error) => {
            drop(key);
            return Err(error);
        }
    };
    let operation_result = (|| {
        match mode {
            1 => {
                // First clear the target's contents, to avoid a child key's own ACL or leftover handles causing the recursive parent deletion to fail.
                clear_key_contents(&key)?;
                drop(key);
                delete_target_key_from_parent(&namespace_key, &target, context)?;
                // A normal delete handles only the current node; if a parent lock was temporarily removed for old-version compatibility, restore it on success.
                if original_namespace_acl
                    .as_deref()
                    .is_some_and(is_lightc_namespace_lock_sddl_ref)
                {
                    restore_acl(
                        &namespace_key,
                        original_namespace_acl
                            .as_deref()
                            .ok_or_else(|| "缺少原始父级 ACL 备份".to_string())?,
                    )?;
                }
                verify_target_absent(&target, context)
            }
            2 => {
                // Lock the parent key first, then reuse the handle with delete permission to delete the target, eliminating the window for the client to recreate the node first.
                add_namespace_lock_and_verify(&namespace_key)?;
                clear_key_contents(&key)?;
                drop(key);
                delete_target_key_from_parent(&namespace_key, &target, context)?;
                verify_target_absent(&target, context)
            }
            _ => unreachable!("已在入口校验清理模式"),
        }
    })();
    if let Err(error) = operation_result {
        // Any verification failure restores the pre-operation ACL, to avoid leaving an unexplainable "half-cleaned, half-locked" state.
        if let Ok(rollback_key) = open_target_key(
            &target,
            context,
            KEY_READ | WRITE_DAC | WRITE_OWNER | context.view_flags,
        ) {
            if let Some(acl) = original_acl.as_deref() {
                let _ = restore_acl(&rollback_key, acl);
            }
        }
        if let Some(namespace_acl) = original_namespace_acl.as_deref() {
            let _ = restore_acl(&namespace_key, namespace_acl);
        }
        return Err(error);
    }

    let refresh_message = if mode == 2 {
        // Refresh Explorer after physical deletion, ensuring the old Shell Namespace cache is invalidated immediately.
        match restart_explorer() {
            Ok(()) => "节点已物理删除，已锁定父级防止复活并刷新 Explorer".to_string(),
            Err(error) => format!(
                "节点已物理删除并锁定父级，但 Explorer 刷新失败，请手动刷新：{}",
                error
            ),
        }
    } else {
        "节点已删除".to_string()
    };

    Ok(ShellIconOperationResult {
        success: true,
        message: refresh_message,
        backup_path: Some(backup.backup_path),
        needs_explorer_refresh: true,
    })
}

/// Unlocking only restores the original ACL and does not automatically re-import content, avoiding unexpected icon restoration when the user just wants to allow the software to re-register.
pub fn unlock_shell_icon(target: &ShellIconTarget) -> Result<ShellIconOperationResult, String> {
    let target = normalize_target(target)?;
    if matches!(target.entry_kind, ShellEntryKind::NavigationPane) {
        return Err("导航窗格入口不支持防复活锁定".to_string());
    }
    let context = target_context(&target)?;
    let backup =
        find_latest_backup(&target)?.ok_or_else(|| "未找到该节点的 ACL 备份".to_string())?;
    let namespace_key = open_namespace_key(context, KEY_READ | WRITE_DAC | WRITE_OWNER)?;
    // The new anti-revival lock is on the parent, so unlocking must restore the parent as well, otherwise the software still cannot recreate the node.
    if let Some(namespace_sddl) = backup.namespace_acl_sddl.as_deref() {
        restore_acl(&namespace_key, namespace_sddl)?;
    } else if read_acl_sddl(&namespace_key)?
        .as_deref()
        .is_some_and(is_lightc_namespace_lock_sddl_ref)
    {
        // Older backups have no parent snapshot, so only remove LightC's own deny ACE, preserving other system ACLs.
        remove_lightc_lock_acl(&namespace_key)?;
    }
    // After physical deletion the target key not existing is a normal state; unlocking only needs to restore the parent's permissions.
    if let Ok(key) = open_target_key(
        &target,
        context,
        KEY_READ | WRITE_DAC | WRITE_OWNER | context.view_flags,
    ) {
        if let Some(acl_sddl) = backup.acl_sddl.as_deref() {
            restore_acl(&key, acl_sddl)?;
        }
    }
    Ok(ShellIconOperationResult {
        success: true,
        message: "防复活权限已解除，当前节点内容未恢复".to_string(),
        backup_path: Some(backup.backup_path),
        needs_explorer_refresh: true,
    })
}

/// Import registry content from the most recent backup and restore the original ACL, covering both normal delete and thorough cleanup scenarios.
pub fn restore_shell_icon(target: &ShellIconTarget) -> Result<ShellIconOperationResult, String> {
    let target = normalize_target(target)?;
    if matches!(target.entry_kind, ShellEntryKind::NavigationPane) {
        return restore_navigation_entry(&target);
    }
    let context = target_context(&target)?;
    let backup =
        find_latest_backup(&target)?.ok_or_else(|| "未找到该节点的注册表备份".to_string())?;
    let namespace_key = open_namespace_key(context, KEY_READ | WRITE_DAC | WRITE_OWNER)?;
    let runtime_namespace_acl = read_acl_sddl(&namespace_key)?;
    let mut namespace_acl_changed = false;
    // Restore parent write permission before importing; when older metadata has no parent snapshot, remove the old LightC lock for compatibility.
    if let Some(namespace_sddl) = backup.namespace_acl_sddl.as_deref() {
        restore_acl(&namespace_key, namespace_sddl)?;
        namespace_acl_changed = true;
    } else if runtime_namespace_acl
        .as_deref()
        .is_some_and(is_lightc_namespace_lock_sddl_ref)
    {
        remove_lightc_lock_acl(&namespace_key)?;
        namespace_acl_changed = true;
    }
    let restore_result = (|| {
        // The target key in thorough mode may still have a deny-write ACL; restore the target ACL before importing.
        if let Some(sddl) = backup.acl_sddl.as_deref() {
            if let Ok(key) = open_target_key(
                &target,
                context,
                KEY_READ | WRITE_DAC | WRITE_OWNER | context.view_flags,
            ) {
                restore_acl(&key, sddl)?;
            }
        }
        import_registry_backup(&backup.backup_path, &target)?;
        // reg.exe import recreates the node; the ACL after import must be restored once more, covering backups that contain a full security descriptor.
        if let Some(sddl) = backup.acl_sddl.as_deref() {
            let key = open_target_key(
                &target,
                context,
                KEY_READ | WRITE_DAC | WRITE_OWNER | context.view_flags,
            )?;
            restore_acl(&key, sddl)?;
        }
        Ok::<(), String>(())
    })();
    if let Err(error) = restore_result {
        // On import failure, restore the pre-operation parent ACL, to avoid leaving an intermediate "write permission has been changed" state.
        if namespace_acl_changed {
            if let Some(runtime_sddl) = runtime_namespace_acl.as_deref() {
                let _ = restore_acl(&namespace_key, runtime_sddl);
            }
        }
        return Err(error);
    }
    Ok(ShellIconOperationResult {
        success: true,
        message: "注册表节点和原始 ACL 已恢复".to_string(),
        backup_path: Some(backup.backup_path),
        needs_explorer_refresh: true,
    })
}

pub fn open_shell_icon_backup_dir() -> Result<(), String> {
    let path = backup_dir();
    fs::create_dir_all(&path).map_err(|error| format!("创建备份目录失败: {}", error))?;
    Command::new("explorer")
        .arg(&path)
        .spawn()
        .map_err(|error| format!("打开备份目录失败: {}", error))?;
    Ok(())
}

/// Open a separate virtual-disk operation log, so users are not limited to one-off toasts to review history.
pub fn open_shell_icon_log() -> Result<(), String> {
    let path = shell_icon_log_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建操作记录目录失败: {}", error))?;
    }
    if !path.exists() {
        fs::write(&path, "").map_err(|error| format!("创建操作记录失败: {}", error))?;
    }
    // Opening the log content directly matches the expectation of an "operation record" better than just locating the file, and does not depend on Explorer refresh state.
    Command::new("notepad.exe")
        .arg(&path)
        .spawn()
        .map_err(|error| format!("打开操作记录失败: {}", error))?;
    Ok(())
}

/// Record the result of a virtual-disk operation; a log write failure does not affect the registry operation itself.
pub fn record_shell_icon_operation(
    target: &ShellIconTarget,
    operation: &str,
    result: &Result<ShellIconOperationResult, String>,
) {
    let (success, message, backup_path) = match result {
        Ok(value) => (
            value.success,
            value.message.clone(),
            value.backup_path.clone(),
        ),
        Err(error) => (false, error.clone(), None),
    };
    let entry = ShellIconOperationLog {
        timestamp: Local::now().to_rfc3339(),
        operation: operation.to_string(),
        target: target.clone(),
        success,
        message,
        backup_path,
    };
    let Ok(json) = serde_json::to_string(&entry) else {
        log::warn!("序列化虚拟磁盘操作记录失败");
        return;
    };
    let path = shell_icon_log_path();
    let write_result = (|| -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("创建操作记录目录失败: {}", error))?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| format!("打开操作记录失败: {}", error))?;
        writeln!(file, "{}", json).map_err(|error| format!("写入操作记录失败: {}", error))
    })();
    if let Err(error) = write_result {
        log::warn!("{}", error);
    }
}

fn shell_icon_log_path() -> PathBuf {
    crate::data_dir::get_data_dir()
        .join("logs")
        .join("shell_icons.log")
}

/// Write the target key into Regedit's LastKey and then open the registry editor, avoiding manual copying of long paths by the user.
pub fn open_shell_icon_registry(target: &ShellIconTarget) -> Result<(), String> {
    let target = normalize_target(target)?;
    let context = target_context(&target)?;
    let _key = open_target_key(&target, context, KEY_READ | context.view_flags)?;
    let (regedit_config, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Applets\Regedit")
        .map_err(|error| format!("写入注册表编辑器定位信息失败: {}", error))?;
    let root_name = regedit_root_name(&regedit_config);
    let registry_path = format!(
        r"{}\HKEY_{}\{}",
        root_name,
        if target.hive == "HKCU" {
            "CURRENT_USER"
        } else {
            "LOCAL_MACHINE"
        },
        target_registry_subkey_path(&target)
    );
    regedit_config
        .set_value("LastKey", &registry_path)
        .map_err(|error| format!("保存注册表定位信息失败: {}", error))?;
    // An already-running Regedit usually does not re-read LastKey; close the old process before launching to reliably expand to the target key.
    let regedit_status = hidden_command("taskkill", &["/f", "/im", "regedit.exe"])?;
    if !regedit_status.status.success() && regedit_status.status.code() != Some(128) {
        return Err("关闭已有注册表编辑器失败，无法定位到目标节点".to_string());
    }
    Command::new("regedit.exe")
        .spawn()
        .map_err(|error| format!("打开注册表编辑器失败: {}", error))?;
    Ok(())
}

/// Regedit's LastKey uses localized root node names; reusing the existing value stays compatible with both Chinese and English systems.
fn regedit_root_name(regedit_config: &RegKey) -> String {
    regedit_config
        .get_value::<String, _>("LastKey")
        .ok()
        .and_then(|last_key| {
            last_key
                .split_once(r"\HKEY_")
                .map(|(root_name, _)| root_name.trim().to_string())
        })
        .filter(|root_name| !root_name.is_empty())
        .unwrap_or_else(|| "计算机".to_string())
}

pub fn restart_explorer() -> Result<(), String> {
    // Only send a Shell change notification, do not end explorer.exe, avoiding loss of injected state for taskbar extensions like TranslucentTB.
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED,
            SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
    Ok(())
}

fn build_shell_icon_info_from_key(
    key: &RegKey,
    target: &ShellIconTarget,
    context: RegistryTargetContext,
) -> Result<ShellIconInfo, String> {
    let namespace_root = RegKey::predef(context.root)
        .open_subkey_with_flags(MY_COMPUTER_NAMESPACE_PATH, KEY_READ | context.view_flags)
        .map_err(|error| format!("打开 Namespace 根键失败: {}", error))?;
    build_shell_icon_info(
        &namespace_root,
        &target.clsid,
        &target.hive,
        &target.registry_view,
        context,
    )
    .or_else(|_| {
        let source_path = None;
        let is_protected = SYSTEM_CLSIDS
            .iter()
            .any(|item| item.eq_ignore_ascii_case(&target.clsid));
        Ok(ShellIconInfo {
            entry_kind: target.entry_kind.clone(),
            clsid: target.clsid.clone(),
            name: target.clsid.clone(),
            application_name: None,
            reg_path: registry_path(target),
            hive: target.hive.clone(),
            registry_view: target.registry_view.clone(),
            source_path,
            risk_level: if is_protected { "protected" } else { "unknown" }.to_string(),
            risk_reason: "目标状态发生变化，请重新扫描".to_string(),
            is_system_protected: is_protected,
            is_locked: read_acl_sddl(key)
                .ok()
                .flatten()
                .is_some_and(is_lightc_lock_sddl),
        })
    })
}

fn build_navigation_info_from_key(
    key: &RegKey,
    target: &ShellIconTarget,
) -> Result<ShellIconInfo, String> {
    let name = key
        .get_value::<String, _>("")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| resolve_indirect_string(&value))
        .unwrap_or_else(|| target.clsid.clone());
    let source_path = find_source_path(key);
    let application_name = identify_application(&target.clsid, &name, source_path.as_deref());
    let has_application = application_name.is_some();
    let is_system_protected = SYSTEM_CLSIDS
        .iter()
        .any(|item| item.eq_ignore_ascii_case(&target.clsid))
        || (source_path.as_deref().is_some_and(is_system_component_path)
            && identify_known_application(&target.clsid, &name, source_path.as_deref()).is_none());
    Ok(ShellIconInfo {
        entry_kind: ShellEntryKind::NavigationPane,
        clsid: target.clsid.clone(),
        name,
        application_name,
        reg_path: registry_path(target),
        hive: target.hive.clone(),
        registry_view: target.registry_view.clone(),
        source_path,
        risk_level: if is_system_protected || !has_application {
            "unknown".to_string()
        } else {
            "safe".to_string()
        },
        risk_reason: "目标状态发生变化，请重新扫描".to_string(),
        is_system_protected,
        is_locked: false,
    })
}

fn normalize_target(target: &ShellIconTarget) -> Result<ShellIconTarget, String> {
    let clsid = normalize_clsid(&target.clsid).ok_or_else(|| "CLSID 格式无效".to_string())?;
    if SYSTEM_CLSIDS
        .iter()
        .any(|system_clsid| system_clsid.eq_ignore_ascii_case(&clsid))
    {
        return Err("系统外壳节点受保护，禁止修改".to_string());
    }
    if target.hive != "HKCU" && target.hive != "HKLM" {
        return Err("不支持的注册表 Hive".to_string());
    }
    if target.registry_view != "default"
        && target.registry_view != "32"
        && target.registry_view != "64"
    {
        return Err("不支持的 Registry View".to_string());
    }
    if target.hive == "HKCU" && target.registry_view != "default" {
        return Err("当前版本只允许操作 HKCU 默认视图".to_string());
    }
    Ok(ShellIconTarget {
        clsid,
        hive: target.hive.clone(),
        registry_view: target.registry_view.clone(),
        entry_kind: target.entry_kind.clone(),
    })
}

fn target_context(target: &ShellIconTarget) -> Result<RegistryTargetContext, String> {
    let root = if target.hive == "HKCU" {
        HKEY_CURRENT_USER
    } else {
        HKEY_LOCAL_MACHINE
    };
    let view_flags = match target.registry_view.as_str() {
        "default" => 0,
        "32" => KEY_WOW64_32KEY,
        "64" => KEY_WOW64_64KEY,
        _ => return Err("不支持的 Registry View".to_string()),
    };
    Ok(RegistryTargetContext { root, view_flags })
}

fn open_target_key(
    target: &ShellIconTarget,
    context: RegistryTargetContext,
    flags: u32,
) -> Result<RegKey, String> {
    RegKey::predef(context.root)
        .open_subkey_with_flags(target_registry_subkey_path(target), flags)
        .map_err(|error| format!("打开目标注册表节点失败: {}", error))
}

fn open_namespace_key(context: RegistryTargetContext, flags: u32) -> Result<RegKey, String> {
    RegKey::predef(context.root)
        .open_subkey_with_flags(MY_COMPUTER_NAMESPACE_PATH, flags | context.view_flags)
        .map_err(|error| format!("打开 Namespace 父键失败: {}", error))
}

fn delete_target_key_from_parent(
    parent: &RegKey,
    target: &ShellIconTarget,
    context: RegistryTargetContext,
) -> Result<(), String> {
    // The parent key handle must be acquired before locking, otherwise the newly added deny ACE makes deletion return access denied again.
    parent
        .delete_subkey_with_flags(&target.clsid, context.view_flags)
        .map_err(|error| format!("删除外壳图标节点失败: {}", error))
}

fn clear_key_contents(key: &RegKey) -> Result<(), String> {
    // Enumeration errors must not be ignored, otherwise partial child-key cleanup failures still return "success", leaving revivable content.
    let child_names = key
        .enum_keys()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("枚举注册表子键失败: {}", error))?;
    for child in child_names {
        key.delete_subkey_all(child)
            .map_err(|error| format!("清空子键失败: {}", error))?;
    }
    let value_names = key
        .enum_values()
        .map(|value| value.map(|item| item.0))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("枚举注册表键值失败: {}", error))?;
    for value in value_names {
        key.delete_value(value)
            .map_err(|error| format!("清空键值失败: {}", error))?;
    }
    Ok(())
}

fn verify_target_absent(
    target: &ShellIconTarget,
    context: RegistryTargetContext,
) -> Result<(), String> {
    let path = target_registry_subkey_path(target);
    match RegKey::predef(context.root).open_subkey_with_flags(path, KEY_READ | context.view_flags) {
        Ok(_) => {
            Err("删除操作已返回，但注册表节点仍然存在，请关闭占用该节点的软件后重试".to_string())
        }
        Err(error) if matches!(error.raw_os_error(), Some(2 | 3)) => Ok(()),
        Err(error) => Err(format!(
            "无法核验注册表节点是否删除，可能仍存在或权限不足: {}",
            error
        )),
    }
}

/// Deny ordinary users from creating, modifying and deleting child keys on the parent Namespace, preventing the target from being re-registered by software.
fn add_namespace_lock_and_verify(namespace_key: &RegKey) -> Result<(), String> {
    add_lock_acl(namespace_key)?;
    let acl = read_acl_sddl(namespace_key)?;
    if !acl
        .as_deref()
        .is_some_and(is_lightc_namespace_lock_sddl_ref)
    {
        return Err("防复活父级权限未成功应用".to_string());
    }
    Ok(())
}

fn is_lightc_namespace_lock_sddl_ref(sddl: &str) -> bool {
    is_lightc_lock_sddl_ref(sddl)
}

fn create_backup(
    target: &ShellIconTarget,
    key: &RegKey,
    namespace_key: &RegKey,
    context: RegistryTargetContext,
) -> Result<ShellIconBackup, String> {
    let directory = backup_dir();
    fs::create_dir_all(&directory).map_err(|error| format!("创建备份目录失败: {}", error))?;
    let canonical_path = directory.join(format!("shell_icon_{}.reg", backup_stem(target)));
    let existing_backups = find_backups(target)?;
    if let Some(existing) = existing_backups
        .iter()
        .find(|backup| backup.backup_path == canonical_path.to_string_lossy())
        .or_else(|| {
            existing_backups
                .iter()
                .find(|backup| Path::new(&backup.backup_path).is_file())
        })
    {
        return consolidate_backup(target, context, existing);
    }

    // Backup files use stable names; only one original backup is kept per registry target, avoiding duplicate files on every click.
    let backup_path = canonical_path;
    export_registry_key(target, &backup_path)?;
    let metadata = ShellIconBackup {
        target: target.clone(),
        backup_path: backup_path.to_string_lossy().to_string(),
        acl_sddl: read_acl_sddl(key)?,
        namespace_acl_sddl: read_acl_sddl(namespace_key)?,
        navigation_pin_value: None,
        created_at: Local::now().to_rfc3339(),
    };
    let metadata_path = backup_path.with_extension("json");
    let json = serde_json::to_string_pretty(&metadata)
        .map_err(|error| format!("序列化备份信息失败: {}", error))?;
    fs::write(metadata_path, json).map_err(|error| format!("写入备份信息失败: {}", error))?;
    let _ = context;
    Ok(metadata)
}

/// The navigation pane only backs up the CLSID node and the pinned value, avoiding mistaking the entire CLSID registration info for removable objects.
fn remove_navigation_entry(target: &ShellIconTarget) -> Result<ShellIconOperationResult, String> {
    let context = target_context(target)?;
    let key = open_target_key(
        target,
        context,
        KEY_READ | KEY_SET_VALUE | context.view_flags,
    )?;
    let info = build_navigation_info_from_key(&key, target)?;
    if info.is_system_protected || info.risk_level == "unknown" {
        return Err("该节点无法确认是第三方导航窗格入口，已阻止操作".to_string());
    }
    let pin_value = key
        .get_value::<u32, _>(NAVIGATION_PIN_VALUE)
        .map_err(|error| format!("读取导航窗格固定状态失败: {}", error))?;
    let backup = create_navigation_backup(target, &key, pin_value, context)?;
    key.set_value(NAVIGATION_PIN_VALUE, &0u32)
        .map_err(|error| format!("移除导航窗格固定状态失败: {}", error))?;
    let current_value = key
        .get_value::<u32, _>(NAVIGATION_PIN_VALUE)
        .map_err(|error| format!("核验导航窗格固定状态失败: {}", error))?;
    if current_value != 0 {
        return Err("导航窗格固定状态未成功移除".to_string());
    }
    let message = match restart_explorer() {
        Ok(()) => "导航窗格入口已移除，已刷新 Explorer".to_string(),
        Err(error) => format!(
            "导航窗格入口已移除，但 Explorer 刷新失败，请手动刷新：{}",
            error
        ),
    };
    Ok(ShellIconOperationResult {
        success: true,
        message,
        backup_path: Some(backup.backup_path),
        needs_explorer_refresh: true,
    })
}

fn restore_navigation_entry(target: &ShellIconTarget) -> Result<ShellIconOperationResult, String> {
    let context = target_context(target)?;
    let backup = find_latest_backup(target)?
        .ok_or_else(|| "未找到该导航窗格入口的注册表备份".to_string())?;
    let key = open_target_key(
        target,
        context,
        KEY_READ | KEY_SET_VALUE | context.view_flags,
    )?;
    let pin_value = backup
        .navigation_pin_value
        .ok_or_else(|| "导航窗格备份缺少固定状态，无法恢复".to_string())?;
    key.set_value(NAVIGATION_PIN_VALUE, &pin_value)
        .map_err(|error| format!("恢复导航窗格固定状态失败: {}", error))?;
    let current_value = key
        .get_value::<u32, _>(NAVIGATION_PIN_VALUE)
        .map_err(|error| format!("核验导航窗格固定状态失败: {}", error))?;
    if current_value != pin_value {
        return Err("导航窗格固定状态恢复后核验不一致".to_string());
    }
    let message = match restart_explorer() {
        Ok(()) => "导航窗格入口已恢复，已刷新 Explorer".to_string(),
        Err(error) => format!(
            "导航窗格入口已恢复，但 Explorer 刷新失败，请手动刷新：{}",
            error
        ),
    };
    Ok(ShellIconOperationResult {
        success: true,
        message,
        backup_path: Some(backup.backup_path),
        needs_explorer_refresh: true,
    })
}

fn create_navigation_backup(
    target: &ShellIconTarget,
    key: &RegKey,
    pin_value: u32,
    context: RegistryTargetContext,
) -> Result<ShellIconBackup, String> {
    let directory = backup_dir();
    fs::create_dir_all(&directory).map_err(|error| format!("创建备份目录失败: {}", error))?;
    let canonical_path = directory.join(format!("shell_icon_{}.reg", backup_stem(target)));
    if let Some(existing) = find_backups(target)?
        .into_iter()
        .find(|backup| Path::new(&backup.backup_path).is_file())
    {
        return consolidate_navigation_backup(target, context, &existing, pin_value);
    }
    export_registry_key(target, &canonical_path)?;
    let metadata = ShellIconBackup {
        target: target.clone(),
        backup_path: canonical_path.to_string_lossy().to_string(),
        acl_sddl: read_acl_sddl(key)?,
        namespace_acl_sddl: None,
        navigation_pin_value: Some(pin_value),
        created_at: Local::now().to_rfc3339(),
    };
    write_backup_metadata(&canonical_path, &metadata)?;
    Ok(metadata)
}

fn consolidate_navigation_backup(
    target: &ShellIconTarget,
    context: RegistryTargetContext,
    original: &ShellIconBackup,
    pin_value: u32,
) -> Result<ShellIconBackup, String> {
    let directory = backup_dir();
    let canonical_path = directory.join(format!("shell_icon_{}.reg", backup_stem(target)));
    if original.backup_path != canonical_path.to_string_lossy()
        && Path::new(&original.backup_path).is_file()
        && !canonical_path.is_file()
    {
        fs::copy(&original.backup_path, &canonical_path)
            .map_err(|error| format!("整理注册表备份失败: {}", error))?;
    }
    let mut canonical = original.clone();
    canonical.backup_path = canonical_path.to_string_lossy().to_string();
    canonical.navigation_pin_value = Some(pin_value);
    write_backup_metadata(&canonical_path, &canonical)?;
    let _ = context;
    Ok(canonical)
}

fn write_backup_metadata(backup_path: &Path, metadata: &ShellIconBackup) -> Result<(), String> {
    let json = serde_json::to_string_pretty(metadata)
        .map_err(|error| format!("序列化备份信息失败: {}", error))?;
    fs::write(backup_path.with_extension("json"), json)
        .map_err(|error| format!("写入备份信息失败: {}", error))
}

fn backup_stem(target: &ShellIconTarget) -> String {
    let entry_kind = match target.entry_kind {
        ShellEntryKind::MyComputer => "my_computer",
        ShellEntryKind::NavigationPane => "navigation_pane",
    };
    format!(
        "{}_{}_{}_{}",
        entry_kind,
        target.hive,
        target.registry_view,
        target.clsid.trim_matches(['{', '}'])
    )
}

fn consolidate_backup(
    target: &ShellIconTarget,
    context: RegistryTargetContext,
    original: &ShellIconBackup,
) -> Result<ShellIconBackup, String> {
    let directory = backup_dir();
    let canonical_path = directory.join(format!("shell_icon_{}.reg", backup_stem(target)));
    if original.backup_path != canonical_path.to_string_lossy()
        && Path::new(&original.backup_path).is_file()
        && !canonical_path.is_file()
    {
        // Compatible with older timestamped backups: take the earliest one as the original snapshot and migrate to a stable file name.
        fs::copy(&original.backup_path, &canonical_path)
            .map_err(|error| format!("整理注册表备份失败: {}", error))?;
    }

    let mut canonical = original.clone();
    if canonical.namespace_acl_sddl.is_none() {
        let namespace_key = open_namespace_key(context, KEY_READ | WRITE_DAC | WRITE_OWNER)?;
        canonical.namespace_acl_sddl = read_acl_sddl(&namespace_key)?;
    }
    canonical.backup_path = canonical_path.to_string_lossy().to_string();
    let metadata_path = canonical_path.with_extension("json");
    let json = serde_json::to_string_pretty(&canonical)
        .map_err(|error| format!("写入整理后的备份信息失败: {}", error))?;
    fs::write(&metadata_path, json).map_err(|error| format!("写入备份信息失败: {}", error))?;

    // Keep only one set of files with stable names per target, avoiding continuous growth of the backup directory.
    for backup in find_backups(target)? {
        if backup.backup_path == canonical.backup_path {
            continue;
        }
        let _ = fs::remove_file(&backup.backup_path);
        let _ = fs::remove_file(Path::new(&backup.backup_path).with_extension("json"));
    }
    Ok(canonical)
}

fn export_registry_key(target: &ShellIconTarget, backup_path: &Path) -> Result<(), String> {
    let mut args = vec![
        "export".to_string(),
        registry_path(target),
        backup_path.to_string_lossy().to_string(),
        "/y".to_string(),
    ];
    append_view_arg(&mut args, target.registry_view.as_str());
    let output = hidden_command(
        "reg.exe",
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "导出注册表备份失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn import_registry_backup(path: &str, target: &ShellIconTarget) -> Result<(), String> {
    let mut args = vec!["import".to_string(), path.to_string()];
    append_view_arg(&mut args, target.registry_view.as_str());
    let output = hidden_command(
        "reg.exe",
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "导入注册表备份失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn append_view_arg(args: &mut Vec<String>, view: &str) {
    if view == "32" {
        args.push("/reg:32".to_string());
    }
    if view == "64" {
        args.push("/reg:64".to_string());
    }
}

fn backup_dir() -> PathBuf {
    crate::data_dir::get_data_dir()
        .join("reg_backups")
        .join("shell_icons")
}

fn find_latest_backup(target: &ShellIconTarget) -> Result<Option<ShellIconBackup>, String> {
    Ok(find_backups(target)?.pop())
}

fn find_backups(target: &ShellIconTarget) -> Result<Vec<ShellIconBackup>, String> {
    let directory = backup_dir();
    let mut candidates = Vec::new();
    if !directory.is_dir() {
        return Ok(candidates);
    }
    for entry in fs::read_dir(directory).map_err(|error| format!("读取备份目录失败: {}", error))?
    {
        let path = entry
            .map_err(|error| format!("读取备份项失败: {}", error))?
            .path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(json) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(backup) = serde_json::from_str::<ShellIconBackup>(&json) else {
            continue;
        };
        if &backup.target == target {
            candidates.push(backup);
        }
    }
    candidates.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    Ok(candidates)
}

fn has_navigation_backup(target: &ShellIconTarget) -> Result<bool, String> {
    Ok(find_backups(target)?.iter().any(|backup| {
        backup.navigation_pin_value.is_some() && Path::new(&backup.backup_path).is_file()
    }))
}

fn find_source_path(clsid_key: &RegKey) -> Option<String> {
    for subkey_name in ["InProcServer32", "LocalServer32"] {
        let Ok(key) = clsid_key.open_subkey_with_flags(subkey_name, KEY_READ) else {
            continue;
        };
        if let Ok(path) = key.get_value::<String, _>("") {
            return Some(path);
        }
    }
    None
}

fn is_system_component_path(path: &str) -> bool {
    let expanded = expand_environment_variables(path)
        .replace('/', "\\")
        .to_ascii_lowercase();
    let windows_root = std::env::var("WINDIR")
        .unwrap_or_else(|_| "C:\\Windows".to_string())
        .to_ascii_lowercase();
    expanded.starts_with(&format!("{}\\system32", windows_root))
        || expanded.starts_with(&format!("{}\\syswow64", windows_root))
        || expanded.contains("\\microsoft\\")
}

fn expand_environment_variables(path: &str) -> String {
    let mut expanded = path.to_string();
    let environment_names = [
        ("WINDIR", None),
        // SystemRoot is not exposed on some trimmed systems or test environments; falling back to Windir is needed to continue filtering system components.
        ("SYSTEMROOT", Some("WINDIR")),
        ("SYSTEMDRIVE", None),
        ("PROGRAMFILES", None),
        ("PROGRAMFILES(X86)", None),
    ];
    for (name, fallback_name) in environment_names {
        let marker = format!("%{}%", name);
        let value = std::env::var(name)
            .or_else(|_| match fallback_name {
                Some(fallback) => std::env::var(fallback),
                None => Err(std::env::VarError::NotPresent),
            })
            .or_else(|_| {
                if name == "SYSTEMDRIVE" {
                    std::env::var("WINDIR").map(|windows_root| {
                        windows_root
                            .split_once('\\')
                            .map(|(drive, _)| drive.to_string())
                            .unwrap_or_else(|| "C:".to_string())
                    })
                } else {
                    Err(std::env::VarError::NotPresent)
                }
            });
        if let Ok(value) = value {
            expanded = replace_case_insensitive(&expanded, &marker, &value);
        }
    }
    expanded
}

/// Windows environment variable names are case-insensitive, so replacement must also tolerate the mixed-case forms returned by the registry.
fn replace_case_insensitive(source: &str, marker: &str, replacement: &str) -> String {
    let source_upper = source.to_ascii_uppercase();
    let marker_upper = marker.to_ascii_uppercase();
    let mut result = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(relative_index) = source_upper[cursor..].find(&marker_upper) {
        let start = cursor + relative_index;
        result.push_str(&source[cursor..start]);
        result.push_str(replacement);
        cursor = start + marker.len();
    }
    result.push_str(&source[cursor..]);
    result
}

fn normalize_clsid(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let value = trimmed
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))?;
    let groups = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = value.split('-').collect();
    if parts.len() != groups.len()
        || parts.iter().zip(groups).any(|(part, expected)| {
            part.len() != expected || !part.chars().all(|ch| ch.is_ascii_hexdigit())
        })
    {
        return None;
    }
    Some(format!("{{{}}}", value.to_ascii_uppercase()))
}

fn registry_path(target: &ShellIconTarget) -> String {
    format!(r"{}\{}", target.hive, target_registry_subkey_path(target))
}

fn target_registry_subkey_path(target: &ShellIconTarget) -> String {
    match target.entry_kind {
        ShellEntryKind::MyComputer => {
            format!(r"{}\{}", MY_COMPUTER_NAMESPACE_PATH, target.clsid)
        }
        ShellEntryKind::NavigationPane => {
            format!(r"{}\{}", NAVIGATION_CLSID_PATH, target.clsid)
        }
    }
}

fn read_acl_sddl(key: &RegKey) -> Result<Option<String>, String> {
    unsafe {
        let mut owner: PSID = std::ptr::null_mut();
        let mut group: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let status = GetSecurityInfo(
            key.raw_handle() as _,
            SE_REGISTRY_KEY,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            &mut group,
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        );
        if status != 0 {
            return Err(format!("读取注册表 ACL 失败: {}", status));
        }
        let mut string_descriptor = std::ptr::null_mut();
        let mut length = 0;
        let converted = sddl::ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1 as u32,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut string_descriptor,
            &mut length,
        );
        if converted == FALSE {
            LocalFree(descriptor);
            return Err(format!("转换注册表 ACL 失败: {}", GetLastError()));
        }
        let value = OsString::from_wide(std::slice::from_raw_parts(
            string_descriptor,
            length as usize,
        ))
        .to_string_lossy()
        .to_string();
        LocalFree(string_descriptor as _);
        LocalFree(descriptor);
        Ok(Some(value))
    }
}

fn add_lock_acl(key: &RegKey) -> Result<(), String> {
    unsafe {
        // Repeated clicks on thorough delete do not append the same ACE again, avoiding the ACL growing along with the backup.
        if read_acl_sddl(key)?
            .as_deref()
            .is_some_and(is_lightc_lock_sddl_ref)
        {
            return Ok(());
        }
        let mut owner: PSID = std::ptr::null_mut();
        let mut group: PSID = std::ptr::null_mut();
        let mut old_dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let status = GetSecurityInfo(
            key.raw_handle() as _,
            SE_REGISTRY_KEY,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            &mut group,
            &mut old_dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        );
        if status != 0 {
            return Err(format!("读取注册表 ACL 失败: {}", status));
        }
        let mut everyone_sid: PSID = std::ptr::null_mut();
        let mut authority = SID_IDENTIFIER_AUTHORITY {
            Value: SECURITY_WORLD_SID_AUTHORITY,
        };
        if AllocateAndInitializeSid(
            &mut authority,
            1,
            SECURITY_WORLD_RID,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            &mut everyone_sid,
        ) != TRUE
        {
            LocalFree(descriptor);
            return Err(format!("创建 Everyone SID 失败: {}", GetLastError()));
        }
        let mut trustee: winapi::um::accctrl::TRUSTEE_W = std::mem::zeroed();
        trustee.TrusteeForm = TRUSTEE_IS_SID;
        trustee.TrusteeType = TRUSTEE_IS_WELL_KNOWN_GROUP;
        trustee.ptstrName = everyone_sid as _;
        let mut entry: EXPLICIT_ACCESS_W = std::mem::zeroed();
        entry.grfAccessPermissions = KEY_SET_VALUE | KEY_CREATE_SUB_KEY | DELETE;
        entry.grfAccessMode = DENY_ACCESS as ACCESS_MODE;
        entry.grfInheritance = NO_INHERITANCE;
        entry.Trustee = trustee;
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        let acl_status = SetEntriesInAclW(1, &mut entry, old_dacl, &mut new_dacl);
        if acl_status != 0 {
            FreeSid(everyone_sid);
            LocalFree(descriptor);
            return Err(format!("构造锁定 ACL 失败: {}", acl_status));
        }
        let set_status = SetSecurityInfo(
            key.raw_handle() as _,
            SE_REGISTRY_KEY,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null_mut(),
        );
        LocalFree(new_dacl as _);
        FreeSid(everyone_sid);
        LocalFree(descriptor);
        if set_status != 0 {
            return Err(format!("应用锁定 ACL 失败: {}", set_status));
        }
        Ok(())
    }
}

fn restore_acl(key: &RegKey, sddl: &str) -> Result<(), String> {
    unsafe {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let mut length = 0;
        if sddl::ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1 as u32,
            &mut descriptor,
            &mut length,
        ) == FALSE
        {
            return Err(format!("解析备份 ACL 失败: {}", GetLastError()));
        }
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut dacl_present = FALSE;
        let mut dacl_defaulted = FALSE;
        let mut owner: PSID = std::ptr::null_mut();
        let mut group: PSID = std::ptr::null_mut();
        let mut owner_defaulted = FALSE;
        let mut group_defaulted = FALSE;
        if winapi::um::securitybaseapi::GetSecurityDescriptorDacl(
            descriptor,
            &mut dacl_present,
            &mut dacl,
            &mut dacl_defaulted,
        ) == FALSE
        {
            LocalFree(descriptor);
            return Err(format!("读取备份 ACL 失败: {}", GetLastError()));
        }
        if winapi::um::securitybaseapi::GetSecurityDescriptorOwner(
            descriptor,
            &mut owner,
            &mut owner_defaulted,
        ) == FALSE
            || winapi::um::securitybaseapi::GetSecurityDescriptorGroup(
                descriptor,
                &mut group,
                &mut group_defaulted,
            ) == FALSE
        {
            LocalFree(descriptor);
            return Err(format!("读取备份所有者失败: {}", GetLastError()));
        }
        let status = SetSecurityInfo(
            key.raw_handle() as _,
            SE_REGISTRY_KEY,
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            owner,
            group,
            if dacl_present == TRUE {
                dacl
            } else {
                std::ptr::null_mut()
            },
            std::ptr::null_mut(),
        );
        LocalFree(descriptor);
        if status != 0 {
            Err(format!("恢复注册表 ACL 失败: {}", status))
        } else {
            Ok(())
        }
    }
}

fn is_lightc_lock_sddl(sddl: String) -> bool {
    is_lightc_lock_sddl_ref(&sddl)
}

fn is_lightc_lock_sddl_ref(sddl: &str) -> bool {
    sddl.split('(')
        .skip(1)
        .filter_map(|part| part.split(')').next())
        .any(|ace| {
            let fields: Vec<&str> = ace.split(';').collect();
            if fields.len() < 6 || fields[0] != "D" || fields[5] != "WD" {
                return false;
            }
            is_lightc_lock_mask(fields[2])
        })
}

fn is_lightc_lock_mask(mask: &str) -> bool {
    let normalized = mask.trim().to_ascii_uppercase();
    if normalized == "DCLCSD" {
        return true;
    }
    normalized
        .strip_prefix("0X")
        .and_then(|value| u32::from_str_radix(value, 16).ok())
        .is_some_and(|value| {
            (value & (KEY_SET_VALUE | KEY_CREATE_SUB_KEY | DELETE))
                == (KEY_SET_VALUE | KEY_CREATE_SUB_KEY | DELETE)
        })
}

/// Remove the Everyone deny ACE LightC itself added from the current security descriptor, leaving other users' ACLs unchanged.
fn remove_lightc_lock_acl(key: &RegKey) -> Result<(), String> {
    let current_sddl = read_acl_sddl(key)?.ok_or_else(|| "读取目标注册表 ACL 失败".to_string())?;
    let unlocked_sddl = remove_lightc_lock_aces_from_sddl(&current_sddl)
        .ok_or_else(|| "目标节点不存在 LightC 防复活 ACL".to_string())?;
    restore_acl(key, &unlocked_sddl)
}

fn remove_lightc_lock_aces_from_sddl(sddl: &str) -> Option<String> {
    let mut result = String::with_capacity(sddl.len());
    let mut cursor = 0;
    let mut removed = false;
    while let Some(relative_open) = sddl[cursor..].find('(') {
        let open = cursor + relative_open;
        let relative_close = sddl[open..].find(')')?;
        let close = open + relative_close;
        let ace = &sddl[open + 1..close];
        result.push_str(&sddl[cursor..open]);
        if is_lightc_lock_ace(ace) {
            removed = true;
        } else {
            result.push_str(&sddl[open..=close]);
        }
        cursor = close + 1;
    }
    result.push_str(&sddl[cursor..]);
    removed.then_some(result)
}

fn is_lightc_lock_ace(ace: &str) -> bool {
    let fields: Vec<&str> = ace.split(';').collect();
    fields.len() >= 6 && fields[0] == "D" && fields[5] == "WD" && is_lightc_lock_mask(fields[2])
}

fn resolve_indirect_string(raw: &str) -> String {
    if !raw.starts_with('@') {
        return raw.to_string();
    }
    let source: Vec<u16> = raw.encode_utf16().chain(std::iter::once(0)).collect();
    let mut output = vec![0u16; 512];
    #[link(name = "shlwapi")]
    extern "system" {
        fn SHLoadIndirectString(
            source: *const u16,
            output: *mut u16,
            length: u32,
            reserved: *mut *const c_void,
        ) -> i32;
    }
    let result = unsafe {
        SHLoadIndirectString(
            source.as_ptr(),
            output.as_mut_ptr(),
            output.len() as u32,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        if let Some(end) = output.iter().position(|value| *value == 0) {
            return OsString::from_wide(&output[..end])
                .to_string_lossy()
                .to_string();
        }
    }
    raw.to_string()
}

fn hidden_command(program: &str, args: &[&str]) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    Command::new(program)
        .args(args)
        .creation_flags(0x08000000)
        .output()
        .map_err(|error| format!("执行 {} 失败: {}", program, error))
}

#[cfg(test)]
mod tests {
    use super::{
        identify_application, identify_known_application, is_lightc_lock_sddl,
        is_system_component_path, normalize_clsid, registry_path,
        remove_lightc_lock_aces_from_sddl, ShellEntryKind, ShellIconTarget,
    };

    #[test]
    fn normalizes_only_guid_clsid() {
        assert_eq!(
            normalize_clsid("{20d04fe0-3aea-1069-a2d8-08002b30309d}"),
            Some("{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string())
        );
        assert!(normalize_clsid("not-a-clsid").is_none());
    }

    #[test]
    fn builds_fixed_namespace_path() {
        let target = ShellIconTarget {
            clsid: "{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}".to_string(),
            hive: "HKCU".to_string(),
            registry_view: "default".to_string(),
            entry_kind: ShellEntryKind::MyComputer,
        };
        assert!(registry_path(&target)
            .contains("MyComputer\\NameSpace\\{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}"));
    }

    #[test]
    fn builds_fixed_navigation_registry_path() {
        let target = ShellIconTarget {
            clsid: "{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}".to_string(),
            hive: "HKLM".to_string(),
            registry_view: "64".to_string(),
            entry_kind: ShellEntryKind::NavigationPane,
        };
        // The navigation pane target must be pinned to the CLSID root path and must not be rewritten by frontend-supplied content.
        assert!(registry_path(&target)
            .contains("Software\\Classes\\CLSID\\{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}"));
    }

    #[test]
    fn detects_lightc_deny_ace() {
        assert!(is_lightc_lock_sddl("D:(D;;0x00010006;;;WD)".to_string()));
        assert!(is_lightc_lock_sddl("D:(D;;DCLCSD;;;WD)".to_string()));
        assert!(!is_lightc_lock_sddl("D:(A;;0x00010006;;;WD)".to_string()));
    }

    #[test]
    fn removes_only_lightc_lock_ace_from_sddl() {
        let sddl = "O:USG:WDD:(D;;DCLCSD;;;WD)(A;;KA;;;BA)";
        let unlocked = remove_lightc_lock_aces_from_sddl(sddl).expect("lock ACE should be found");
        assert_eq!(unlocked, "O:USG:WDD:(A;;KA;;;BA)");
        assert!(remove_lightc_lock_aces_from_sddl("O:USG:WDD:(A;;KA;;;BA)").is_none());
    }

    #[test]
    fn identifies_known_cloud_disk_application() {
        assert_eq!(
            identify_application(
                "{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}",
                "CLSID_BaiduNetdisk",
                Some(r"C:\Program Files\BaiduNetdisk\BaiduNetdiskShellExt.dll")
            ),
            Some("百度网盘".to_string())
        );
    }

    #[test]
    fn identifies_known_application_without_component_path() {
        assert_eq!(
            identify_application("{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}", "百度网盘", None),
            Some("百度网盘".to_string())
        );
    }

    #[test]
    fn keeps_known_application_when_using_system_shell_host() {
        assert_eq!(
            identify_known_application(
                "{679F137C-3162-45DA-BE3C-2F9C3D093F64}",
                "百度网盘",
                Some(r"C:\Windows\System32\shdocvw.dll"),
            ),
            Some("百度网盘")
        );
    }

    #[test]
    fn does_not_identify_application_from_clsid_digits() {
        assert_eq!(
            identify_application(
                "{AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE}",
                "CLSID_Unknown",
                Some(r"C:\Program Files\Vendor\ShellExt.dll"),
            ),
            None
        );
    }

    #[test]
    fn expands_windows_environment_path_before_system_filtering() {
        assert!(is_system_component_path(
            r"%SystemRoot%\System32\shell32.dll"
        ));
    }
}
