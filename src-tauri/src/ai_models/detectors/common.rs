use crate::ai_models::model_file_rules::{
    is_model_package_directory, is_supported_model_extension,
};
use crate::ai_models::types::{AssetSource, ModelItem};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

pub fn user_home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

pub fn normalize_existing_path(path: PathBuf) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }

    // canonicalize merges different spellings of the same path; on failure, keep the original path so a permission issue doesn't abort the scan.
    Some(path.canonicalize().unwrap_or(path))
}

pub fn unique_existing_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut unique_paths = Vec::new();

    for path in paths {
        if let Some(existing_path) = normalize_existing_path(path) {
            let key = existing_path.to_string_lossy().to_lowercase();
            if seen.insert(key) {
                unique_paths.push(existing_path);
            }
        }
    }

    unique_paths
}

pub fn directory_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .sum()
}

/// Read a file's real usage: symlinks must be followed to the target file.
///
/// In caches of tools like HuggingFace, real files live under blobs while model directories are all symlinks.
/// On Windows a symlink's own length is 0, so using symlink_metadata would count a data-bearing model as 0 bytes.
pub fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
}

/// Safety cap on directory traversal.
///
/// The target may be a directory the user specified via environment variables (HF_HOME / OLLAMA_MODELS),
/// so when it points to a huge directory a single scan must not expand forever; both depth and entry count are limited.
const MAX_SIZE_WALK_DEPTH: usize = 12;
const MAX_SIZE_WALK_ENTRIES: usize = 200_000;

/// A directory's own real usage: count only real files, don't follow reparse points.
///
/// Used for stats like "cache root": symlinks there are accounted for by their target directories, so the total equals real disk usage.
pub fn directory_size_deduped(path: &Path) -> u64 {
    let mut budget = MAX_SIZE_WALK_ENTRIES;
    directory_size_with_budget(path, 0, &mut budget)
}

fn directory_size_with_budget(path: &Path, depth: usize, budget: &mut usize) -> u64 {
    if depth > MAX_SIZE_WALK_DEPTH || *budget == 0 {
        return 0;
    }

    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };

    if metadata.is_file() {
        return metadata.len();
    }

    if !metadata.is_dir() {
        return 0;
    }

    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };

    let mut total = 0u64;
    for entry in entries.filter_map(Result::ok) {
        if *budget == 0 {
            break;
        }
        *budget -= 1;

        let name = entry.file_name();
        if is_noise_directory_name(&name.to_string_lossy()) {
            continue;
        }

        total += directory_size_with_budget(&entry.path(), depth + 1, budget);
    }

    total
}

/// Resolve the real path a reparse point (symlink/junction) points to.
///
/// HuggingFace snapshot files are symlinks to blobs and must be counted by their target file,
/// otherwise on Windows the link's own length is 0 and the model is counted as empty.
pub fn resolve_reparse_target(path: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(path).ok()?;
    if target.is_absolute() {
        return Some(target);
    }

    // A relative link is based on the directory containing the link
    path.parent().map(|parent| parent.join(&target))
}

/// Resolve an entry's real size in a model directory: real files use their own size, symlinks use the target file's size.
pub fn linked_file_size(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(path).ok()?;

    if metadata.is_file() {
        return Some(metadata.len());
    }

    if !metadata.file_type().is_symlink() {
        return None;
    }

    let target = resolve_reparse_target(path)?;
    std::fs::symlink_metadata(&target)
        .ok()
        .filter(|target_metadata| target_metadata.is_file())
        .map(|target_metadata| target_metadata.len())
}

/// Directory names to skip during counting.
fn is_noise_directory_name(file_name: &str) -> bool {
    matches!(
        file_name,
        ".git" | "node_modules" | "target" | "$RECYCLE.BIN" | "System Volume Information"
    )
}

pub fn is_model_extension(path: &Path) -> bool {
    is_supported_model_extension(path)
}

pub fn collect_model_files(root: &Path) -> Vec<ModelItem> {
    collect_model_files_with_min_size(root, 0)
}

/// Collect model files with a minimum-size threshold.
///
/// In known platform directories (ComfyUI's various folders, etc.) file semantics are clear, so a 0 threshold is fine;
/// but when a user-defined root (OLLAMA_MODELS / HF_HOME) is mounted directly, a threshold is required,
/// otherwise small files like config.json under it would be treated as model assets.
pub fn collect_model_files_with_min_size(root: &Path, min_size: u64) -> Vec<ModelItem> {
    let mut models: Vec<ModelItem> = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(skip_hidden_system_noise)
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if entry.file_type().is_dir() && is_model_package_directory(path) {
                // Core ML's .mlpackage is a directory bundle, it won't be hit by file-extension logic, so count it as a whole asset.
                let size = directory_size(path);
                if size > 0 {
                    return Some(ModelItem {
                        name: path
                            .file_name()
                            .and_then(|value| value.to_str())
                            .unwrap_or("未命名模型包")
                            .to_string(),
                        size,
                        path: path.to_path_buf(),
                    });
                }
            }

            if !entry.file_type().is_file() {
                return None;
            }

            if !is_model_extension(path) {
                return None;
            }

            let size = file_size(path)?;
            if size < min_size {
                return None;
            }

            Some(ModelItem {
                name: path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("未命名模型")
                    .to_string(),
                size,
                path: path.to_path_buf(),
            })
        })
        .collect();

    models.sort_by(|left, right| right.size.cmp(&left.size));
    models
}

pub fn source_from_models(
    name: &str,
    path: PathBuf,
    mut models: Vec<ModelItem>,
) -> Option<AssetSource> {
    if models.is_empty() {
        return None;
    }

    models.sort_by(|left, right| right.size.cmp(&left.size));
    let total_size = models.iter().map(|model| model.size).sum();

    Some(AssetSource {
        name: name.to_string(),
        path,
        total_size,
        model_count: models.len(),
        models,
    })
}

/// Collect model files with a source prefix: same-named files in different model dirs are distinguished as "source / filename" for easier location in the list.
pub fn collect_prefixed_model_files(root: &Path, prefix: &str) -> Vec<ModelItem> {
    let mut models = collect_model_files(root);
    for model in &mut models {
        model.name = format!("{} / {}", prefix, model.name);
    }
    models
}

fn skip_hidden_system_noise(entry: &DirEntry) -> bool {
    let file_name = entry.file_name().to_string_lossy();

    // These directories don't carry user-manageable model assets; skipping them reduces recursion cost and false positives.
    !matches!(
        file_name.as_ref(),
        ".git" | "node_modules" | "target" | "$RECYCLE.BIN" | "System Volume Information"
    )
}
