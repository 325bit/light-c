//! 目录联接（junction）/ 符号链接（symlink）识别。
//!
//! 为什么需要：用 FolderMove 之类的工具把某个目录搬到别的盘之后，原位置留下的是一个
//! **重解析点**（junction 或 symlink）。扫描器沿它读到的是真实文件，但界面上显示的还是
//! 原来那个盘符 —— 用户会以为"我 C 盘没这么大啊"，也看不出这个目录其实早就不在 C 盘了。
//! 所以这里把「这层链接从哪指到哪」和「最终落地位置」一起解出来，交给界面显示。
//!
//! 只用 kernel32 的四个函数，不引入新依赖：
//! - `GetFileAttributesW` 判断是不是重解析点
//! - `CreateFileW(..., FILE_FLAG_OPEN_REPARSE_POINT)` + `DeviceIoControl(FSCTL_GET_REPARSE_POINT)`
//!   读出 tag，用来区分 junction 与 symlink
//! - 目标字符串交给 `std::fs::read_link`（std 已经处理好 junction 的 `\??\` 前缀）
//!
//! ⚠️ 只做「识别与展示」，**不改变遍历语义**：体积统计仍然不进链接（见 `dev_cache::measure`
//! 里的注释），否则一个指回上级的 junction 会让扫描原地打转。

use serde::Serialize;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const INVALID_FILE_ATTRIBUTES: u32 = 0xFFFF_FFFF;
const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

#[link(name = "kernel32")]
extern "system" {
    fn GetFileAttributesW(name: *const u16) -> u32;
    fn CreateFileW(
        name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security: *mut core::ffi::c_void,
        creation: u32,
        flags: u32,
        template: *mut core::ffi::c_void,
    ) -> *mut core::ffi::c_void;
    fn DeviceIoControl(
        device: *mut core::ffi::c_void,
        code: u32,
        in_buffer: *mut core::ffi::c_void,
        in_size: u32,
        out_buffer: *mut core::ffi::c_void,
        out_size: u32,
        returned: *mut u32,
        overlapped: *mut core::ffi::c_void,
    ) -> i32;
    fn CloseHandle(handle: *mut core::ffi::c_void) -> i32;
}

/// 链接类型。界面上要分开显示 —— junction 删掉只删链接，symlink 语义相近但来源不同，
/// 用户看到具体类型才知道该用哪个工具去改。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkKind {
    /// `mklink /D` 建的目录符号链接
    Symlink,
    /// `mklink /J` 建的目录联接，FolderMove / 手动搬迁最常用
    Junction,
}

/// 一条路径上的链接关系。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkInfo {
    /// 链接类型（前端据此显示"符号链接"/"目录联接"）
    pub kind: LinkKind,
    /// 路径里**哪一层**是链接。
    ///
    /// 通常不是被扫的那一项本身：用户搬走的是 `.workbuddy`，而被扫的是 `.workbuddy\logs`。
    pub link_path: String,
    /// 链接记录的目标（绝对路径，已去掉 `\??\` 前缀）
    pub target: String,
    /// 整条路径跟随所有链接后最终落在哪 —— 也就是空间真正被占用的地方
    pub resolved: String,
}

fn wide(value: &OsStr) -> Vec<u16> {
    let mut units: Vec<u16> = value.encode_wide().collect();
    units.push(0);
    units
}

fn is_reparse_point(path: &Path) -> bool {
    let attrs = unsafe { GetFileAttributesW(wide(path.as_os_str()).as_ptr()) };
    attrs != INVALID_FILE_ATTRIBUTES && attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// 读重解析点的 tag。读不到（权限/不是重解析点）返回 `None`。
///
/// ⚠️ 输出缓冲区必须给足 16 KB（MAXIMUM_REPARSE_DATA_BUFFER_SIZE）：
/// 只给 4~8 字节时 `DeviceIoControl` 会带着 ERROR_MORE_DATA 直接失败，
/// 于是所有链接都被当成"不是链接"——而且静默失败，很难发现。
fn reparse_tag(path: &Path) -> Option<u32> {
    const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;

    let handle = unsafe {
        CreateFileW(
            wide(path.as_os_str()).as_ptr(),
            GENERIC_READ,
            FILE_SHARE_ALL,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() || handle as isize == -1 {
        return None;
    }

    let mut buffer = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_GET_REPARSE_POINT,
            std::ptr::null_mut(),
            0,
            buffer.as_mut_ptr() as *mut core::ffi::c_void,
            buffer.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    unsafe { CloseHandle(handle) };
    if ok == 0 || returned < 4 {
        return None;
    }
    Some(u32::from_le_bytes([
        buffer[0], buffer[1], buffer[2], buffer[3],
    ]))
}

/// 这个路径**本身**是不是链接；返回具体类型。
pub fn kind_of(path: &Path) -> Option<LinkKind> {
    if !is_reparse_point(path) {
        return None;
    }
    match reparse_tag(path) {
        Some(IO_REPARSE_TAG_SYMLINK) => Some(LinkKind::Symlink),
        Some(IO_REPARSE_TAG_MOUNT_POINT) => Some(LinkKind::Junction),
        // 云占位符（OneDrive 按需下载）也是重解析点，但那不是"搬走了"，不要误报
        _ => None,
    }
}

/// 去掉 `\\?\` / `\\?\UNC\` 前缀，统一成用户看得懂的形式。
fn friendly(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{}", rest);
    }
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
}

/// 路径最终落地位置（跟随所有链接）。解析不了就回退到原路径。
fn final_path(path: &Path) -> String {
    match std::fs::canonicalize(path) {
        Ok(real) => friendly(&real),
        Err(_) => friendly(path),
    }
}

/// 分析整条路径上的链接关系。
///
/// 从盘符根往下逐层检查，只报**最靠上那一层**链接 —— 一个被搬走的目录下面再套一层链接
/// 极少见，报第一层既够用又不会把界面塞满。没有链接返回 `None`。
pub fn analyze(path: &Path) -> Option<LinkInfo> {
    // ancestors() 是"从自己往上"，反转过来就是从根往下，才能取到最外层那一层
    let mut layers: Vec<PathBuf> = path.ancestors().map(Path::to_path_buf).collect();
    layers.reverse();

    for layer in layers {
        // 跳过 "C:" 和 "C:\" 这类没有意义的层级
        if layer.to_string_lossy().len() <= 3 {
            continue;
        }
        let Some(kind) = kind_of(&layer) else {
            continue;
        };
        // std::fs::read_link 对 junction 与 symlink 都管用
        let target = std::fs::read_link(&layer)
            .map(|target| friendly(&target))
            .unwrap_or_default();
        return Some(LinkInfo {
            kind,
            link_path: friendly(&layer),
            target,
            resolved: final_path(path),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 普通目录不该被当成链接。
    #[test]
    fn plain_directory_is_not_a_link() {
        let temp = std::env::temp_dir();
        assert!(kind_of(&temp).is_none());
        assert!(analyze(&temp.join("whatever")).is_none());
    }

    /// 本机真实存在的搬迁目录（`.workbuddy` 等）应当能识别出类型和落点。
    ///
    /// 只有「这台机器确实把 `.workbuddy` 搬走过」时才断言；
    /// 没搬过就跳过，不把环境差异算成失败。
    #[test]
    fn detects_real_moved_directory() {
        let Some(home) = std::env::var_os("USERPROFILE") else {
            return;
        };
        let link = PathBuf::from(home).join(".workbuddy");
        // 没搬过（不是重解析点）→ 这条用例没有可验证的对象
        if !is_reparse_point(&link) {
            return;
        }
        let info = analyze(&link.join("logs"))
            .expect("`.workbuddy` 是重解析点，但 analyze() 没识别出来");
        assert!(!info.link_path.is_empty());
        assert!(!info.target.is_empty());
        assert!(!info.resolved.is_empty());
        // 落地位置不该还带着 `\\?\` 前缀，否则界面上很难看
        assert!(!info.resolved.starts_with(r"\\?\"));
        assert!(matches!(info.kind, LinkKind::Symlink | LinkKind::Junction));
    }
}
