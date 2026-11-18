use std::path::Path;

const MB: u64 = 1024 * 1024;
const HIGH_CONFIDENCE_MIN_SIZE: u64 = 50 * MB;
const MODEL_RUNTIME_MIN_SIZE: u64 = 100 * MB;
const CHECKPOINT_MIN_SIZE: u64 = 500 * MB;
const NOISY_MODEL_MIN_SIZE: u64 = 1024 * MB;

#[derive(Clone, Copy)]
struct ModelFileRule {
    min_size_for_mft: u64,
}

pub fn is_model_file_path(path: &Path) -> bool {
    model_file_rule(path).is_some()
}

pub fn is_supported_model_extension(path: &Path) -> bool {
    is_model_file_path(path)
}

pub fn is_model_package_directory(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return false;
    };

    matches!(extension.to_ascii_lowercase().as_str(), "mlpackage")
}

pub fn mft_model_min_size(file_name: &str) -> Option<u64> {
    model_file_rule(Path::new(file_name)).map(|rule| rule.min_size_for_mft)
}

fn model_file_rule(path: &Path) -> Option<ModelFileRule> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if is_tensorflow_checkpoint_data_file(&file_name) {
        return Some(ModelFileRule {
            min_size_for_mft: CHECKPOINT_MIN_SIZE,
        });
    }

    let extension = path
        .extension()
        .and_then(|value| value.to_str())?
        .to_ascii_lowercase();

    let rule = match extension.as_str() {
        // The most common weight formats for local LLM/diffusion models; the extension itself has high confidence, so the threshold can be lowered to cover LoRA.
        "safetensors" | "gguf" | "ggml" => ModelFileRule {
            min_size_for_mft: HIGH_CONFIDENCE_MIN_SIZE,
        },
        // Common PyTorch/Stable Diffusion checkpoints, usually large, but .ckpt may also be reused by a few pieces of software.
        "ckpt" => ModelFileRule {
            min_size_for_mft: CHECKPOINT_MIN_SIZE,
        },
        // Inference runtime / cross-platform model formats; the extension is quite specific, suitable for direct recognition in platform directories and MFT fallback.
        "onnx" | "ort" | "tflite" | "pb" | "h5" | "hdf5" | "keras" | "mlmodel" | "mnn" | "rknn"
        | "mindir" | "om" | "engine" | "plan" | "trt" | "uff" | "pdmodel" | "pdiparams"
        | "pdparams" | "caffemodel" | "dlc" | "hef" | "xmodel" | "bmodel" | "pte" | "task"
        | "mar" | "nemo" => ModelFileRule {
            min_size_for_mft: MODEL_RUNTIME_MIN_SIZE,
        },
        // Older ecosystems like Darknet use .weights, but that name may also be reused by ordinary software, so the threshold is higher than for generic runtime formats.
        "weights" | "t7" => ModelFileRule {
            min_size_for_mft: CHECKPOINT_MIN_SIZE,
        },
        // These extensions are too common in Python/dev environments; only treat them as model candidates when very large or in a platform directory.
        "bin" | "pt" | "pth" | "pkl" | "pickle" | "joblib" | "npz" | "params" => ModelFileRule {
            min_size_for_mft: NOISY_MODEL_MIN_SIZE,
        },
        _ => return None,
    };

    Some(rule)
}

fn is_tensorflow_checkpoint_data_file(file_name: &str) -> bool {
    // TensorFlow checkpoint weights often exist as model.ckpt.data-00000-of-00001, so you cannot judge by the final extension alone.
    file_name.contains(".ckpt.data-") || file_name.starts_with("ckpt.data-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn recognizes_mainstream_model_formats() {
        for file_name in [
            "model.safetensors",
            "qwen.gguf",
            "stable_diffusion.ckpt",
            "model.onnx",
            "model.tflite",
            "saved_model.pb",
            "model.keras",
            "model.hdf5",
            "model.mlmodel",
            "model.ort",
            "model.engine",
            "model.trt",
            "model.pdiparams",
            "model.caffemodel",
            "model.dlc",
            "model.hef",
            "model.xmodel",
            "model.bmodel",
            "model.pte",
            "model.task",
            "model.nemo",
            "model.mindir",
        ] {
            assert!(is_model_file_path(Path::new(file_name)), "{}", file_name);
        }
    }

    #[test]
    fn recognizes_model_package_directories() {
        assert!(is_model_package_directory(Path::new("MobileNet.mlpackage")));
    }

    #[test]
    fn supports_tensorflow_checkpoint_data_shards() {
        assert_eq!(
            mft_model_min_size("model.ckpt.data-00000-of-00001"),
            Some(CHECKPOINT_MIN_SIZE)
        );
    }
}
