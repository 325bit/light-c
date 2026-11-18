//! Target table: which directories count as "dev space", and the cost of deleting them.
//!
//! **All paths are built at runtime from environment variables**, and no user-directory literals appear in the source —
//! this is the prerequisite for being safe to push to a remote. To add a new target, just add a block here;
//! scanning, UI, and delete validation need no changes.
//!
//! i18n: UI text is looked up by `id` (`devCache.items.<id>.name/.note`);
//! the `name` / `note` in this file are only Chinese fallbacks and for readable logs.

use std::path::PathBuf;

use super::types::DevRisk;

/// A concrete directory target.
pub struct TargetSpec {
    pub id: &'static str,
    /// Group id, determines which section the card belongs to
    pub group: &'static str,
    pub name: &'static str,
    pub note: &'static str,
    pub risk: DevRisk,
    pub path: PathBuf,
}

/// How to recognize a version directory. Different software names them differently, so a function decides instead of a hard-coded regex.
#[derive(Clone, Copy)]
pub enum VersionPattern {
    /// `1.2.3.4` four-part numbers (Edge / WebView / Copilot)
    FourPart,
    /// `app-1.2.3`（GitHub Desktop）
    AppPrefix,
}

/// A set of "multiple version directories of the same software".
///
/// The parent directory itself is **never deleted**; deletion granularity stops at version subdirectories —
/// this is the boundary that directories like `Edge\Application` must respect.
pub struct VersionSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub base: PathBuf,
    pub pattern: VersionPattern,
    /// Registry key for detecting the current version (falls back to "highest version number is current" if unavailable)
    pub current_probe: Option<RegProbe>,
}

/// Registry probe: read a key's string value as the current version.
pub struct RegProbe {
    pub hive: ProbeHive,
    pub key: &'static str,
    pub value: &'static str,
}

pub enum ProbeHive {
    Hklm,
}

/// Environment variable resolution result. When a variable is missing, give an impossible-to-match placeholder,
/// so subsequent `exists()` is simply false instead of failing the whole scan.
pub struct Env {
    pub user_profile: PathBuf,
    pub local_appdata: PathBuf,
    pub appdata: PathBuf,
    pub program_data: PathBuf,
    pub program_files_x86: PathBuf,
    pub windir: PathBuf,
}

impl Env {
    pub fn detect() -> Self {
        let user_profile = var("USERPROFILE").unwrap_or_else(|| PathBuf::from(r"C:\__no_such_user__"));
        let local_appdata = var("LOCALAPPDATA")
            .unwrap_or_else(|| user_profile.join("AppData").join("Local"));
        let appdata = var("APPDATA").unwrap_or_else(|| user_profile.join("AppData").join("Roaming"));
        let program_data = var("PROGRAMDATA").unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        let program_files_x86 = var("ProgramFiles(x86)")
            .or_else(|| var("ProgramFiles"))
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));
        let windir = var("SystemRoot")
            .or_else(|| var("windir"))
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));

        Self {
            user_profile,
            local_appdata,
            appdata,
            program_data,
            program_files_x86,
            windir,
        }
    }
}

fn var(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// All cleanable targets.
pub fn targets(env: &Env) -> Vec<TargetSpec> {
    let home = &env.user_profile;
    let local = &env.local_appdata;
    let roaming = &env.appdata;

    let mut list = Vec::with_capacity(24);
    let mut add = |id: &'static str,
                   group: &'static str,
                   name: &'static str,
                   note: &'static str,
                   risk: DevRisk,
                   path: PathBuf| {
        list.push(TargetSpec {
            id,
            group,
            name,
            note,
            risk,
            path,
        });
    };

    // ---------------- Package manager caches: re-downloaded on next build ----------------
    add(
        "nuget",
        "pkg",
        "NuGet 全局包缓存",
        "删除后 dotnet restore 会重新下载依赖",
        DevRisk::Cache,
        home.join(".nuget").join("packages"),
    );
    add(
        "cargo-registry",
        "pkg",
        "Cargo 依赖缓存",
        "删除后 cargo 构建会重新下载 crate",
        DevRisk::Cache,
        home.join(".cargo").join("registry"),
    );
    add(
        "gradle",
        "pkg",
        "Gradle 缓存",
        "删除后下次构建重新下载依赖",
        DevRisk::Cache,
        home.join(".gradle").join("caches"),
    );
    add(
        "maven",
        "pkg",
        "Maven 本地仓库",
        "删除后 mvn 会重新下载依赖",
        DevRisk::Cache,
        home.join(".m2").join("repository"),
    );
    add(
        "npm",
        "pkg",
        "npm 缓存",
        "删除后 npm install 重新下载",
        DevRisk::Cache,
        local.join("npm-cache"),
    );
    add(
        "pnpm",
        "pkg",
        "pnpm 存储",
        "删除后 pnpm install 重新下载",
        DevRisk::Cache,
        local.join("pnpm").join("store"),
    );
    add(
        "yarn",
        "pkg",
        "Yarn 缓存",
        "删除后 yarn 重新下载依赖",
        DevRisk::Cache,
        local.join("Yarn").join("Cache"),
    );
    add(
        "pip",
        "pkg",
        "pip 缓存",
        "删除后 pip 重新下载 wheel",
        DevRisk::Cache,
        local.join("pip").join("cache"),
    );

    // ---------------- Build caches: next compile becomes slower ----------------
    add(
        "go-build",
        "build",
        "Go 编译缓存",
        "go clean -cache 的等价操作，下次编译变慢",
        DevRisk::Cache,
        local.join("go-build"),
    );
    add(
        "jetbrains",
        "build",
        "JetBrains 索引缓存",
        "删除后 IDE 下次打开项目会重建索引",
        DevRisk::Cache,
        local.join("JetBrains"),
    );
    add(
        "vscode-cacheddata",
        "build",
        "VS Code 编译快照",
        "CachedData，删除后首次启动稍慢",
        DevRisk::Cache,
        roaming.join("Code").join("CachedData"),
    );
    add(
        "vscode-cache",
        "build",
        "VS Code 运行缓存",
        "Cache 目录，删除后自动重建",
        DevRisk::Cache,
        roaming.join("Code").join("Cache"),
    );

    // ---------------- Logs and traces ----------------
    add(
        "wb-logs",
        "logs",
        "WorkBuddy 日志",
        "纯运行日志，无保留价值",
        DevRisk::Cache,
        home.join(".workbuddy").join("logs"),
    );
    add(
        "wb-traces",
        "logs",
        "WorkBuddy 追踪记录",
        "trace 文件，仅排障时用得上",
        DevRisk::Cache,
        home.join(".workbuddy").join("traces"),
    );
    add(
        "wb-manifests",
        "logs",
        "WorkBuddy 文件树清单",
        "索引缓存，会自动重建",
        DevRisk::Cache,
        home.join(".workbuddy").join("file-tree-manifests"),
    );

    // ---------------- Require confirmation: data will be lost ----------------
    add(
        "android-avd",
        "confirm",
        "Android 模拟器镜像",
        "删了模拟器内的应用与数据一并消失，不可恢复",
        DevRisk::Confirm,
        home.join(".android").join("avd"),
    );
    add(
        "vscode-workspace",
        "confirm",
        "VS Code 工作区状态",
        "删了会丢编辑器布局与未保存的撤销历史",
        DevRisk::Confirm,
        roaming.join("Code").join("User").join("workspaceStorage"),
    );
    add(
        "ollama",
        "confirm",
        "Ollama 运行时",
        "这是程序本体不是模型文件，删了 Ollama 就用不了；模型另在用户目录下",
        DevRisk::Confirm,
        local.join("Programs").join("Ollama"),
    );

    // ---------------- Display only: deletion forbidden ----------------
    add(
        "rustup",
        "readonly",
        "Rust 工具链",
        "rustc / cargo 本体，删了直接无法编译",
        DevRisk::Readonly,
        home.join(".rustup"),
    );
    add(
        "cargo-bin",
        "readonly",
        "Cargo 可执行文件",
        "已安装的 cargo 子命令，删了对应命令失效",
        DevRisk::Readonly,
        home.join(".cargo").join("bin"),
    );
    add(
        "conda",
        "readonly",
        "Conda 环境",
        "虚拟环境本体，删了环境里的包全部失效",
        DevRisk::Readonly,
        home.join(".conda"),
    );
    add(
        "win-installer",
        "readonly",
        "Windows Installer 缓存",
        "各软件的安装包原件，程序卸载/修复/升级都依赖它，手删会让软件既卸不掉也修不了",
        DevRisk::Readonly,
        env.windir.join("Installer"),
    );
    add(
        "package-cache",
        "readonly",
        "Visual Studio 安装源缓存",
        "Package Cache，修复/修改安装时要用，删了要重新下载",
        DevRisk::Readonly,
        env.program_data.join("Package Cache"),
    );
    add(
        "uwp-packages",
        "readonly",
        "商店应用数据",
        "AppData\\Local\\Packages，MSIX 应用的配置与会话都在里面，不提供清理",
        DevRisk::Readonly,
        local.join("Packages"),
    );

    // ---------------- Other software caches: parts not covered by LightC's existing modules ----------------
    //
    // This section supplements the "union": directories cscan can find but neither LightC's junk cleanup (18 types of
    // Windows/browser caches) nor driver cleanup (only scans System32\drivers) takes care of.
    // Social app cleanup (WeChat/QQ) already covers these, so they are not listed again here.
    add(
        "nvidia-downloader",
        "other",
        "NVIDIA 驱动下载缓存",
        "驱动装完就没用的安装包，删了不影响已装驱动",
        DevRisk::Cache,
        env.program_data.join("NVIDIA Corporation").join("Downloader"),
    );
    add(
        "nvidia-app-ota",
        "other",
        "NVIDIA App 驱动包",
        "UpdateFramework 里的 ota 安装包，删了驱动回滚时需要重下",
        DevRisk::Cache,
        env.program_data
            .join("NVIDIA Corporation")
            .join("NVIDIA App")
            .join("UpdateFramework"),
    );
    add(
        "steam-htmlcache",
        "other",
        "Steam 内置浏览器缓存",
        "不影响游戏库与下载，删了商店页首次加载稍慢",
        DevRisk::Cache,
        local.join("Steam").join("htmlcache"),
    );
    add(
        "overwolf-dumps",
        "other",
        "Overwolf 崩溃转储",
        "崩溃 dump，纯调试用途",
        DevRisk::Cache,
        local.join("Overwolf").join("CrashDumps"),
    );
    add(
        "epic-webcache",
        "other",
        "Epic 启动器网页缓存",
        "webcache，删了商店页重新加载",
        DevRisk::Cache,
        local.join("EpicGamesLauncher").join("Saved").join("webcache"),
    );
    add(
        "overwolf",
        "other",
        "Overwolf 数据",
        "整个应用数据目录，删了登录状态与插件配置一起没",
        DevRisk::Confirm,
        local.join("Overwolf"),
    );
    add(
        "utools",
        "other",
        "uTools 数据",
        "插件与本地索引，删了要重新装插件",
        DevRisk::Confirm,
        roaming.join("uTools"),
    );
    add(
        "baidunetdisk",
        "other",
        "百度网盘缓存",
        "下载缓存与日志，删了不影响云端文件",
        DevRisk::Confirm,
        roaming.join("baidunetdisk"),
    );
    add(
        "thunderstore",
        "other",
        "Thunderstore Mod Manager",
        "Mod 配置与缓存，删了要重新登录并重下 Mod",
        DevRisk::Confirm,
        roaming.join("Thunderstore Mod Manager"),
    );
    add(
        "faceit",
        "other",
        "FACEIT 反作弊客户端",
        "删了需要重新安装才能进服",
        DevRisk::Confirm,
        local.join("FACEIT"),
    );
    add(
        "fivee",
        "other",
        "5E 对战平台",
        "登录态与配置，删了要重新登录",
        DevRisk::Confirm,
        roaming.join("5E对战平台"),
    );
    add(
        "astrbot",
        "other",
        "AstrBot 启动器",
        "实例配置，删了机器人实例需要重建",
        DevRisk::Confirm,
        local
            .join("com.github.raven95676.astrbot-launcher"),
    );
    add(
        "onedrive",
        "other",
        "OneDrive 本地缓存",
        "云盘缓存，删了需要重新同步（不要删整个 OneDrive 目录）",
        DevRisk::Confirm,
        local.join("Microsoft").join("OneDrive"),
    );
    add(
        "ps-remote",
        "other",
        "PS Remote Play 缓存",
        "安装源缓存，删了修复安装要重新下载",
        DevRisk::Confirm,
        env.program_data.join("Sony Interactive Entertainment Inc"),
    );
    add(
        "davinci",
        "other",
        "DaVinci Resolve 缓存",
        "安装源与项目缓存，删了修复安装要重新下载",
        DevRisk::Confirm,
        env.program_data.join("Blackmagic Design"),
    );
    add(
        "epic",
        "other",
        "Epic 安装缓存",
        "安装源缓存，删了修复安装要重新下载",
        DevRisk::Confirm,
        env.program_data.join("Epic"),
    );

    list
}

/// Multi-version directories.
pub fn version_specs(env: &Env) -> Vec<VersionSpec> {
    let local = &env.local_appdata;
    let pf86 = &env.program_files_x86;
    let ms = pf86.join("Microsoft");

    vec![
        VersionSpec {
            id: "edge-core",
            name: "Microsoft Edge（EdgeCore）",
            base: ms.join("EdgeCore"),
            pattern: VersionPattern::FourPart,
            // EdgeUpdate client GUID of the Stable channel
            // (EdgeCore is released at the same version as the main app and shares the same client registration)
            current_probe: Some(RegProbe {
                hive: ProbeHive::Hklm,
                key: r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{56EB18F8-B008-4CBD-B6D2-8C97FE7E9062}",
                value: "pv",
            }),
        },
        VersionSpec {
            id: "edge-app",
            name: "Microsoft Edge（主程序）",
            base: ms.join("Edge").join("Application"),
            pattern: VersionPattern::FourPart,
            current_probe: Some(RegProbe {
                hive: ProbeHive::Hklm,
                key: r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{56EB18F8-B008-4CBD-B6D2-8C97FE7E9062}",
                value: "pv",
            }),
        },
        VersionSpec {
            id: "edge-webview",
            name: "Edge WebView2 运行时",
            base: ms.join("EdgeWebView").join("Application"),
            pattern: VersionPattern::FourPart,
            current_probe: Some(RegProbe {
                hive: ProbeHive::Hklm,
                key: r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}",
                value: "pv",
            }),
        },
        VersionSpec {
            id: "copilot",
            name: "Microsoft Copilot",
            base: ms.join("Copilot").join("Application"),
            pattern: VersionPattern::FourPart,
            current_probe: Some(RegProbe {
                hive: ProbeHive::Hklm,
                key: r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{C50565E9-CCCF-44B4-BA15-5AC5C6569197}",
                value: "pv",
            }),
        },
        VersionSpec {
            id: "github-desktop",
            name: "GitHub Desktop",
            base: local.join("GitHubDesktop"),
            pattern: VersionPattern::AppPrefix,
            current_probe: None,
        },
    ]
}
