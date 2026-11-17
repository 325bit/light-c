//! 扫描与测量。
//!
//! 和 LightC 原有的 `ai_tool_cleaner` 不同：那里是在工具根目录下**找**白名单子目录，
//! 这里每一个目标就是一个确定目录，直接量体积即可。
//!
//! 并行用 rayon 逐层 BFS：一层目录一次 `par_iter` 摊到所有核，收完再进下一层。
//! 不用「递归 + par_iter」，那样会在深层目录炸出指数级任务。

use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::time::Instant;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
use winreg::RegKey;

use super::targets::{self, Env, ProbeHive, RegProbe, VersionPattern, VersionSpec};
use super::types::{
    DevCacheEntry, DevCacheGroup, DevCacheScanResult, DevRisk, VersionEntry, VersionGroup,
};

#[derive(Default)]
struct LevelPart {
    bytes: u64,
    files: usize,
    skipped: usize,
    dirs: Vec<PathBuf>,
}

/// 给绝对路径加 `\\?\` 前缀，绕开 MAX_PATH。
///
/// 同时把正斜杠归一成反斜杠：Win32 的扩展路径 `\\?\` **不认** `C:/Windows`，
/// 而调用方（Git Bash、部分 shell）会把路径悄悄转成正斜杠。
pub fn long_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy().replace('/', "\\");
    if text.starts_with(r"\\?\") {
        return PathBuf::from(text);
    }
    if !Path::new(&text).is_absolute() {
        return PathBuf::from(text);
    }
    if let Some(rest) = text.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{}", rest))
    } else {
        PathBuf::from(format!(r"\\?\{}", text))
    }
}

/// 展示用路径：去掉 `\\?\` 前缀，正斜杠归一。
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('/', "\\");
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", rest)
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text
    }
}

fn read_level(dir: &Path) -> LevelPart {
    let mut part = LevelPart::default();
    let reader = match std::fs::read_dir(long_path(dir)) {
        Ok(reader) => reader,
        Err(_) => {
            // 权限不足或目录被占用：跳过这一层，不让整次扫描失败
            part.skipped += 1;
            return part;
        }
    };
    for entry in reader.filter_map(|item| item.ok()) {
        match entry.file_type() {
            // 符号链接与 junction 的 is_dir() 为 false，天然不会跟进到别的盘
            Ok(kind) if kind.is_dir() => part.dirs.push(entry.path()),
            Ok(kind) if kind.is_file() => match entry.metadata() {
                Ok(meta) => {
                    part.bytes += meta.len();
                    part.files += 1;
                }
                Err(_) => part.skipped += 1,
            },
            _ => {}
        }
    }
    part
}

/// 逐层并行统计目录体积，返回 (字节, 文件数, 跳过数)。
pub fn measure(root: &Path) -> (u64, usize, usize) {
    let mut bytes = 0u64;
    let mut files = 0usize;
    let mut skipped = 0usize;
    let mut frontier: Vec<PathBuf> = vec![root.to_path_buf()];

    while !frontier.is_empty() {
        let parts: Vec<LevelPart> = frontier.par_iter().map(|dir| read_level(dir)).collect();
        let mut next: Vec<PathBuf> = Vec::new();
        for part in parts {
            bytes += part.bytes;
            files += part.files;
            skipped += part.skipped;
            next.extend(part.dirs);
        }
        frontier = next;
    }
    (bytes, files, skipped)
}

/// 完整扫描：工具链缓存 + 多版本残留。
pub fn scan() -> DevCacheScanResult {
    let started = Instant::now();
    let env = Env::detect();

    // 目标之间互不依赖，并行量体积；最慢的那个决定总耗时
    let specs = targets::targets(&env);
    let measured: Vec<(u64, usize, usize)> = specs.par_iter().map(|spec| measure(&spec.path)).collect();

    let mut skipped = 0usize;
    let mut entries: Vec<DevCacheEntry> = specs
        .iter()
        .zip(measured)
        .map(|(spec, (size, file_count, skip))| {
            skipped += skip;
            let exists = spec.path.is_dir();
            DevCacheEntry {
                id: spec.id.to_string(),
                name: spec.name.to_string(),
                path: display_path(&spec.path),
                size,
                file_count,
                risk: spec.risk,
                note: spec.note.to_string(),
                exists,
                // 搬迁过的目录（junction / symlink）要标出真实落点：
                // 否则 `.workbuddy` 明明在 D 盘，界面却说是 `C:\Users\...\.workbuddy`
                link: crate::scanner::reparse::analyze(&spec.path),
            }
        })
        .collect();

    // 目录不存在就整条不展示：那种 0 B 的行只是"这个工具没装"，
    // 摆在列表里既不能勾也占地方，反而让人以为扫描漏了东西。
    // （只读项也一样 —— 只有在它真的存在时，"这里有个东西但不该动"才成立）
    entries.retain(|entry| entry.exists);

    let mut groups = build_groups(&specs, entries);
    // 组内、组间都按体积倒序：第一眼看到的就是最能省的那块
    for group in &mut groups {
        group.entries.sort_by(|a, b| b.size.cmp(&a.size));
    }
    groups.sort_by(|a, b| b.total_size.cmp(&a.total_size));

    let version_groups = scan_versions(&env);
    let version_reclaimable_size = version_groups.iter().map(|g| g.reclaimable_size).sum();
    let version_total_size: u64 = version_groups.iter().map(|g| g.total_size).sum();
    let version_total_files: usize = version_groups
        .iter()
        .flat_map(|g| g.entries.iter())
        .map(|entry| entry.file_count)
        .sum();

    DevCacheScanResult {
        // 总计要**含**多版本目录：界面上"总计占用"和两个页签的体积之和必须对得上，
        // 否则用户会以为版本那块没被算进去（或者被重复算了）
        total_size: groups.iter().map(|g| g.total_size).sum::<u64>() + version_total_size,
        total_files: groups.iter().map(|g| g.file_count).sum::<usize>() + version_total_files,
        safe_size: groups
            .iter()
            .flat_map(|g| g.entries.iter())
            .filter(|entry| entry.risk == DevRisk::Cache)
            .map(|entry| entry.size)
            .sum(),
        groups,
        version_groups,
        version_reclaimable_size,
        elapsed_ms: started.elapsed().as_millis() as u64,
        skipped,
    }
}

/// 按 `spec.group` 归类；分组顺序沿用 targets.rs 里的出现顺序，避免每次刷新顺序乱跳。
fn build_groups(specs: &[targets::TargetSpec], entries: Vec<DevCacheEntry>) -> Vec<DevCacheGroup> {
    const GROUP_NAMES: &[(&str, &str)] = &[
        ("pkg", "包管理器缓存"),
        ("build", "构建与索引缓存"),
        ("logs", "日志与追踪"),
        ("confirm", "需确认的项目"),
        ("other", "其它软件缓存"),
        ("readonly", "只展示 · 请勿删除"),
    ];

    let mut order: Vec<&str> = Vec::new();
    for spec in specs {
        if !order.contains(&spec.group) {
            order.push(spec.group);
        }
    }

    order
        .into_iter()
        .filter_map(|group_id| {
            let group_entries: Vec<DevCacheEntry> = entries
                .iter()
                .filter(|entry| {
                    specs
                        .iter()
                        .any(|spec| spec.id == entry.id && spec.group == group_id)
                })
                .cloned()
                .collect();
            if group_entries.is_empty() {
                return None;
            }
            let name = GROUP_NAMES
                .iter()
                .find(|(id, _)| *id == group_id)
                .map(|(_, name)| name.to_string())
                .unwrap_or_else(|| group_id.to_string());
            Some(DevCacheGroup {
                group_id: group_id.to_string(),
                group_name: name,
                total_size: group_entries.iter().map(|entry| entry.size).sum(),
                file_count: group_entries.iter().map(|entry| entry.file_count).sum(),
                entries: group_entries,
            })
        })
        .collect()
}

/// 扫描多版本目录。父目录本身永远不删，删除粒度只到版本子目录。
fn scan_versions(env: &Env) -> Vec<VersionGroup> {
    let specs = targets::version_specs(env);
    specs
        .into_iter()
        .filter_map(|spec| scan_one_version_group(&spec))
        .collect()
}

/// 列出版本根目录下的「版本号子目录」，按版本号降序（当前版本通常排在最前）。
///
/// 扫描和删除**共用这一份**：之前两边各写了一套，结果同一个"目录名 vs 解析后版本号"
/// 的比对 bug 在两个地方各犯了一次，改一处漏一处。
pub(super) fn list_versions(spec: &VersionSpec) -> Vec<(String, PathBuf)> {
    let Ok(reader) = std::fs::read_dir(long_path(&spec.base)) else {
        return Vec::new();
    };
    let mut versions: Vec<(String, PathBuf)> = Vec::new();
    for entry in reader.filter_map(|item| item.ok()) {
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // 只认版本号目录：`Optimized`（硬链接聚合）、`*.deleting`（卸载中的半成品）
        // 这类非版本目录一律跳过，删了后果不可预期
        if parse_version(&name, &spec.pattern).is_none() {
            continue;
        }
        versions.push((name, entry.path()));
    }
    versions.sort_by(|a, b| compare_version(&b.0, &a.0, &spec.pattern));
    versions
}

/// 判定当前正在使用的版本目录名，以及**凭什么这么判定**（展示给用户看）。
///
/// 优先读注册表；读不到才退化成"版本号最高"，并在 reason 里说清楚是推断。
/// 注册表读到了但目录不存在（刚升级、或刚回滚）也要说明，不能默默当成推断。
pub(super) fn resolve_current(
    spec: &VersionSpec,
    versions: &[(String, PathBuf)],
) -> (Option<String>, String) {
    let fallback = versions.first().map(|(name, _)| name.clone());

    let Some(probed) = spec
        .current_probe
        .as_ref()
        .and_then(probe_current_version)
    else {
        return (fallback, "未读到注册表，按版本号最高推断".to_string());
    };

    // 注册表里记的是纯版本号（`3.6.11`），目录名可能是 `app-3.6.11`，
    // 所以必须拿**解析后的版本号**去比，直接拿目录名比永远对不上
    let matched = versions
        .iter()
        .find(|(name, _)| {
            parse_version(name, &spec.pattern)
                .map(|parsed| parsed.eq_ignore_ascii_case(&probed))
                .unwrap_or(false)
        })
        .map(|(name, _)| name.clone());

    match matched {
        Some(name) => (Some(name), "注册表记录的当前版本".to_string()),
        None => (
            fallback,
            format!("注册表记录 {probed}，但该版本目录不存在，按现存最高版本推断"),
        ),
    }
}

fn scan_one_version_group(spec: &VersionSpec) -> Option<VersionGroup> {
    let versions = list_versions(spec);
    if versions.len() < 2 {
        return None;
    }

    let (current, current_reason) = resolve_current(spec, &versions);

    let sizes: Vec<(u64, usize)> = versions
        .par_iter()
        .map(|(_, path)| {
            let (bytes, files, _) = measure(path);
            (bytes, files)
        })
        .collect();

    let entries: Vec<VersionEntry> = versions
        .iter()
        .zip(sizes)
        .map(|((name, path), (size, file_count))| {
            let is_current = current.as_deref() == Some(name.as_str());
            VersionEntry {
                version: parse_version(name, &spec.pattern).unwrap_or_else(|| name.clone()),
                path: display_path(path),
                size,
                file_count,
                is_current,
                // 判定依据只挂在当前版本那一项上：旧版本没有"为什么不是当前"可讲
                current_reason: if is_current {
                    current_reason.clone()
                } else {
                    String::new()
                },
            }
        })
        .collect();

    let total_size = entries.iter().map(|entry| entry.size).sum();
    let reclaimable_size = entries
        .iter()
        .filter(|entry| !entry.is_current)
        .map(|entry| entry.size)
        .sum();

    Some(VersionGroup {
        id: spec.id.to_string(),
        name: spec.name.to_string(),
        base_path: display_path(&spec.base),
        total_size,
        reclaimable_size,
        entries,
    })
}

/// 目录名 → 版本号字符串。解析不出来返回 None。
///
/// 两种命名形态的段数不一样，所以段数必须按 pattern 分别约束：
/// - `FourPart`（Edge / WebView / Copilot）：整串就是四段版本号，**不许带前缀** ——
///   放宽成「几段都行」会让 `157.0.4341` 这种半截目录、甚至 `1.2` 都被当成版本目录；
/// - `AppPrefix`（GitHub Desktop）：`app-3.6.11` 这种「前缀 + 三段」，**必须带前缀**，
///   否则同级的 `packages` 之类一旦凑巧长得像版本号就会被误认。
pub fn parse_version(name: &str, pattern: &VersionPattern) -> Option<String> {
    let candidate = match pattern {
        VersionPattern::FourPart => {
            if version_segments(name).len() != 4 {
                return None;
            }
            name
        }
        VersionPattern::AppPrefix => {
            // Windows 目录名大小写不敏感，`App-3.6.11` 也要认；
            // 前缀是纯 ASCII，按字节切片不会切到多字节字符中间
            if name.len() < 4 || !name[..4].eq_ignore_ascii_case("app-") {
                return None;
            }
            let rest = &name[4..];
            let count = version_segments(rest).len();
            if !(3..=4).contains(&count) {
                return None;
            }
            rest
        }
    };
    Some(candidate.to_string())
}

/// 按 `.` 切段，全部是纯数字才返回各段，否则返回空 Vec（表示"不是版本号"）。
fn version_segments(value: &str) -> Vec<&str> {
    let parts: Vec<&str> = value.split('.').collect();
    if parts
        .iter()
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        parts
    } else {
        Vec::new()
    }
}

fn version_tuple(value: &str, pattern: &VersionPattern) -> Vec<u64> {
    let candidate = match pattern {
        VersionPattern::FourPart => value,
        VersionPattern::AppPrefix => value.strip_prefix("app-").unwrap_or(value),
    };
    candidate
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn compare_version(a: &str, b: &str, pattern: &VersionPattern) -> std::cmp::Ordering {
    version_tuple(a, pattern).cmp(&version_tuple(b, pattern))
}

/// 目录名是不是「版本号目录」这种形态。删除校验复用同一套判断，
/// 保证「扫得出来」和「删得掉」用的是同一个口径。
pub fn has_version_form(name: &str, pattern: &VersionPattern) -> bool {
    parse_version(name, pattern).is_some()
}

/// 读注册表里的「当前版本号」。
///
/// 同一个键可能在 64 位视图、32 位视图（WOW6432Node）或显式写了 `WOW6432Node` 的
/// 路径下 —— Edge 系就踩过：目录明明存在，却因为只试了一种组合而读不到，
/// 白白退化成"按版本号最高推断"。这里把几种组合都试一遍，读到算数。
fn probe_current_version(probe: &RegProbe) -> Option<String> {
    let hive = match probe.hive {
        ProbeHive::Hklm => HKEY_LOCAL_MACHINE,
    };
    let base = probe.key.replace("SOFTWARE\\WOW6432Node\\", "SOFTWARE\\");

    // 先按 32 位视图（Edge 的 EdgeUpdate 键通常登记在这里），再试 64 位视图
    let attempts: [(&str, u32); 3] = [
        (base.as_str(), KEY_WOW64_32KEY),
        (base.as_str(), KEY_WOW64_64KEY),
        (probe.key, KEY_WOW64_64KEY),
    ];

    for (key_path, view) in attempts {
        let Ok(key) = RegKey::predef(hive).open_subkey_with_flags(key_path, KEY_READ | view) else {
            continue;
        };
        let Ok(value) = key.get_value::<String, _>(probe.value) else {
            continue;
        };
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}
