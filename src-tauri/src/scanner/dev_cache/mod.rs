//! 开发空间清理（cscan 能力的 light-c 版）。
//!
//! 定位：补齐 light-c 原有模块**没有覆盖**的那部分空间 ——
//! 开发工具链缓存（NuGet / Cargo / Gradle / Go / JetBrains…）、
//! 同一软件的多套旧版本目录（Edge 的 4 套 EdgeCore 之类），
//! 以及 cscan 扫得出、但垃圾清理与驱动清理都不管的其它软件缓存。
//!
//! 与既有模块的分工（不重复计数）：
//! - 浏览器 / 临时文件 / 回收站 / 缩略图 / 更新缓存 → `junk_clean`、`leftovers`
//! - 已安装驱动本体 → `driver_cleanup`（本模块只管**下载下来的驱动安装包**）
//! - 微信 / QQ 等聊天数据 → `social_scanner`
//! - 大文件与目录占用分布 → `big_files`、`hotspot`
//!
//! 所有路径在运行时由环境变量解析，源码不含任何个人目录字面量。

mod delete;
mod measure;
mod targets;
mod types;

// 导出名加上模块前缀：`scanner/mod.rs` 会把各子模块 `pub use` 到同一层，
// 直接叫 `scan` / `delete` 会和别处的同名符号撞车。
pub use delete::delete as delete_dev_cache_paths;
pub use measure::scan as scan_dev_cache;
// 只有结果类型需要跨模块（commands 层）用到，其余结构体在子模块内部互相引用即可；
// 全部 re-export 会触发 unused_imports 警告。
pub use types::{DevCacheDeleteResult, DevCacheScanResult};

#[cfg(test)]
mod tests {
    use super::measure::{display_path, has_version_form, long_path};
    use super::targets::VersionPattern;
    use std::path::Path;

    /// Edge / WebView / Copilot 用的是四段版本号，`Optimized` 这种绝不能被当成版本目录 ——
    /// 它可能是硬链接聚合目录，删了后果不可预期。
    #[test]
    fn four_part_versions() {
        assert!(has_version_form("157.0.4341.60", &VersionPattern::FourPart));
        assert!(has_version_form("155.0.4283.62", &VersionPattern::FourPart));
        assert!(!has_version_form("Optimized", &VersionPattern::FourPart));
        assert!(!has_version_form("157.0.4341", &VersionPattern::FourPart));
        assert!(!has_version_form("1.2.3.4.5", &VersionPattern::FourPart));
        assert!(!has_version_form("157.0.4341.x", &VersionPattern::FourPart));
    }

    /// GitHub Desktop 的目录名是 `app-3.6.11`：三段 + 必须带前缀。
    ///
    /// 这条以前写错过 —— 老实现不管 pattern 一律要求四段，于是 `app-3.6.11`
    /// 被剥掉前缀后只剩三段、直接判成"不是版本目录"，整个 GitHub Desktop 组扫不出来。
    #[test]
    fn github_desktop_versions() {
        assert!(has_version_form(
            "app-3.6.11",
            &VersionPattern::AppPrefix
        ));
        // Windows 目录名大小写不敏感，前缀换了大小写也要认
        assert!(has_version_form(
            "App-3.6.11",
            &VersionPattern::AppPrefix
        ));
        assert!(has_version_form(
            "app-3.6.11.1",
            &VersionPattern::AppPrefix
        ));
        // 没有前缀的一律不认：同级还躺着 `packages` 这类目录，
        // 放宽成"只要长得像版本号就算"会误判
        assert!(!has_version_form("3.6.11", &VersionPattern::AppPrefix));
        // 两段太短，`-unpacked` 之类的派生目录带非数字
        assert!(!has_version_form("app-3.6", &VersionPattern::AppPrefix));
        assert!(!has_version_form(
            "app-3.6.11-unpacked",
            &VersionPattern::AppPrefix
        ));
        assert!(!has_version_form("packages", &VersionPattern::AppPrefix));
    }

    /// `\\?\` 前缀不认正斜杠：调用方传来 `C:/Windows` 时必须归一，
    /// 否则 read_dir 直接失败、界面会显示 0 B。
    #[test]
    fn path_normalization() {
        assert_eq!(
            long_path(Path::new("C:/Windows/Temp")).to_string_lossy(),
            r"\\?\C:\Windows\Temp"
        );
        assert_eq!(
            display_path(Path::new(r"\\?\C:\Windows\Temp")),
            r"C:\Windows\Temp"
        );
        // 已有前缀不重复添加
        assert_eq!(
            long_path(Path::new(r"\\?\C:\Windows")).to_string_lossy(),
            r"\\?\C:\Windows"
        );
    }

    /// 删除校验必须挡住"范围外"的路径。
    ///
    /// 这条测的是**真删函数**，只是喂给它的都是不可能命中的路径 ——
    /// 一旦三层校验哪天被改漏了，这里会先把问题暴露出来，而不是等到用户手滑。
    #[test]
    fn delete_rejects_paths_outside_scope() {
        let result = super::delete_dev_cache_paths(&[
            r"C:\definitely-not-a-dev-cache-target".to_string(),
            r"C:\code\..\Windows\System32\drivers".to_string(),
        ]);
        assert_eq!(result.deleted, 0, "范围外路径不该删掉任何东西");
        assert_eq!(result.freed, 0);
        assert_eq!(result.failed.len(), 2);
        assert!(result
            .failed
            .iter()
            .all(|failure| !failure.reason.is_empty()));
    }

    /// 手动 smoke test：拿真实机器上的目录跑一遍扫描，把结果打出来。
    ///
    /// 不进默认用例池（`#[ignore]`）—— 它会真的遍历几 GB 的包缓存，且结果依赖本机装了什么。
    /// 跑法：`cargo test --lib smoke_scan_real_machine -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn smoke_scan_real_machine() {
        let result = super::scan_dev_cache();
        println!(
            "总计 {}（{} 个文件）/ 可直接清理 {} / 版本可回收 {} / 耗时 {}ms / 跳过 {}",
            result.total_size,
            result.total_files,
            result.safe_size,
            result.version_reclaimable_size,
            result.elapsed_ms,
            result.skipped
        );
        for group in &result.groups {
            println!(
                "[{}] {}  {}  {} 项",
                group.group_id,
                group.group_name,
                group.total_size,
                group.entries.len()
            );
            for entry in &group.entries {
                let link_note = match &entry.link {
                    Some(link) => format!(
                        "  ⤴ {:?} {} → {}（实体 {}）",
                        link.kind, link.link_path, link.target, link.resolved
                    ),
                    None => String::new(),
                };
                println!(
                    "    {:?} {:>12}  {}  {}{}",
                    entry.risk, entry.size, entry.name, entry.path, link_note
                );
            }
        }
        for group in &result.version_groups {
            println!(
                "[ver:{}] {}  {}  可回收 {}  基目录 {}",
                group.id, group.name, group.total_size, group.reclaimable_size, group.base_path
            );
            for entry in &group.entries {
                println!(
                    "    {} {:>12} 当前={} {}",
                    entry.version, entry.size, entry.is_current, entry.current_reason
                );
            }
        }

        // 结构性不变量：每组最多一个"当前版本"，可回收量必须小于总量
        for group in &result.version_groups {
            let current = group.entries.iter().filter(|entry| entry.is_current).count();
            assert!(current <= 1, "{} 出现 {} 个当前版本", group.name, current);
            assert!(group.reclaimable_size <= group.total_size);
        }
    }
}
