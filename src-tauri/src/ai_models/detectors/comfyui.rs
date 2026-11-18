use super::{
    collect_prefixed_model_files, source_from_models, unique_existing_paths, user_home_dir,
    DetectorOutput, ModelDetector,
};
use crate::ai_models::types::ModelItem;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

const COMFYUI_MODEL_DIRS: [&str; 12] = [
    "checkpoints",
    "diffusion_models",
    "unet",
    "loras",
    "embeddings",
    "vae",
    "text_encoders",
    "clip",
    "clip_vision",
    "controlnet",
    "upscale_models",
    "photomaker",
];

pub struct ComfyUiDetector;

impl ComfyUiDetector {
    pub fn new() -> Self {
        Self
    }
}

impl ModelDetector for ComfyUiDetector {
    fn detect(&self) -> DetectorOutput {
        let mut candidate_roots = Vec::new();
        let mut install_roots = Vec::new();

        if let Some(home_dir) = user_home_dir() {
            for install_root in [
                home_dir.join("ComfyUI"),
                home_dir.join("Desktop").join("ComfyUI"),
                home_dir.join("Documents").join("ComfyUI"),
            ] {
                candidate_roots.push(install_root.join("models"));
                install_roots.push(install_root);
            }
        }

        if let Some(app_data_dir) = std::env::var_os("APPDATA").map(PathBuf::from) {
            let desktop_config_dir = app_data_dir.join("ComfyUI");
            candidate_roots.extend(read_comfyui_desktop_model_paths(&desktop_config_dir));
            candidate_roots.extend(read_extra_model_paths_file(
                &desktop_config_dir,
                &desktop_config_dir.join("extra_models_config.yaml"),
            ));
        }

        // Iterate over drives that actually exist, instead of hard-coding C~F: users who install ComfyUI on G: or later would be entirely missed
        for drive in available_drive_roots() {
            // The official Windows portable package extracts to `ComfyUI_windows_portable\ComfyUI\models`,
            // and common all-in-one bundles have inconsistent directory names (Easy-Install, etc.), so shallow-scan one level at the drive root for `ComfyUI*`.
            for install_root in find_portable_comfyui_roots(&drive) {
                candidate_roots.push(install_root.join("models"));
                install_roots.push(install_root);
            }

            let install_root = drive.join("ComfyUI");
            candidate_roots.push(install_root.join("models"));
            install_roots.push(install_root);
        }

        for install_root in unique_existing_paths(install_roots) {
            candidate_roots.extend(read_extra_model_paths(&install_root));
        }

        let mut models = Vec::new();
        // Record the source path as "the root directory where models were actually found", not the first existing candidate:
        // the two may differ (e.g. config points to a D: data directory), and writing the wrong one makes users think another directory took up this space.
        let resolved_roots = unique_existing_paths(candidate_roots);
        let mut best_root_match: Option<(PathBuf, u64)> = None;

        for root in &resolved_roots {
            let mut found = collect_comfyui_models(root);
            let found_size: u64 = found.iter().map(|model| model.size).sum();
            if found_size > 0
                && best_root_match
                    .as_ref()
                    .map(|(_, size)| found_size > *size)
                    .unwrap_or(true)
            {
                best_root_match = Some((root.clone(), found_size));
            }
            models.append(&mut found);
        }

        let source_path = best_root_match
            .map(|(path, _)| path)
            .or_else(|| resolved_roots.first().cloned())
            .unwrap_or_default();

        DetectorOutput {
            source: source_from_models("ComfyUI", source_path, models),
            warnings: Vec::new(),
        }
    }
}

/// Enumerate root directories of drives that actually exist on this machine (e.g. `D:\`).
#[cfg(target_os = "windows")]
fn available_drive_roots() -> Vec<PathBuf> {
    (b'C'..=b'Z')
        .map(|letter| PathBuf::from(format!("{}:\\", letter as char)))
        .filter(|drive| drive.exists())
        .collect()
}

#[cfg(not(target_os = "windows"))]
fn available_drive_roots() -> Vec<PathBuf> {
    vec![PathBuf::from("/")]
}

/// Shallow-scan one level at the drive root to find ComfyUI's installation directory.
///
/// The official Windows portable package extracts to `ComfyUI_windows_portable\ComfyUI\models`,
/// and bundle directory names are all over the place (`ComfyUI-Easy-Install`, `ComfyUI-aki-v1.x`, etc.),
/// but as long as the name starts with `ComfyUI`, recognize it by "directory has models" or "sub-level ComfyUI has models".
/// Only read one level of directories, so the cost is independent of the drive root.
fn find_portable_comfyui_roots(drive_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(drive_root) else {
        return Vec::new();
    };

    let mut roots = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let is_dir = entry
            .file_type()
            .map(|kind| kind.is_dir())
            .unwrap_or(false);
        if !is_dir {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if !name.starts_with("comfyui") {
            continue;
        }

        let install_root = entry.path();
        let nested_root = install_root.join("ComfyUI");
        if nested_root.join("models").is_dir() {
            roots.push(nested_root);
        } else if install_root.join("models").is_dir() {
            roots.push(install_root);
        }
    }

    roots
}

fn collect_comfyui_models(models_root: &Path) -> Vec<ModelItem> {
    let mut models = Vec::new();

    for directory_name in COMFYUI_MODEL_DIRS {
        let model_type_dir = models_root.join(directory_name);
        if !model_type_dir.is_dir() {
            continue;
        }

        // ComfyUI's subdirectories are asset types themselves; putting them in the name reduces the user's comprehension cost.
        models.extend(collect_prefixed_model_files(&model_type_dir, directory_name));
    }

    models
}

fn read_extra_model_paths(install_root: &Path) -> Vec<PathBuf> {
    let yaml_path = install_root.join("extra_model_paths.yaml");
    read_extra_model_paths_file(install_root, &yaml_path)
}

fn read_extra_model_paths_file(base_dir: &Path, yaml_path: &Path) -> Vec<PathBuf> {
    let Ok(content) = fs::read_to_string(&yaml_path) else {
        return Vec::new();
    };

    parse_extra_model_paths(base_dir, &content)
}

fn read_comfyui_desktop_model_paths(config_dir: &Path) -> Vec<PathBuf> {
    let config_path = config_dir.join("config.json");
    let Ok(content) = fs::read_to_string(&config_path) else {
        return Vec::new();
    };

    let Ok(config) = serde_json::from_str::<Value>(&content) else {
        return Vec::new();
    };

    let Some(base_path_value) = find_json_string_key(&config, "basePath") else {
        return Vec::new();
    };

    let base_path = resolve_yaml_path(config_dir, &base_path_value);
    // In the desktop version basePath points to the ComfyUI workspace, and models are still organized under the standard models subdirectory.
    vec![base_path.join("models")]
}

fn parse_extra_model_paths(install_root: &Path, content: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut base_path: Option<PathBuf> = None;

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || !line.contains(':') {
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = cleanup_yaml_value(value);

        if key == "base_path" {
            base_path = Some(resolve_yaml_path(install_root, &value));
            continue;
        }

        let Some(current_base_path) = &base_path else {
            continue;
        };

        if key == "models" {
            roots.push(resolve_yaml_path(current_base_path, &value));
            continue;
        }

        if COMFYUI_MODEL_DIRS.contains(&key) {
            if let Some(parent_path) = resolve_model_type_parent(current_base_path, key, &value) {
                roots.push(parent_path);
            }
        }
    }

    if roots.is_empty() {
        if let Some(base_path) = base_path {
            roots.push(base_path.join("models"));
        }
    }

    roots
}

fn resolve_model_type_parent(
    base_path: &Path,
    directory_name: &str,
    value: &str,
) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }

    let configured_path = resolve_yaml_path(base_path, value);
    if configured_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.eq_ignore_ascii_case(directory_name))
        .unwrap_or(false)
    {
        return configured_path.parent().map(Path::to_path_buf);
    }

    Some(configured_path)
}

fn cleanup_yaml_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_end_matches('/')
        .trim_end_matches('\\')
        .to_string()
}

fn resolve_yaml_path(base_path: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base_path.join(path)
    }
}

fn find_json_string_key(value: &Value, target_key: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, child_value) in map {
                if key == target_key {
                    if let Some(text) = child_value.as_str() {
                        return Some(text.to_string());
                    }
                }

                if let Some(found_value) = find_json_string_key(child_value, target_key) {
                    return Some(found_value);
                }
            }
            None
        }
        Value::Array(values) => values
            .iter()
            .find_map(|child_value| find_json_string_key(child_value, target_key)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    fn fixture(name: &str) -> PathBuf {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lightc-comfyui-{}-{}-{}",
            name,
            std::process::id(),
            sequence
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("创建测试目录失败");
        dir
    }

    /// Structure of the official Windows portable package: outer bundle directory + inner real ComfyUI.
    #[test]
    fn finds_portable_and_integrated_install_roots() {
        let drive = fixture("portable");

        // Official portable package: ComfyUI_windows_portable\ComfyUI\models
        let portable = drive.join("ComfyUI_windows_portable");
        std::fs::create_dir_all(portable.join("ComfyUI").join("models").join("checkpoints"))
            .unwrap();
        std::fs::write(
            portable
                .join("ComfyUI")
                .join("models")
                .join("checkpoints")
                .join("sd_xl_base.safetensors"),
            vec![0u8; 4096],
        )
        .unwrap();

        // All-in-one bundle: models is directly under the root directory
        let integrated = drive.join("ComfyUI-aki-v1.6");
        std::fs::create_dir_all(integrated.join("models").join("loras")).unwrap();

        // Names that look right but are not installation directories (no models) must be ignored
        std::fs::create_dir_all(drive.join("ComfyUI_backup").join("python")).unwrap();

        let mut roots: Vec<String> = find_portable_comfyui_roots(&drive)
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        roots.sort();

        assert_eq!(roots, vec!["ComfyUI", "ComfyUI-aki-v1.6"]);

        // A portable-package path must actually yield scanned models, otherwise finding the right directory but reading nothing is useless
        let models = collect_comfyui_models(&portable.join("ComfyUI").join("models"));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "checkpoints / sd_xl_base.safetensors");

        let _ = std::fs::remove_dir_all(&drive);
    }
}
