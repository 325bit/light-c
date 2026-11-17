//! 删除与路径校验。
//!
//! 删除不可逆，所以前端传来的路径**一律不可信**，这里全部重新校验：
//! ① 不含 `.` / `..` 组件 ② 必须精确等于某个目标目录（缓存项），
//! 或位于某个版本根目录**之下且目录名是版本号**（版本项）
//! ③ 只读项、以及任何版本根目录本身，一律拒绝。

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::measure::{display_path, measure};
use super::targets::{self, Env, VersionPattern};
use super::types::{DevCacheDeleteFailure, DevCacheDeleteResult, DevRisk};

/// 归一化：去扩展前缀、正斜杠转反斜杠、去尾部分隔符、转小写。
/// 用来做「字符串层面」的相等判断，避免大小写和分隔符差异导致的漏判。
fn norm(raw: &str) -> String {
    let mut text = display_path(Path::new(raw));
    while text.ends_with('\\') && text.len() > 3 {
        text.pop();
    }
    text.to_ascii_lowercase()
}

fn has_relative_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

pub fn delete(paths: &[String]) -> DevCacheDeleteResult {
    let env = Env::detect();
    let specs = targets::targets(&env);

    // 可删集合：只收 Cache / Confirm。Readonly 永远进不来。
    let mut deletable: HashSet<String> = HashSet::new();
    // 只读集合单独留着，命中时要给出「为什么不能删」而不是笼统的「不在范围内」
    let mut readonly: HashSet<String> = HashSet::new();
    for spec in &specs {
        let key = norm(&display_path(&spec.path));
        match spec.risk {
            DevRisk::Readonly => {
                readonly.insert(key);
            }
            _ => {
                deletable.insert(key);
            }
        }
    }

    // 版本项：允许删，但必须严格是「版本根目录之下的版本号目录」，且不能是当前版本
    let mut version_bases: Vec<(String, VersionPattern)> = Vec::new();
    let mut protected_versions: HashSet<String> = HashSet::new();
    for spec in targets::version_specs(&env) {
        version_bases.push((norm(&display_path(&spec.base)), spec.pattern.clone()));
        if let Some(current) = current_version_path(&spec) {
            protected_versions.insert(norm(&current));
        }
    }

    let mut deleted = 0usize;
    let mut freed = 0u64;
    let mut failed = Vec::new();

    for raw in paths {
        let path = PathBuf::from(raw);

        if has_relative_component(&path) {
            failed.push(DevCacheDeleteFailure {
                path: raw.clone(),
                reason: "路径包含 . 或 .. 组件".to_string(),
            });
            continue;
        }

        let key = norm(raw);

        if readonly.contains(&key) {
            failed.push(DevCacheDeleteFailure {
                path: raw.clone(),
                reason: "该项标记为只展示，不提供删除".to_string(),
            });
            continue;
        }

        let is_target = deletable.contains(&key);
        let is_version = if is_target {
            false
        } else {
            match_version_entry(&key, &version_bases)
        };

        if !is_target && !is_version {
            failed.push(DevCacheDeleteFailure {
                path: raw.clone(),
                reason: "不在可清理范围内".to_string(),
            });
            continue;
        }
        if is_version && protected_versions.contains(&key) {
            failed.push(DevCacheDeleteFailure {
                path: raw.clone(),
                reason: "这是当前正在使用的版本".to_string(),
            });
            continue;
        }

        if !path.exists() {
            failed.push(DevCacheDeleteFailure {
                path: raw.clone(),
                reason: "目录不存在".to_string(),
            });
            continue;
        }

        // 先量体积再删，删完就量不出来了
        let (before, _, _) = measure(&path);
        match std::fs::remove_dir_all(super::measure::long_path(&path)) {
            Ok(()) => {
                deleted += 1;
                freed += before;
                log::info!("已删除开发空间目录: {raw}（{} 字节）", before);
            }
            Err(error) => {
                // 目录被占用是常见情况（IDE / 构建进程还开着），记录后继续删其它项
                log::warn!("删除失败 {raw}: {error}");
                failed.push(DevCacheDeleteFailure {
                    path: raw.clone(),
                    reason: error.to_string(),
                });
            }
        }
    }

    DevCacheDeleteResult {
        deleted,
        freed,
        failed,
    }
}

/// 判断路径是不是「版本根目录 / 版本号目录」这种形态。
fn match_version_entry(key: &str, bases: &[(String, VersionPattern)]) -> bool {
    for (base, pattern) in bases {
        let Some(rest) = key.strip_prefix(base.as_str()) else {
            continue;
        };
        // 必须是 base\ 开头，不能是 base 本身（那是父目录，绝不能删）
        let Some(name) = rest.strip_prefix('\\') else {
            continue;
        };
        // 只允许一层：`base\1.2.3.4`，再深就不认了
        if name.is_empty() || name.contains('\\') {
            continue;
        }
        if super::measure::has_version_form(name, pattern) {
            return true;
        }
    }
    false
}

/// 读出某组当前正在使用的版本目录（绝对路径）。
///
/// 删除时重新算一遍，而不复用扫描结果 —— 两次操作之间用户可能更新过软件。
/// 列目录和判定当前版本的逻辑与扫描侧**完全共用**，不在这里重写一遍。
fn current_version_path(spec: &targets::VersionSpec) -> Option<String> {
    let versions = super::measure::list_versions(spec);
    if versions.is_empty() {
        return None;
    }
    let (name, _) = super::measure::resolve_current(spec, &versions);
    let name = name?;
    versions
        .iter()
        .find(|(candidate, _)| candidate == &name)
        .map(|(_, path)| display_path(path))
}
