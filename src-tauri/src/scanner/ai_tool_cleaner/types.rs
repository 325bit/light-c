//! AI 工具清理的数据结构。
//!
//! 只放 serde 结构体，不放逻辑 —— 前端 `src/types/aiToolCleanup.ts` 与本文件一一对应。

use serde::Serialize;

use crate::scanner::reparse::LinkInfo;

/// 单个缓存目录的统计结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCacheEntry {
    /// 目录名（用于界面展示，如 "Cache" / "media_cache"）
    pub name: String,
    pub path: String,
    pub size: u64,
    pub file_count: usize,
    /// 这条路径上有没有 junction / symlink（用 FolderMove 之类把目录搬到别的盘之后，
    /// 原地留下的是链接）。有值时界面要同时显示**真实落点**，否则用户看到 `C:\...`
    /// 会以为自己 C 盘被占了这么多。没有链接时不序列化，省得前端到处判空对象。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<LinkInfo>,
}

/// 缓存目录里的单个文件，供明细弹窗按需拉取。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCacheFile {
    pub path: String,
    pub size: u64,
    /// 最后修改时间（Unix 秒）。明细弹窗的「按修改时间排序」用它；
    /// 读不到按 0 传，排序时垫底。
    pub modified_time: u64,
}

/// 明细弹窗的文件列表。
///
/// `truncated` 是必须的：目录规模不可控，截断后不告诉用户，
/// 他会以为"就这么多文件"，和大小的差距对不上。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCacheFileList {
    pub files: Vec<AiCacheFile>,
    pub truncated: bool,
}

/// 一个工具的汇总。界面按工具分组展示，用户先看到"哪个工具占了多少"。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiToolGroup {
    pub tool_id: String,
    pub tool_name: String,
    pub total_size: u64,
    pub file_count: usize,
    /// 该工具下命中的缓存目录，按体积从大到小
    pub entries: Vec<AiCacheEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiToolScanResult {
    pub groups: Vec<AiToolGroup>,
    pub total_size: u64,
    pub total_files: usize,
}

/// 单个删除失败的目录。
///
/// 必须带原因：工具开着时缓存文件被占用是最常见的失败场景，
/// 界面要能说清"是没删掉，不是没东西可删"。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCacheDeleteFailure {
    pub path: String,
    pub reason: String,
}

/// 批量删除的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCacheDeleteResult {
    /// 成功删掉的目录数
    pub deleted: usize,
    /// 没删掉的目录及原因
    pub failed: Vec<AiCacheDeleteFailure>,
}
