#![cfg(target_os = "windows")]

use crate::ai_models::model_file_rules::mft_model_min_size;
use crate::ai_models::types::{AiModelPhaseDuration, AiModelScanProgress, AssetSource, ModelItem};
use crate::scanner::big_files_engine::mft_core;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct CoveredRoot {
    pub source_name: String,
    pub path: PathBuf,
}

pub fn discover_models_via_mft<F>(
    covered_roots: &[CoveredRoot],
    scan_started_at: &Instant,
    progress: &F,
) -> (Vec<AssetSource>, Vec<String>, Vec<AiModelPhaseDuration>)
where
    F: Fn(AiModelScanProgress) + Sync,
{
    let drives: Vec<char> = local_drive_letters()
        .into_iter()
        .filter(|drive_letter| mft_core::is_ntfs(*drive_letter))
        .collect();

    // The $MFT of each drive is independent; serially the total time is the sum of all drives, in parallel it equals the slowest drive.
    // Each drive's stage timings go into its own Vec and are merged by spawn order at the end, keeping the UI order stable.
    let per_drive: Vec<(char, Result<Vec<(String, ModelItem)>, String>, Vec<AiModelPhaseDuration>)> =
        std::thread::scope(|scope| {
            let handles: Vec<_> = drives
                .iter()
                .map(|&drive_letter| {
                    scope.spawn(move || {
                        let mut phases = Vec::new();
                        let outcome = scan_drive_for_model_candidates(
                            drive_letter,
                            covered_roots,
                            scan_started_at,
                            progress,
                            &mut phases,
                        );
                        (drive_letter, outcome, phases)
                    })
                })
                .collect();

            handles
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .collect()
        });

    let mut grouped_models: HashMap<String, Vec<ModelItem>> = HashMap::new();
    let mut warnings = Vec::new();
    let mut phase_durations = Vec::new();

    for (drive_letter, outcome, phases) in per_drive {
        phase_durations.extend(phases);
        match outcome {
            Ok(models) => {
                for (source_name, model) in models {
                    grouped_models.entry(source_name).or_default().push(model);
                }
            }
            Err(error) => warnings.push(format!("{}: {}", drive_letter, error)),
        }
    }

    let mut sources = grouped_models
        .into_iter()
        .filter_map(|(source_name, mut models)| {
            if models.is_empty() {
                return None;
            }

            models.sort_by(|left, right| right.size.cmp(&left.size));
            let total_size = models.iter().map(|model| model.size).sum();
            let source_path = common_parent_path(&models).unwrap_or_default();

            Some(AssetSource {
                name: source_name,
                path: source_path,
                total_size,
                model_count: models.len(),
                models,
            })
        })
        .collect::<Vec<_>>();

    sources.sort_by(|left, right| right.total_size.cmp(&left.total_size));
    (sources, warnings, phase_durations)
}

fn scan_drive_for_model_candidates<F>(
    drive_letter: char,
    covered_roots: &[CoveredRoot],
    scan_started_at: &Instant,
    progress: &F,
    phase_durations: &mut Vec<AiModelPhaseDuration>,
) -> Result<Vec<(String, ModelItem)>, String>
where
    F: Fn(AiModelScanProgress) + Sync,
{
    let mut phase_started_at = Instant::now();
    emit_mft_progress(
        progress,
        scan_started_at,
        &phase_started_at,
        drive_letter,
        "mftEnumerate",
        &format!("正在读取 {} 盘 NTFS 主文件表", drive_letter),
    );
    // One sequential pass: get the name (for filtering extensions), parent-child relationships, and size together.
    // Originally it first enumerated names via USN, then read back $MFT by id — the same data passed through twice, and USN enumeration was the slow pass.
    let metadata_reader = mft_core::NtfsFileMetadataReader::open(drive_letter)?;
    let (entries, mft_stats) = metadata_reader.read_all_records(&|_| true)?;
    log::info!(
        "[AiModels-MFT] {} 盘读取完成：{} 条记录，分段 {}，读盘 {}ms，解析 {}ms",
        drive_letter,
        entries.len(),
        mft_stats.fragments,
        mft_stats.io_ms,
        mft_stats.parse_ms
    );
    finish_mft_phase(
        phase_durations,
        "mftEnumerate",
        &format!(
            "{} 盘 MFT 读取（分段 {} · 读盘 {:.1}s、解析 {:.1}s）",
            drive_letter,
            mft_stats.fragments,
            mft_stats.io_ms as f64 / 1000.0,
            mft_stats.parse_ms as f64 / 1000.0
        ),
        phase_started_at,
    );

    phase_started_at = Instant::now();
    emit_mft_progress(
        progress,
        scan_started_at,
        &phase_started_at,
        drive_letter,
        "mftFilter",
        &format!("正在筛选 {} 盘大模型候选", drive_letter),
    );
    let candidate_min_sizes = entries
        .iter()
        .filter(|entry| !entry.is_dir)
        .filter_map(|entry| {
            // The MFT fallback only cares about a few large-model extensions; filter at the file-name level first, avoiding reading the size of every file on the drive.
            model_extension_threshold(&entry.name).map(|min_size| (entry.mft_id, min_size))
        })
        .collect::<HashMap<_, _>>();
    finish_mft_phase(
        phase_durations,
        "mftFilter",
        &format!("{} 盘候选筛选", drive_letter),
        phase_started_at,
    );

    if candidate_min_sizes.is_empty() {
        return Ok(Vec::new());
    }

    phase_started_at = Instant::now();
    emit_mft_progress(
        progress,
        scan_started_at,
        &phase_started_at,
        drive_letter,
        "mftPaths",
        &format!("正在重建 {} 盘模型路径", drive_letter),
    );
    let candidate_ids = candidate_min_sizes.keys().copied().collect::<HashSet<_>>();
    let paths = mft_core::rebuild_paths_for_records(&entries, drive_letter, &candidate_ids);
    finish_mft_phase(
        phase_durations,
        "mftPaths",
        &format!("{} 盘路径重建", drive_letter),
        phase_started_at,
    );

    if paths.is_empty() {
        return Ok(Vec::new());
    }

    phase_started_at = Instant::now();
    emit_mft_progress(
        progress,
        scan_started_at,
        &phase_started_at,
        drive_letter,
        "mftMetadata",
        &format!("正在校验 {} 盘候选文件大小", drive_letter),
    );
    // Once paths are obtained the records are useless: full-drive records carry names and take hundreds of MB,
    // and with multiple drives in parallel several copies pile up at once — release as early as possible.
    drop(entries);

    // First shrink candidates in memory to "path + threshold", then stat in parallel:
    // candidates are usually only tens or hundreds but scattered across the whole drive; serial stat was measured at over a second.
    //
    // Normalize covered paths only once: originally every candidate canonicalized every coverage root again
    // (canonicalize opens a file handle on Windows), which with thousands of candidates is hundreds of thousands of redundant system calls.
    let covered_keys = covered_root_keys(covered_roots);

    let resolved: Vec<(PathBuf, u64, String)> = candidate_min_sizes
        .into_iter()
        .filter_map(|(mft_id, min_size)| {
            let path = PathBuf::from(paths.get(&mft_id)?);
            if should_skip_path(&path) || !is_plausible_mft_model_candidate(&path) {
                return None;
            }

            let path_key = normalize_path_key(&path);
            if covered_source_for_key(&path_key, &covered_keys).is_some() {
                return None;
            }

            Some((path, min_size, path_key))
        })
        .collect();

    let models: Vec<(String, ModelItem)> = resolved
        .par_iter()
        .filter_map(|(path, min_size, path_key)| {
            // MFT handles fast candidate location, size reading goes back to standard metadata,
            // so a few files are not wrongly dropped before the path stage due to NTFS record parsing differences.
            let metadata = path.metadata().ok()?;
            if !metadata.is_file() || metadata.len() < *min_size {
                return None;
            }

            let source_name = infer_source_name(path, path_key, &covered_keys);
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("未知模型")
                .to_string();

            Some((
                source_name,
                ModelItem {
                    name,
                    size: metadata.len(),
                    path: path.clone(),
                },
            ))
        })
        .collect();
    finish_mft_phase(
        phase_durations,
        "mftMetadata",
        &format!("{} 盘候选校验", drive_letter),
        phase_started_at,
    );

    Ok(models)
}

fn emit_mft_progress<F>(
    progress: &F,
    scan_started_at: &Instant,
    phase_started_at: &Instant,
    drive_letter: char,
    stage: &str,
    message: &str,
) where
    F: Fn(AiModelScanProgress) + Sync,
{
    progress(AiModelScanProgress {
        stage: stage.to_string(),
        message: message.to_string(),
        elapsed_ms: scan_started_at.elapsed().as_millis(),
        stage_elapsed_ms: phase_started_at.elapsed().as_millis(),
        // The drive letter in the stage text is interpolated by the frontend, so pass the drive letter out as well
        drive_label: Some(drive_letter.to_string()),
    });
}

fn finish_mft_phase(
    phase_durations: &mut Vec<AiModelPhaseDuration>,
    stage: &str,
    label: &str,
    phase_started_at: Instant,
) {
    phase_durations.push(AiModelPhaseDuration {
        stage: stage.to_string(),
        label: label.to_string(),
        duration_ms: phase_started_at.elapsed().as_millis(),
    });
}

/// Normalized key of a coverage path: source name + lowercase path.
///
/// Precompute once for all candidates of this drive to reuse; re-canonicalizing per candidate is pure waste.
struct CoveredRootKey<'a> {
    source_name: &'a str,
    key: String,
}

fn covered_root_keys(covered_roots: &[CoveredRoot]) -> Vec<CoveredRootKey<'_>> {
    covered_roots
        .iter()
        .map(|root| CoveredRootKey {
            source_name: root.source_name.as_str(),
            key: normalize_path_key(&root.path),
        })
        .collect()
}

/// Which covered source a candidate falls under; `None` means the config layer never scanned this path, so MFT should add it.
fn covered_source_for_key<'a>(
    path_key: &str,
    covered_keys: &'a [CoveredRootKey<'_>],
) -> Option<&'a str> {
    covered_keys
        .iter()
        .find(|root| path_key_starts_with(path_key, &root.key))
        .map(|root| root.source_name)
}

fn path_key_starts_with(path_key: &str, root_key: &str) -> bool {
    // Hot path: every candidate is compared against every coverage path, so do not allocate strings for this
    let root_key = root_key.trim_end_matches('\\');
    if path_key == root_key {
        return true;
    }
    if !path_key.starts_with(root_key) {
        return false;
    }

    // Must stop at a separator, otherwise `d:\models2` would be treated as a descendant of `d:\models`
    path_key[root_key.len()..].starts_with('\\')
}

fn model_extension_threshold(file_name: &str) -> Option<u64> {
    // MFT filters by file name first; the unified rules already set different size thresholds for extension misjudgment risks.
    mft_model_min_size(file_name)
}

fn infer_source_name(
    path: &Path,
    path_key: &str,
    covered_keys: &[CoveredRootKey<'_>],
) -> String {
    if let Some(source_name) = covered_source_for_key(path_key, covered_keys) {
        return source_name.to_string();
    }

    let lower_path = path.to_string_lossy().to_ascii_lowercase();
    if lower_path.contains("\\diffusion_models\\")
        || lower_path.contains("\\checkpoints\\")
        || lower_path.contains("\\loras\\")
        || lower_path.contains("\\text_encoders\\")
        || lower_path.contains("\\controlnet\\")
        || lower_path.contains("\\vae\\")
    {
        return "ComfyUI".to_string();
    }

    if lower_path.contains("\\hub\\models--") || lower_path.contains("\\huggingface\\") {
        return "HuggingFace".to_string();
    }

    if lower_path.contains("\\.lmstudio\\") {
        return "LM Studio".to_string();
    }

    if lower_path.contains("\\.ollama\\models\\") {
        return "Ollama".to_string();
    }

    if lower_path.contains("\\llama.cpp\\") || lower_path.contains("\\llama\\models\\") {
        return "llama.cpp".to_string();
    }

    "未归类".to_string()
}

fn normalize_path_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn should_skip_path(path: &Path) -> bool {
    let lower_path = path.to_string_lossy().to_ascii_lowercase();
    lower_path.contains("\\windows\\")
        || lower_path.contains("\\program files\\")
        || lower_path.contains("\\program files (x86)\\")
        || lower_path.contains("\\node_modules\\")
        || lower_path.contains("\\target\\")
        || lower_path.contains("\\.git\\")
        || lower_path.contains("\\$recycle.bin\\")
        || lower_path.contains("\\system volume information\\")
        || is_game_or_benchmark_path(&lower_path)
        // Chromium-based browsers put local AI/inference caches in the user data directory; these are not model assets the user actively manages, so skipping them fits the product semantics better.
        || lower_path.contains("\\user data\\optguideondevicemodel\\")
        || lower_path.contains("\\user data\\provenancedata\\")
}

fn is_game_or_benchmark_path(lower_path: &str) -> bool {
    // Game assets often use huge .bin/.dlc chunks; excluding these directories outright in deep discovery avoids misreporting Steam games and 3DMark DLC as model assets.
    lower_path.contains("\\steamapps\\")
        || lower_path.contains("\\steam library\\")
        || lower_path.contains("\\steamlibrary\\")
        || lower_path.contains("\\epic games\\")
        || lower_path.contains("\\ea games\\")
        || lower_path.contains("\\ubisoft\\")
        || lower_path.contains("\\gog galaxy\\")
        || lower_path.contains("\\xboxgames\\")
        || lower_path.contains("\\3dmark\\")
        || lower_path.contains("\\3dmark ")
}

fn is_plausible_mft_model_candidate(path: &Path) -> bool {
    // .bin/.dlc have the highest false-positive rate in a full-drive MFT scan, so they must be confirmed with file-name or directory semantics; the platform-config-layer scan is not affected by this fallback rule.
    if !requires_ai_context_for_mft(path) {
        return true;
    }

    has_ai_model_context(path)
}

fn requires_ai_context_for_mft(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };

    matches!(extension.to_ascii_lowercase().as_str(), "bin" | "dlc")
}

fn has_ai_model_context(path: &Path) -> bool {
    let lower_path = path
        .to_string_lossy()
        .replace('/', "\\")
        .to_ascii_lowercase();
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Directory semantics are more reliable than a single extension: only .bin/.dlc that users deliberately placed in model/inference-framework directories enter the uncategorized results.
    let path_hints = [
        "\\models\\",
        "\\model\\",
        "\\checkpoints\\",
        "\\diffusion_models\\",
        "\\loras\\",
        "\\text_encoders\\",
        "\\controlnet\\",
        "\\vae\\",
        "\\unet\\",
        "\\clip\\",
        "\\llm\\",
        "\\llama\\",
        "\\ollama\\",
        "\\huggingface\\",
        "\\transformers\\",
        "\\sentence-transformers\\",
        "\\comfyui\\",
        "\\stable-diffusion\\",
        "\\stable_diffusion\\",
        "\\sd-webui\\",
        "\\kohya\\",
        "\\ai\\",
        "\\ml\\",
    ];
    if path_hints.iter().any(|hint| lower_path.contains(hint)) {
        return true;
    }

    // File names only accept strong model-naming characteristics, to avoid ordinary asset names like model.bin amplifying false positives again.
    [
        "pytorch_model",
        "tf_model",
        "adapter_model",
        "diffusion_pytorch_model",
        "consolidated.",
        "consolidated-",
        "model-",
        "model_",
        "weights",
        "qwen",
        "deepseek",
        "chatglm",
        "baichuan",
        "mistral",
        "mixtral",
        "gemma",
        "llama",
        "stable-diffusion",
        "stable_diffusion",
        "text_encoder",
        "image_encoder",
    ]
    .iter()
    .any(|hint| file_name.contains(hint))
}

fn common_parent_path(models: &[ModelItem]) -> Option<PathBuf> {
    models.first().and_then(|model| {
        if model.path.is_file() {
            model.path.parent().map(Path::to_path_buf)
        } else {
            Some(model.path.clone())
        }
    })
}

fn local_drive_letters() -> Vec<char> {
    ('A'..='Z')
        .filter(|drive_letter| PathBuf::from(format!("{}:\\", drive_letter)).is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_llama_cpp_for_portable_gguf_models() {
        let path = Path::new(r"D:\ai\llama\models\Qwythos-9B-Claude-Mythos.gguf");
        let source_name = infer_source_name(path, &normalize_path_key(path), &[]);

        assert_eq!(source_name, "llama.cpp");
    }

    #[test]
    fn covered_root_match_requires_a_path_boundary() {
        // Looking only at the prefix would wrongly treat `D:\models2` as a descendant of `D:\models`, skipping the whole block
        assert!(path_key_starts_with(r"d:\models\a.gguf", r"d:\models"));
        assert!(!path_key_starts_with(r"d:\models2\a.gguf", r"d:\models"));
        assert!(path_key_starts_with(r"d:\models", r"d:\models"));
    }

    #[test]
    fn covered_source_wins_over_path_hints() {
        let covered_keys = vec![CoveredRootKey {
            source_name: "ComfyUI",
            key: r"d:\comfyui\models".to_string(),
        }];

        // A path containing `\checkpoints\` would normally be pushed to ComfyUI, but ownership must follow the coverage record
        assert_eq!(
            covered_source_for_key(r"d:\comfyui\models\checkpoints\a.safetensors", &covered_keys),
            Some("ComfyUI")
        );
        assert_eq!(
            covered_source_for_key(r"e:\other\checkpoints\a.safetensors", &covered_keys),
            None
        );
    }

    #[test]
    fn skips_browser_on_device_model_cache_paths() {
        assert!(should_skip_path(Path::new(
            r"C:\Users\chunyu\AppData\Local\Google\Chrome\User Data\OptGuideOnDeviceModel\2025.8.11.1\weights.bin"
        )));
        assert!(should_skip_path(Path::new(
            r"C:\Users\chunyu\AppData\Local\Microsoft\Edge\User Data\ProvenanceData\2025.10.7.5\vti-b-p32-visual.quant.ort"
        )));
    }

    #[test]
    fn skips_game_and_benchmark_deep_discovery_noise() {
        assert!(should_skip_path(Path::new(
            r"D:\likesteam\steamapps\common\Monster Hunter World\chunk\chunkG0.bin"
        )));
        assert!(should_skip_path(Path::new(
            r"E:\3dmark\3DMark v2.27\3DMark v2.27\3dmark-v2-27-0-time-spy-test-v1-2-5-3.dlc"
        )));
    }

    #[test]
    fn requires_ai_context_for_noisy_mft_extensions() {
        assert!(!is_plausible_mft_model_candidate(Path::new(
            r"D:\assets\chunks\chunkG0.bin"
        )));
        assert!(!is_plausible_mft_model_candidate(Path::new(
            r"E:\downloads\3dmark-v2-27-0-speed-way-test-v1-0-7-1.dlc"
        )));
        assert!(is_plausible_mft_model_candidate(Path::new(
            r"D:\ai\models\Qwen\pytorch_model-00001-of-00002.bin"
        )));
        assert!(is_plausible_mft_model_candidate(Path::new(
            r"D:\ml\models\vision\model_quantized.dlc"
        )));
    }
}
