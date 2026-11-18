//! Tool catalog.
//!
//! **To add a tool, just add an entry here**: no changes needed to scanning, UI, or deletion.
//! Each tool gets multiple candidate root directories, because the same tool lives in different places under different install methods
//! (e.g., dsh desktop in AppData, CLI cache in a dot-directory under the user profile).

use std::path::{Path, PathBuf};

/// A candidate data root directory for a tool.
///
/// Multiple candidates are used instead of a single one because the same tool lives in different places under different install methods:
/// for example, dsh's desktop cache is in AppData while its CLI cache is in a dot-directory under the user profile.
pub(super) struct ToolSpec {
    pub(super) id: &'static str,
    pub(super) name: &'static str,
    pub(super) roots: Vec<PathBuf>,
    /// Extra cleanable directory names that apply only to **this tool**.
    ///
    /// The global whitelist only includes directories whose "name itself indicates cache"; names like `.tmp` may be
    /// important staging areas in other tools, so authorize per tool rather than adding them to the global list.
    pub(super) extra_cleanable_names: &'static [&'static str],
}

/// Tool catalog.
///
/// Low-user-count tools like Trae are postponed; Codex and dsh are prioritized based on user feedback.
/// To add a tool, just add an entry here; scanning and UI need no changes.
pub(super) fn build_specs(
    appdata: &Path,
    local_appdata: &Path,
    user_profile: &Path,
) -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "claude-code",
            name: "Claude Code",
            roots: vec![user_profile.join(".claude")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "codex",
            name: "Codex",
            roots: vec![user_profile.join(".codex")],
            // `.tmp` is Codex's atomic-replacement staging area used when updating plugins and the marketplace, measured at 84MB.
            // After deletion, the next sync re-unpacks it without affecting installed plugins; applies only to Codex.
            extra_cleanable_names: &[".tmp"],
        },
        ToolSpec {
            id: "dsh",
            name: "DeepSeek（dsh）",
            roots: vec![
                user_profile.join(".dsh"),
                // The wallpaper engine's cache is stored separately in a sibling directory and is often the largest
                user_profile.join(".dsh-wallpaper-engine"),
                appdata.join("@deepseek-ai").join("dsh-desktop"),
            ],
            extra_cleanable_names: &[],
        },
        // The VS Code shell family shares the same set of cache directory names (see the whitelist in core),
        // so one whitelist covers them all; adding a tool is just a few lines of paths.
        ToolSpec {
            id: "cursor",
            name: "Cursor",
            // Scan Roaming only. ~/.cursor under the user profile holds cli-config.json, plugins, and
            // worktrees (git worktrees contain the user's uncommitted code), which have no cache and should not be deleted by us.
            roots: vec![appdata.join("Cursor")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "trae",
            name: "Trae",
            // The official docs list the international, domestic, and SOLO editions as three separate app names; try all of them,
            // nonexistent ones are skipped automatically. ~/.trae-cn holds project snapshots and extensions, so do not scan it.
            roots: vec![
                appdata.join("Trae"),
                appdata.join("Trae CN"),
                appdata.join("TRAE SOLO CN"),
            ],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "workbuddy",
            name: "WorkBuddy",
            roots: vec![
                local_appdata.join("WorkBuddy"),
                appdata.join("WorkBuddy"),
                // The CLI writes its cache in a dot-directory under the user profile, a separate set from the desktop's AppData
                user_profile.join(".workbuddy"),
            ],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "devin",
            name: "Devin",
            roots: vec![appdata.join("devin")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "windsurf",
            name: "Windsurf",
            // ~/.windsurf only has extensions and plans (drafts); the caches are all in the Roaming copy
            roots: vec![appdata.join("Windsurf")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "codeium",
            name: "Codeium / Windsurf",
            roots: vec![user_profile.join(".codeium")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "continue",
            name: "Continue",
            roots: vec![user_profile.join(".continue")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "codebuddy",
            name: "CodeBuddy",
            roots: vec![user_profile.join(".codebuddy")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "gemini",
            name: "Gemini CLI",
            roots: vec![user_profile.join(".gemini")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "antigravity",
            name: "Antigravity",
            roots: vec![appdata.join("Antigravity")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "kimi",
            name: "Kimi",
            roots: vec![appdata.join("kimi-desktop")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            id: "doubao",
            name: "豆包（Doubao）",
            // The desktop app is a Chromium shell; caches are all under `User Data\Default`
            // (`Cache` 55MB / `Code Cache` / `GPUCache` / top-level `GrShaderCache`),
            // structurally identical to Cursor / Trae, so **no whitelist extension is needed**; just add a root directory.
            //
            // ⚠️ Of the 1.37GB under this directory, about 1.1GB is the AI sandbox's runtime, not cache:
            // - `sandbox_runtime\bases` (617MB) extracted node/python/pwsh runtimes
            // - `sandbox_runtime\downloads` (234MB) installers for those runtimes
            // - `Default\sandbox_envs_dir` (274MB) per-session environments
            // The first two are cleanable (`downloads` see extra_cleanable_names below),
            // `bases` is the runtime currently in use; deleting it breaks functionality outright - never touch it.
            //
            // Also, `gecko_cache` (19MB) looks like a cache by name but contains the `gecko2.db` database;
            // `DoubaoLinkRouter` is a standalone routing component (only bin and telemetry); neither has cleanable items.
            roots: vec![local_appdata.join("Doubao")],
            // `downloads` holds the installers for `sandbox_runtime\bases` (234MB on this machine),
            // already-extracted runtimes are unaffected; deleting only means re-downloading at next sandbox initialization.
            // Not in the global whitelist: in other tools this name is very likely the user's downloads directory.
            extra_cleanable_names: &["downloads"],
        },
        // Domestic CLI tools: directory names follow public conventions; not installed locally, so untested,
        // the only consequence of a wrong path is that the tool does not appear; no false deletion.
        ToolSpec {
            id: "qwen",
            name: "Qwen Code",
            roots: vec![user_profile.join(".qwen")],
            extra_cleanable_names: &[],
        },
        ToolSpec {
            // xAI's official terminal coding assistant is called Grok Build (not the community grok-cli),
            // everything lives in ~/.grok: bin holds binaries, auth.json credentials, sessions chat sessions,
            // only cache-type directories will match the whitelist
            id: "grok",
            name: "Grok Build",
            roots: vec![user_profile.join(".grok")],
            extra_cleanable_names: &[],
        },
    ]
}
