use super::{file_size, unique_existing_paths, user_home_dir, DetectorOutput, ModelDetector};
use crate::ai_models::types::{AssetSource, ModelItem};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Blobs in the blobs directory not referenced by any manifest must be at least this large to count as a model.
/// Ollama's blobs are all sharded weights, far larger than this value; the threshold only filters leftover small files.
const ORPHAN_BLOB_MIN_SIZE: u64 = 1024 * 1024;

pub struct OllamaDetector;

impl OllamaDetector {
    pub fn new() -> Self {
        Self
    }
}

impl ModelDetector for OllamaDetector {
    fn detect(&self) -> DetectorOutput {
        let mut warnings = Vec::new();
        let mut candidate_roots = Vec::new();

        if let Ok(models_path) = std::env::var("OLLAMA_MODELS") {
            candidate_roots.push(PathBuf::from(models_path));
        }

        if let Some(home_dir) = user_home_dir() {
            candidate_roots.push(home_dir.join(".ollama").join("models"));
        }

        let roots = unique_existing_paths(candidate_roots);
        let mut merged_models = Vec::new();
        let mut referenced_blobs = HashSet::new();
        let mut source_path = None;

        for root in roots {
            let manifests_dir = root.join("manifests");
            let blobs_dir = root.join("blobs");
            if !manifests_dir.is_dir() || !blobs_dir.is_dir() {
                continue;
            }

            source_path.get_or_insert_with(|| root.clone());
            match read_ollama_models(&root, &mut referenced_blobs) {
                Ok(models) => merged_models.extend(models),
                Err(error) => warnings.push(error),
            }

            // Some users place models by copying directories or "importing", with no manifest at all.
            // Counting only manifests would report 0 overall, so scan again by file characteristics, using dedup to avoid duplicating results from above.
            merged_models.extend(collect_blobs_without_manifest(
                &blobs_dir,
                &mut referenced_blobs,
            ));
        }

        // When there are no models, do not return an empty source: the directory exists but has no weights (Ollama installed but no model pulled),
        // and reporting "Ollama discovered" would make users think something was found.
        if merged_models.is_empty() {
            return DetectorOutput {
                source: None,
                warnings,
            };
        }

        merged_models.sort_by(|left, right| right.size.cmp(&left.size));
        // Total is counted by "actually referenced blob files": multiple models share the same weight,
        // summing per model would double-count the shared part, making the total larger than actual disk usage.
        let total_size = referenced_blobs
            .iter()
            .filter_map(|path| file_size(path))
            .sum();

        DetectorOutput {
            source: Some(AssetSource {
                name: "Ollama".to_string(),
                path: source_path.unwrap_or_default(),
                total_size,
                model_count: merged_models.len(),
                models: merged_models,
            }),
            warnings,
        }
    }
}

/// Collect weight files under blobs not referenced by any manifest.
///
/// Ollama blob file names are `sha256-<digest>`, with no readable name; use the first 12 digits of the digest as display name,
/// so users can at least recognize in the list that this is a weight file not recorded by any manifest.
fn collect_blobs_without_manifest(
    blobs_dir: &Path,
    referenced_blobs: &mut HashSet<PathBuf>,
) -> Vec<ModelItem> {
    let mut models = Vec::new();

    for entry in WalkDir::new(blobs_dir)
        .follow_links(false)
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path().to_path_buf();
        // Weights already recorded by a manifest are not counted again
        if referenced_blobs.contains(&path) {
            continue;
        }

        let Some(size) = file_size(&path) else {
            continue;
        };
        // Files that are too small are leftover fragments, not models; note that at this point they must not be added to referenced_blobs,
        // otherwise they would be mislabeled as "counted" and skip later checks.
        if size < ORPHAN_BLOB_MIN_SIZE {
            continue;
        }

        referenced_blobs.insert(path.clone());

        let digest = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("ollama-blob")
            .trim_start_matches("sha256-");
        let short_digest: String = digest.chars().take(12).collect();

        models.push(ModelItem {
            name: format!("未记录权重的 blob · {}", short_digest),
            size,
            path,
        });
    }

    models.sort_by(|left, right| right.size.cmp(&left.size));
    models
}

fn read_ollama_models(
    root: &Path,
    referenced_blobs: &mut HashSet<PathBuf>,
) -> Result<Vec<ModelItem>, String> {
    let manifests_dir = root.join("manifests");
    let blobs_dir = root.join("blobs");
    let mut models = Vec::new();

    for entry in WalkDir::new(&manifests_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let manifest_path = entry.path();
        let manifest_text = fs::read_to_string(manifest_path).map_err(|error| {
            format!(
                "读取 Ollama manifest 失败：{}，{}",
                manifest_path.display(),
                error
            )
        })?;
        let manifest: Value = serde_json::from_str(&manifest_text).map_err(|error| {
            format!(
                "解析 Ollama manifest 失败：{}，{}",
                manifest_path.display(),
                error
            )
        })?;

        let mut model_blobs = HashSet::new();
        if let Some(layers) = manifest.get("layers").and_then(|value| value.as_array()) {
            for layer in layers {
                let Some(digest) = layer.get("digest").and_then(|value| value.as_str()) else {
                    continue;
                };
                if let Some(blob_path) = digest_to_blob_path(&blobs_dir, digest) {
                    model_blobs.insert(blob_path.clone());
                    referenced_blobs.insert(blob_path);
                }
            }
        }

        if let Some(config_digest) = manifest
            .get("config")
            .and_then(|value| value.get("digest"))
            .and_then(|value| value.as_str())
        {
            if let Some(blob_path) = digest_to_blob_path(&blobs_dir, config_digest) {
                model_blobs.insert(blob_path.clone());
                referenced_blobs.insert(blob_path);
            }
        }

        let size = model_blobs.iter().filter_map(|path| file_size(path)).sum();
        if size == 0 {
            continue;
        }

        models.push(ModelItem {
            name: manifest_model_name(&manifests_dir, manifest_path),
            size,
            // Ollama's real weights are shared blobs; showing the root directory better matches users' mental model than sha256 files
            path: root.to_path_buf(),
        });
    }

    Ok(models)
}

fn digest_to_blob_path(blobs_dir: &Path, digest: &str) -> Option<PathBuf> {
    let normalized_digest = digest.strip_prefix("sha256:")?;
    Some(blobs_dir.join(format!("sha256-{}", normalized_digest)))
}

fn manifest_model_name(manifests_dir: &Path, manifest_path: &Path) -> String {
    let relative_parts: Vec<String> = manifest_path
        .strip_prefix(manifests_dir)
        .unwrap_or(manifest_path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect();

    if relative_parts.len() >= 4 {
        let namespace = &relative_parts[1];
        let model_name = &relative_parts[2];
        let tag = &relative_parts[3];
        if namespace == "library" {
            return format!("{}:{}", model_name, tag);
        }
        return format!("{}/{}:{}", namespace, model_name, tag);
    }

    manifest_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("Ollama 模型")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    fn fixture(name: &str) -> PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lightc-ollama-{}-{}-{}",
            name,
            std::process::id(),
            sequence
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建测试目录失败");
        dir
    }

    /// Write an Ollama manifest, with layer digests and config-file digest pointing to sha256 files under blobs.
    fn write_manifest(
        root: &Path,
        model: &str,
        tag: &str,
        layer_digests: &[&str],
        config_digest: &str,
    ) {
        let manifest_dir = root
            .join("manifests")
            .join("registry.ollama.ai")
            .join("library")
            .join(model);
        std::fs::create_dir_all(&manifest_dir).unwrap();

        let layers: Vec<String> = layer_digests
            .iter()
            .map(|digest| format!(r#"{{"digest":"sha256:{}"}}"#, digest))
            .collect();
        let manifest = format!(
            r#"{{"layers":[{}],"config":{{"digest":"sha256:{}"}}}}"#,
            layers.join(","),
            config_digest
        );
        std::fs::write(manifest_dir.join(tag), manifest).unwrap();
    }

    #[test]
    fn total_size_counts_shared_blobs_once() {
        let root = fixture("shared-blobs");
        let blobs_dir = root.join("blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();

        // Two models share the same weight layer d0, each with its own small extra layer
        std::fs::write(blobs_dir.join("sha256-d0"), vec![0u8; 1024]).unwrap();
        std::fs::write(blobs_dir.join("sha256-a1"), vec![0u8; 256]).unwrap();
        std::fs::write(blobs_dir.join("sha256-a2"), vec![0u8; 128]).unwrap();
        write_manifest(&root, "shared-a", "latest", &["d0", "a1"], "a1");
        write_manifest(&root, "shared-b", "latest", &["d0", "a2"], "a2");

        let mut referenced_blobs = HashSet::new();
        let models = read_ollama_models(&root, &mut referenced_blobs).unwrap();
        let per_model_total: u64 = models.iter().map(|model| model.size).sum();
        let distinct_total: u64 = referenced_blobs
            .iter()
            .filter_map(|path| file_size(path))
            .sum();

        assert_eq!(models.len(), 2);
        // Summing per model would count the shared d0 twice, while actual disk usage has only one copy
        assert_eq!(per_model_total, 1024 + 256 + 1024 + 128);
        assert_eq!(distinct_total, 1024 + 256 + 128);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collects_blobs_without_manifest_once() {
        let root = fixture("orphan-blob");
        let blobs_dir = root.join("blobs");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::write(blobs_dir.join("sha256-d0"), vec![0u8; 2048]).unwrap();
        std::fs::write(blobs_dir.join("sha256-small"), vec![0u8; 16]).unwrap();
        write_manifest(&root, "imported", "latest", &["d0"], "d0");

        let mut referenced_blobs = HashSet::new();
        read_ollama_models(&root, &mut referenced_blobs).unwrap();

        // d0 is already referenced by a manifest and should not appear again as an "unrecorded weight blob"
        let orphans = collect_blobs_without_manifest(&blobs_dir, &mut referenced_blobs);
        assert!(orphans.is_empty());

        // Unreferenced weights are added back; fragment files clearly below the threshold (1MB) are still filtered
        std::fs::write(blobs_dir.join("sha256-orphan"), vec![0u8; 2 * 1024 * 1024]).unwrap();
        let orphans = collect_blobs_without_manifest(&blobs_dir, &mut referenced_blobs);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].size, 2 * 1024 * 1024);
        assert!(orphans[0].name.starts_with("未记录权重的 blob"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
