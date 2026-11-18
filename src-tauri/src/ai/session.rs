// ============================================================================
// AI session persistence
//
// Location: {data dir}/ai/sessions/{module id}/{YYYY-MM-DD}.jsonl
//
// All three choices have real reasons:
// - **One directory per module**: switching modules only loads that module's history, which is both "module isolation"
//   and avoids cross-talk like "seeing junk-cleanup conversations in the old driver".
// - **One file per date**: jsonl is append-only, so a single crash won't destroy the whole history;
//   clearing expired sessions is also just deleting files, no read-modify-write.
// - **Date passed in by the frontend**: the frontend knows best what "today" is in the local timezone; pulling in a date library on the backend isn't worth it.
//
// ⚠️ module_id and date are concatenated into the file path, **they must be validated first**:
//    otherwise input like `..\\..\\` could write outside the data directory (path traversal).
// ============================================================================

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// A single session message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSessionMessage {
    /// Only user / assistant allowed; system prompts are fetched fresh each time, not persisted
    pub role: String,
    pub content: String,
    /// Attachment file names (stored under the attachments directory).
    /// **Store only file names, not base64** — otherwise image data would bloat the session file.
    #[serde(default)]
    pub images: Vec<String>,
    /// Unix millisecond timestamp, for time-based sorting and display
    pub at: i64,
    /// The chain of thought for this round in reasoning mode.
    ///
    /// **Persisting is mandatory**: when a request carries tools, the server requires the assistant's reasoning_content
    /// to be returned verbatim (otherwise 400), and history restored from disk after a restart must be sent with it too.
    #[serde(default)]
    pub reasoning_content: Option<String>,
    /// This assistant message was **generated locally** (a tool query result), not spoken by the model.
    ///
    /// When sending requests it must be treated as user-side content — if treated as assistant, it becomes
    /// "an assistant message with no reasoning_content", and requests with tools get a 400 from the server.
    #[serde(default)]
    pub local_tool_output: bool,
    /// The **profile id** that produced this reply.
    ///
    /// A chain of thought only means something to the provider that produced it: switch providers (especially strict third-party gateways) and it becomes an unknown field,
    /// possibly rejected with 400. So on requests, return only those "produced by the current profile" —
    /// this field is the basis for that decision and must be persisted, otherwise it can't be recognized after a restart.
    #[serde(default)]
    pub produced_by_profile: Option<String>,
}

/// Session root directory: {AI data dir}/sessions
///
/// Go through config's resolution logic instead of assembling the path again —
/// "Clear local data" must compute size and delete along the same path; the two must always point to the same place.
pub fn sessions_root() -> PathBuf {
    super::config::resolve_data_dir().join("sessions")
}

/// Validate module id: only lowercase letters, digits and hyphens allowed (built-in module ids all have this shape).
/// Reject path separators and `..`, blocking path traversal at the source.
fn is_safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-')
}

/// Validate date: must be a strict YYYY-MM-DD.
fn is_safe_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| match index {
        4 | 7 => *byte == b'-',
        _ => byte.is_ascii_digit(),
    })
}

fn session_file(module_id: &str, date: &str) -> Result<PathBuf, String> {
    if !is_safe_segment(module_id) {
        return Err(format!("非法的模块标识: {}", module_id));
    }
    if !is_safe_date(date) {
        return Err(format!("非法的日期格式（应为 YYYY-MM-DD）: {}", date));
    }
    Ok(sessions_root().join(module_id).join(format!("{}.jsonl", date)))
}

/// Append one message. Creates the directory automatically if it doesn't exist.
pub fn append_message(module_id: &str, date: &str, message: AiSessionMessage) -> Result<(), String> {
    let path = session_file(module_id, date)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建会话目录失败 {}: {}", parent.display(), error))?;
    }

    let line = serde_json::to_string(&message).map_err(|error| format!("序列化会话消息失败: {}", error))?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("打开会话文件失败 {}: {}", path.display(), error))?;
    writeln!(file, "{}", line).map_err(|error| format!("写入会话消息失败: {}", error))?;
    Ok(())
}

/// Read all messages for a module on a given day.
/// A missing file returns an empty list ("no history yet" is a normal state, not an error);
/// a single line's parse failure only skips that line — one bad record shouldn't make the whole history unreadable.
pub fn load_session(module_id: &str, date: &str) -> Result<Vec<AiSessionMessage>, String> {
    let path = session_file(module_id, date)?;
    if !path.exists() {
        return Ok(Vec::new());
    }

    let text = fs::read_to_string(&path)
        .map_err(|error| format!("读取会话文件失败 {}: {}", path.display(), error))?;

    let mut messages = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AiSessionMessage>(line) {
            Ok(message) => messages.push(message),
            Err(error) => log::warn!("跳过损坏的会话记录 {} 第 {} 行: {}", path.display(), index + 1, error),
        }
    }
    Ok(messages)
}

/// Delete a module's session file for a given day (the UI's "Clear conversation").
/// A file that doesn't exist is also a success — the goal is "this state after clearing", not "must delete a file".
pub fn clear_session(module_id: &str, date: &str) -> Result<(), String> {
    let path = session_file(module_id, date)?;
    if !path.exists() {
        return Ok(());
    }
    fs::remove_file(&path).map_err(|error| format!("删除会话文件失败 {}: {}", path.display(), error))
}

/// Result of a range read.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSessionPage {
    /// **In chronological order** (old → new), the frontend can prepend it to the message list
    pub messages: Vec<AiSessionMessage>,
    /// Whether there is older data left (UI shows "Load earlier" accordingly)
    pub has_more: bool,
}

/// Max messages per read: whatever the frontend passes is clamped to this range, **preventing one read from blowing up** at the source.
const MAX_RANGE_MESSAGES: usize = 200;

/// Pick dates to read from candidate file names: keep only valid ones **no later than** `end_date`, in descending time order.
///
/// Extracted separately so it can be unit-tested — it decides "which days to read", and getting it wrong means "missing history" or "reading the future".
fn candidate_dates(names: Vec<String>, end_date: &str) -> Vec<String> {
    let mut dates: Vec<String> = names
        .into_iter()
        .filter_map(|name| name.strip_suffix(".jsonl").map(str::to_string))
        // `YYYY-MM-DD` compares lexicographically in time order, no date library needed
        .filter(|date| is_safe_date(date) && date.as_str() <= end_date)
        .collect();
    dates.sort_unstable();
    dates.reverse();
    dates
}

/// Read backward from `end_date`, stop once `max_messages` have been collected.
///
/// Sessions are **one file per day**, so "load earlier" naturally means "read a few more days back":
/// read day by day in reverse date order, return when we have enough, no date arithmetic.
pub fn load_session_range(
    module_id: &str,
    end_date: &str,
    max_messages: usize,
) -> Result<AiSessionPage, String> {
    if !is_safe_segment(module_id) {
        return Err(format!("非法的模块标识: {}", module_id));
    }
    if !is_safe_date(end_date) {
        return Err(format!("非法的日期格式（应为 YYYY-MM-DD）: {}", end_date));
    }
    let limit = max_messages.clamp(1, MAX_RANGE_MESSAGES);

    let directory = sessions_root().join(module_id);
    if !directory.is_dir() {
        return Ok(AiSessionPage { messages: Vec::new(), has_more: false });
    }

    let names = fs::read_dir(&directory)
        .map_err(|error| format!("读取会话目录失败 {}: {}", directory.display(), error))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .collect::<Vec<String>>();
    let dates = candidate_dates(names, end_date);

    let mut collected: Vec<AiSessionMessage> = Vec::new();
    let mut consumed = 0usize;
    for date in &dates {
        // Read day by day from newest to oldest: the newer day is prepended **in front**, so the whole result ends up chronological
        let mut merged = load_session(module_id, date)?;
        merged.append(&mut collected);
        collected = merged;
        consumed += 1;
        if collected.len() >= limit {
            break;
        }
    }

    // ⚠️ **Return whole days**, never truncate in the middle of a day:
    // "Load earlier" uses "the day before the current earliest message's date" as its boundary — if we cut mid-day here,
    // that half day would never be readable (the user's history would have a missing chunk).
    // Only the extreme case of **a single day exceeding twice the cap** gets truncated: better a small gap than one giant read.
    let mut has_more = dates.len() > consumed;
    if collected.len() > limit.saturating_mul(2) {
        collected.drain(0..collected.len() - limit);
        has_more = true;
    }

    Ok(AiSessionPage { messages: collected, has_more })
}

/// Clear a module's sessions for **all dates** (the UI's "Clear conversation").
///
/// Deleting only today's file isn't enough: history loads by day, so after deleting today, yesterday's gets loaded back —
/// what the user sees is "cleared but it came back". So clearing = delete the module's entire directory.
pub fn clear_all_sessions(module_id: &str) -> Result<(), String> {
    if !is_safe_segment(module_id) {
        return Err(format!("非法的模块标识: {}", module_id));
    }
    let directory = sessions_root().join(module_id);
    if !directory.is_dir() {
        return Ok(());
    }
    fs::remove_dir_all(&directory)
        .map_err(|error| format!("清空会话目录失败 {}: {}", directory.display(), error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal_in_module_id_and_date() {
        // These two are concatenated into paths, they must be blocked
        assert!(!is_safe_segment("../../windows"));
        assert!(!is_safe_segment("junk/clean"));
        assert!(!is_safe_segment("junk\\clean"));
        assert!(!is_safe_segment(""));
        assert!(is_safe_segment("junk-clean"));
        assert!(is_safe_segment("driver-cleanup"));
    }

    #[test]
    fn rejects_malformed_dates() {
        assert!(!is_safe_date("2026-1-1"));
        assert!(!is_safe_date("2026/10/14"));
        assert!(!is_safe_date("../2026-10"));
        assert!(is_safe_date("2026-10-14"));
    }

    #[test]
    fn session_file_is_built_under_the_module_directory() {
        let path = session_file("junk-clean", "2026-10-14").expect("合法输入应通过");
        let text = path.to_string_lossy().replace('\\', "/");
        assert!(text.ends_with("ai/sessions/junk-clean/2026-10-14.jsonl"), "实际得到 {}", text);
    }

    #[test]
    fn candidate_dates_keep_valid_past_dates_newest_first() {
        // Only keep valid dates **no later than** end_date; order must be newest first (load order depends on it)
        let names = vec![
            "2026-10-14.jsonl".to_string(),
            "2026-10-16.jsonl".to_string(),
            "2026-10-15.jsonl".to_string(),
            "2026-10-17.jsonl".to_string(),
            "not-a-date.jsonl".to_string(),
            "2026-10-1.jsonl".to_string(),
            "README.txt".to_string(),
        ];
        let dates = candidate_dates(names, "2026-10-16");
        assert_eq!(
            dates.iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["2026-10-16", "2026-10-15", "2026-10-14"]
        );
    }

    #[test]
    fn candidate_dates_are_empty_when_nothing_is_old_enough() {
        let names = vec!["2026-10-18.jsonl".to_string()];
        assert!(candidate_dates(names, "2026-10-16").is_empty());
    }
}
