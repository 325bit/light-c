use super::{
    directory_size_deduped, linked_file_size, normalize_existing_path, source_from_models,
    user_home_dir, DetectorOutput, ModelDetector,
};
use crate::ai_models::types::ModelItem;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct HuggingFaceDetector;

impl HuggingFaceDetector {
    pub fn new() -> Self {
        Self
    }
}

/// One HuggingFace cache: `hub_dir` directly contains `models--*`, `size_root` is the root used for total-usage stats.
///
/// The two fields aren't always the same: `HF_HOME` points to the cache root (with `hub` beneath it),
/// while `HF_HUB_CACHE` points directly at `hub` itself.
struct HubCache {
    hub_dir: PathBuf,
    size_root: PathBuf,
}

impl ModelDetector for HuggingFaceDetector {
    fn detect(&self) -> DetectorOutput {
        let caches = unique_hub_caches(huggingface_cache_candidates());

        let mut models = Vec::new();
        let mut size_roots: Vec<PathBuf> = Vec::new();
        let mut seen_size_roots = HashSet::new();
        let mut source_path = None;

        for cache in &caches {
            // The source path shown to users is the cache root, matching the environment variable they set themselves
            source_path.get_or_insert_with(|| cache.size_root.clone());
            models.append(&mut collect_huggingface_models(&cache.hub_dir));

            let key = cache.size_root.to_string_lossy().to_ascii_lowercase();
            if seen_size_roots.insert(key) {
                size_roots.push(cache.size_root.clone());
            }
        }

        // Each model's size is summed from "the data it references"; when a blob is shared, the total will exceed disk usage,
        // so overall usage is counted separately from the cache directory's real files (symlinks excluded, blobs account for them).
        let total_size: u64 = size_roots
            .iter()
            .map(|root| directory_size_deduped(root))
            .sum();

        DetectorOutput {
            source: source_from_models("HuggingFace", source_path.unwrap_or_default(), models).map(
                |source| crate::ai_models::types::AssetSource {
                    total_size: total_size.max(source.total_size),
                    ..source
                },
            ),
            warnings: Vec::new(),
        }
    }
}

/// Collect all possible cache locations (existence unverified, not deduped).
fn huggingface_cache_candidates() -> Vec<HubCache> {
    let mut candidates = Vec::new();

    // These two variables point directly at the hub directory, which already contains models--*; don't append another hub layer
    for key in ["HF_HUB_CACHE", "HUGGINGFACE_HUB_CACHE"] {
        if let Ok(value) = std::env::var(key) {
            let hub_dir = PathBuf::from(value);
            candidates.push(HubCache {
                size_root: hub_dir.clone(),
                hub_dir,
            });
        }
    }

    // HF_HOME points to the cache root, hub is its subdirectory
    if let Ok(hf_home) = std::env::var("HF_HOME") {
        let root = PathBuf::from(hf_home);
        candidates.push(HubCache {
            hub_dir: root.join("hub"),
            size_root: root,
        });
    }

    if let Some(home_dir) = user_home_dir() {
        let root = home_dir.join(".cache").join("huggingface");
        candidates.push(HubCache {
            hub_dir: root.join("hub"),
            size_root: root,
        });
    }

    candidates
}

/// Keep only caches that really exist, with hub directories deduped.
fn unique_hub_caches(candidates: Vec<HubCache>) -> Vec<HubCache> {
    let mut seen_hubs = HashSet::new();
    let mut caches = Vec::new();

    for HubCache { hub_dir, size_root } in candidates {
        // Multiple environment variables may point to the same place (e.g. HF_HUB_CACHE set to HF_HOME\hub)
        let Some(hub_dir) = normalize_existing_path(hub_dir) else {
            continue;
        };
        if !seen_hubs.insert(hub_dir.to_string_lossy().to_ascii_lowercase()) {
            continue;
        }
        caches.push(HubCache { hub_dir, size_root });
    }

    caches
}

/// Collect models from the HuggingFace cache.
///
/// A single model's size equals "real files in the directory" plus "files targeted by symlinks inside that directory":
/// snapshot files are all links to blobs; on Windows a link's own length is 0, so it must be resolved to its target,
/// otherwise the model is counted as 0 bytes and dropped from the list.
fn collect_huggingface_models(hub_dir: &Path) -> Vec<ModelItem> {
    let mut models = Vec::new();

    if !hub_dir.is_dir() {
        return models;
    }

    let Ok(entries) = hub_dir.read_dir() else {
        return models;
    };

    for entry in entries.filter_map(Result::ok) {
        let model_dir = entry.path();
        if !model_dir.is_dir() {
            continue;
        }

        let Some(dir_name) = model_dir.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !dir_name.starts_with("models--") {
            continue;
        }

        // An incomplete download has a directory but no weights at all; such an empty shell shouldn't appear in the model list
        let size = model_cache_size(&model_dir);
        if size == 0 {
            continue;
        }

        models.push(ModelItem {
            name: huggingface_model_name(&model_dir),
            size,
            path: model_dir,
        });
    }

    models.sort_by(|left, right| right.size.cmp(&left.size));
    models
}

/// Count the size of a single model cache.
///
/// Weights in snapshots are symlinks to blobs; count "the target the link points to" here,
/// but do **not** recurse into the blobs directory itself: otherwise the same data is counted twice
/// (once as the real file in blobs, once as the snapshot link).
fn model_cache_size(model_dir: &Path) -> u64 {
    let mut total = 0u64;
    let snapshots_dir = model_dir.join("snapshots");

    for version_dir in std::fs::read_dir(&snapshots_dir)
        .map(|entries| entries.filter_map(Result::ok).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        // Only take the latest snapshot: one model may have several version dirs, summing per version would double-count shared weights
        let Ok(files) = std::fs::read_dir(version_dir.path()) else {
            continue;
        };
        total = total.max(
            files
                .filter_map(Result::ok)
                .filter_map(|file| linked_file_size(&file.path()))
                .sum(),
        );
    }

    if total > 0 {
        return total;
    }

    // Caches without a standard snapshot structure (old versions or manually placed): fall back to counting real files in the directory
    directory_size_deduped(model_dir)
}

fn huggingface_model_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|name| {
            name.strip_prefix("models--")
                .unwrap_or(name)
                .replace("--", "/")
        })
        .unwrap_or_else(|| "HuggingFace 模型缓存".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    fn fixture(name: &str) -> PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lightc-hf-{}-{}-{}",
            name,
            std::process::id(),
            sequence
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建测试目录失败");
        dir
    }

    /// Recreate HuggingFace's cache structure: real files in blobs, snapshots all symlinks to them.
    /// Before the fix, summing directory by directory counted models as 0 bytes and dropped them from the list.
    #[cfg(target_os = "windows")]
    #[test]
    fn counts_symlinked_model_weights_from_blobs() {
        let root = fixture("symlink");
        let model_dir = root
            .join("hub")
            .join("models--Systran--faster-whisper-base");
        let blobs_dir = model_dir.join("blobs");
        let snapshot_dir = model_dir
            .join("snapshots")
            .join("ebe41f70d5b6dfa9166e2c581c45c9c0cfc57b66");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::create_dir_all(&snapshot_dir).unwrap();

        // 1MB weights, 1KB config
        std::fs::write(blobs_dir.join("weight-blob"), vec![0u8; 1024 * 1024]).unwrap();
        std::fs::write(blobs_dir.join("config-blob"), vec![0u8; 1024]).unwrap();

        // Skip when symlinks can't be created (developer mode off), to avoid false failures in test environments
        if std::os::windows::fs::symlink_file(
            blobs_dir.join("weight-blob"),
            snapshot_dir.join("model.bin"),
        )
        .is_err()
        {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        std::os::windows::fs::symlink_file(
            blobs_dir.join("config-blob"),
            snapshot_dir.join("config.json"),
        )
        .unwrap();

        let models = collect_huggingface_models(&root.join("hub"));

        assert_eq!(models.len(), 1, "符号链接指向的模型不应被丢弃");
        assert_eq!(models[0].name, "Systran/faster-whisper-base");
        assert_eq!(
            models[0].size,
            1024 * 1024 + 1024,
            "应按真实权重统计，而不是符号链接自身的 0 字节"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn counts_plain_files_when_blobs_are_not_used() {
        // Old or manually placed caches have no blobs, real files are placed directly
        let root = fixture("plain");
        let model_dir = root.join("hub").join("models--org--model");
        std::fs::create_dir_all(model_dir.join("snapshots").join("v1")).unwrap();
        std::fs::write(
            model_dir
                .join("snapshots")
                .join("v1")
                .join("pytorch_model.bin"),
            vec![0u8; 4096],
        )
        .unwrap();

        let models = collect_huggingface_models(&root.join("hub"));

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].size, 4096);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_empty_model_shells() {
        let root = fixture("empty");
        std::fs::create_dir_all(root.join("hub").join("models--org--empty")).unwrap();

        let models = collect_huggingface_models(&root.join("hub"));

        assert!(models.is_empty(), "没有任何权重的空壳不应出现在列表里");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// HF_HUB_CACHE points straight at the hub directory; the same cache hit by multiple environment variables counts only once.
    #[test]
    fn dedupes_hub_dirs_and_skips_missing_ones() {
        let root = fixture("hub-cache");
        let hub_dir = root.join("hub");
        std::fs::create_dir_all(hub_dir.join("models--org--model")).unwrap();

        let caches = unique_hub_caches(vec![
            HubCache {
                hub_dir: hub_dir.clone(),
                size_root: root.clone(),
            },
            // Common form of HF_HUB_CACHE: points directly at hub
            HubCache {
                hub_dir: hub_dir.clone(),
                size_root: hub_dir.clone(),
            },
            // Non-existent paths must be dropped, otherwise missing directories get counted in total usage
            HubCache {
                hub_dir: root.join("missing-hub"),
                size_root: root.clone(),
            },
        ]);

        assert_eq!(caches.len(), 1, "同一个 hub 目录只应保留一处");
        // Windows canonicalize adds a `\\?\` prefix, so compare canonicalized values rather than raw strings
        assert_eq!(caches[0].hub_dir, hub_dir.canonicalize().unwrap());
        assert_eq!(caches[0].size_root, root, "先命中的候选决定统计口径");

        let _ = std::fs::remove_dir_all(&root);
    }
}
