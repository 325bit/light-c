// ============================================================================
// AI chat image attachments: write to disk and read back
//
// Storage location: {AI data directory}/attachments/{name}.jpg
//
// **Why not write base64 into the session file**: a compressed screenshot is still 200~400KB,
// and base64 makes it about 1/3 larger; the session jsonl is text appended line by line, so stuffing it in makes the file balloon quickly,
// and every history read must parse it all. After switching to "images in files, records store filenames":
//   - the session file stays small, readable, and greppable;
//   - images can be opened directly with the system image viewer (handy if users want to keep them);
//   - the UI reads them back on demand to show thumbnails and previews.
//
// The filename is the only input and **must be validated**: it is concatenated into a path, otherwise `..\` could write outside the directory.
// Names we generate naturally satisfy `[a-z0-9_-]`, and on read-back we enforce the same rule.
// ============================================================================

use std::fs;
use std::path::PathBuf;

/// Upper limit for a single attachment (a compressed image is usually 200~400KB; leave ample margin here).
/// Exceeding it means the frontend did not compress or the data is abnormal; reject directly to avoid corrupting the disk.
const MAX_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;

fn attachments_dir() -> PathBuf {
    super::config::resolve_data_dir().join("attachments")
}

/// Filenames may contain only lowercase letters, digits, hyphens, and underscores - satisfied when generated, enforced again on read-back.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 96
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.')
        // Block `..` separately: dots themselves are allowed (needed for the extension), but not as a path segment
        && !name.contains("..")
}

fn attachment_path(name: &str) -> Result<PathBuf, String> {
    if !is_safe_name(name) {
        return Err(format!("非法的附件名: {}", name));
    }
    Ok(attachments_dir().join(name))
}

/// Save one attachment and return a filename usable for read-back. The frontend writes this name into the session record.
pub fn save_attachment(data_base64: &str, media_type: &str) -> Result<String, String> {
    // Always persist as jpeg: the frontend compresses to jpeg, and this is a second safeguard to avoid odd extensions in filenames
    let _ = media_type;
    let bytes = base64_decode(data_base64)?;
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(format!("附件过大（{} 字节）", bytes.len()));
    }

    let directory = attachments_dir();
    fs::create_dir_all(&directory)
        .map_err(|error| format!("创建附件目录失败 {}: {}", directory.display(), error))?;

    // Use a timestamp + in-process counter to avoid collisions; no third-party uuid needed, this is enough
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let name = format!("ai-attach-{}-{}.jpg", stamp, next_sequence());

    let path = attachment_path(&name)?;
    fs::write(&path, bytes).map_err(|error| format!("写入附件失败 {}: {}", path.display(), error))?;
    Ok(name)
}

/// Read back an attachment and return a data URL that can be put directly into `<img src>`.
/// Returns an empty string if the file does not exist (images in history may have been cleaned; the UI treats it as "no image").
pub fn read_attachment(name: &str) -> Result<String, String> {
    let path = attachment_path(name)?;
    if !path.is_file() {
        return Ok(String::new());
    }
    let bytes = fs::read(&path).map_err(|error| format!("读取附件失败 {}: {}", path.display(), error))?;
    Ok(format!("data:image/jpeg;base64,{}", base64_encode(&bytes)))
}

// --- Minimal base64: only for attachments, no dependencies ---

const BASE64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let cleaned: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(cleaned.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in cleaned {
        let value = BASE64_TABLE
            .iter()
            .position(|c| *c == byte)
            .ok_or_else(|| "base64 内容非法".to_string())? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Ok(out)
}

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64_TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(BASE64_TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_TABLE[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// In-process incrementing counter: two saves in the same millisecond still get distinct names.
fn next_sequence() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal_in_names() {
        assert!(!is_safe_name("../../windows"));
        assert!(!is_safe_name("a/b.jpg"));
        assert!(!is_safe_name("a\\b.jpg"));
        assert!(!is_safe_name(""));
        assert!(is_safe_name("ai-attach-1760000000000-0.jpg"));
    }

    #[test]
    fn base64_round_trips() {
        // Boundaries of length 1/2/3 must all be correct (the position of padding = differs)
        for raw in [b"a".as_slice(), b"ab".as_slice(), b"abc".as_slice(), b"abcd".as_slice()] {
            let encoded = base64_encode(raw);
            assert_eq!(base64_decode(&encoded).unwrap(), raw, "编码 {}", encoded);
        }
    }
}
