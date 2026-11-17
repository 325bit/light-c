//! 开发空间清理的数据结构。
//!
//! 只放 serde 结构体，不放逻辑 —— 前端 `src/types/devCache.ts` 与本文件一一对应。
//!
//! ⚠️ 所有路径都在**运行时**由环境变量拼出来，源码里不出现任何个人目录字面量，
//! 这样仓库推到远端也不会带出本机信息。

use serde::Serialize;

use crate::scanner::reparse::LinkInfo;

/// 单项的风险等级。前端据此决定默认是否勾选、要不要显示黄条。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DevRisk {
    /// 纯缓存，删掉之后下次运行/构建会重新生成，无感。
    Cache,
    /// 会丢配置或数据（模拟器镜像、工作区状态、模型），必须用户确认。
    Confirm,
    /// 只展示，永远不给勾选框。
    Readonly,
}

/// 一个可清理项（一个具体目录）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCacheEntry {
    /// 稳定 id，前端与删除校验都用它
    pub id: String,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub file_count: usize,
    pub risk: DevRisk,
    /// 一句话说明「删掉之后会怎样」，界面上常驻显示
    pub note: String,
    /// 目录当前是否存在（不存在时不算失败，只是没装这个工具）
    pub exists: bool,
    /// 路径上有没有 junction / symlink（搬迁工具留下的）。
    /// 有值时界面必须同时显示真实落点，否则 `C:\...` 会让人以为空间记在 C 盘。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkInfo>,
}

/// 一个分组（按工具链归类），界面上一层折叠卡片。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCacheGroup {
    pub group_id: String,
    pub group_name: String,
    pub total_size: u64,
    pub file_count: usize,
    pub entries: Vec<DevCacheEntry>,
}

/// 多版本目录里的一个版本。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    pub path: String,
    /// 从目录名解析出的版本号，解析不出来就原样放目录名
    pub version: String,
    pub size: u64,
    pub file_count: usize,
    /// 正在被使用的那一版：界面上标出来并禁止勾选
    pub is_current: bool,
    /// 当前版本是靠什么判定的，展示给用户看，避免"凭版本号猜"被当成事实
    pub current_reason: String,
}

/// 一组同款多版本目录（如 Edge 的 EdgeCore 下有 5 个版本目录）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionGroup {
    pub id: String,
    pub name: String,
    pub base_path: String,
    pub total_size: u64,
    /// 除当前版本外可删的合计
    pub reclaimable_size: u64,
    pub entries: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCacheScanResult {
    pub groups: Vec<DevCacheGroup>,
    /// 工具缓存 + 多版本目录的**合计**：界面上"总计占用"和两个页签之和要对得上
    pub total_size: u64,
    pub total_files: usize,
    /// 风险为 Cache、可直接清理的合计（不含 Confirm / Readonly，也不含多版本目录 ——
    /// 旧版本删除要用户逐个确认，不能混进"可直接清理"）
    pub safe_size: u64,
    pub version_groups: Vec<VersionGroup>,
    pub version_reclaimable_size: u64,
    pub elapsed_ms: u64,
    /// 扫描过程中跳过的路径数（权限不足/被占用），用于解释"为什么比预期小"
    pub skipped: usize,
}

/// 单个删除失败项。必须带原因：文件被占用是最常见的情况，
/// 只回成功数会让界面显示"已删完"而实际占用没降。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCacheDeleteFailure {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevCacheDeleteResult {
    pub deleted: usize,
    pub freed: u64,
    pub failed: Vec<DevCacheDeleteFailure>,
}
