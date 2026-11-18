// ============================================================================
// Incremental parsing of streaming responses
//
// The server returns SSE (Server-Sent Events); two pitfalls must be handled:
//
// 1. **Packet boundaries don't align with line boundaries**. Network data arrives in chunks; one `data: {...}` may be split in two,
//    or several records may arrive at once. So we must buffer ourselves, process only "complete lines", and keep the rest for the next chunk.
// 2. **The two protocols have different incremental formats**:
//      OpenAI-compatible: data: {"choices":[{"delta":{"content":"text"}}]}
//      Anthropic:  data: {"type":"content_block_delta","delta":{"text":"text"}}
//    The end markers also differ, handled by each protocol's own parse function.
//
// Everything here is a pure function that doesn't touch the network — convenient for unit tests pinning down these two pitfalls.
// ============================================================================

use super::presets::AiProtocol;
use super::tools::{
    AiToolCall, TOOL_PROPOSE_DELETE, TOOL_READ_ITEMS, TOOL_READ_ITEM_DETAIL, TOOL_SELECT_ITEMS,
};
use std::collections::BTreeMap;

/// Accumulation of a single tool call's fragments.
///
/// In streaming, tool arguments are **half a JSON emitted across multiple pieces** and must be reassembled by index:
/// OpenAI gives `delta.tool_calls[{index, id?, function:{name?, arguments?}}]`,
/// where only the first fragment has id and name; Anthropic uses `content_block_start` (with id/name)
/// plus a series of `input_json_delta` (with partial_json).
#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Cap on leftover incomplete-line data. A normal line is one SSE data block (tens of KB);
/// hitting 4MB means the server keeps emitting data with no newline — that's an anomaly and must be cut off, not allowed to grow memory forever.
const MAX_BUFFER_BYTES: usize = 4 * 1024 * 1024;

/// Incremental parser for SSE data blocks.
/// Usage: call `push_bytes` on every chunk of network data and take the returned incremental text; call `finish` when the stream ends.
pub struct StreamDecoder {
    protocol: AiProtocol,
    /// Leftover data that doesn't yet form a complete line.
    ///
    /// ⚠️ Here we store **bytes**, not a string: network fragments have nothing to do with UTF-8 character boundaries,
    /// a Chinese character can easily be split into "first 2 bytes + last 1 byte". If we decoded per fragment as it arrived,
    /// half a character would become mojibake — and this text is stored, so a wrong character would persist forever in the session record.
    /// So we split complete lines at the byte level on `\n`, then decode the whole line.
    buffer: Vec<u8>,
    /// Server has declared the end (data: [DONE] or similar)
    pub finished: bool,
    /// Buffer exceeded the cap (server never emits a newline). The caller seeing this should report an error and abort.
    pub overflowed: bool,
    /// Chain of thought (reasoning_content) in thinking mode.
    ///
    /// ⚠️ **Must be sent back to the API verbatim**: DeepSeek explicitly requires that when a request carries `tools`,
    /// the intermediate assistant turn's reasoning_content be sent back in full in all subsequent requests,
    /// even if that turn didn't actually call a tool — if not sent back, you get a 400.
    /// （"The reasoning_content in the thinking mode must be passed back to the API"）。
    /// It **does not go into the body**: both UI display and persisted content use only content.
    reasoning: String,
    /// Accumulating tool calls, sorted by fragment index to keep order stable
    partial_tools: BTreeMap<usize, PartialToolCall>,
}

impl StreamDecoder {
    pub fn new(protocol: AiProtocol) -> Self {
        Self {
            protocol,
            buffer: Vec::new(),
            finished: false,
            overflowed: false,
            reasoning: String::new(),
            partial_tools: BTreeMap::new(),
        }
    }

    /// Takes this round's accumulated chain of thought. Empty means this model/round had no thinking content.
    pub fn take_reasoning(&mut self) -> String {
        std::mem::take(&mut self.reasoning)
    }

    /// Feeds in a piece of raw text; internally it's the byte version, **only for unit tests** (production paths all use `push_bytes`).
    /// Marked `cfg(test)` so it doesn't produce a dead_code warning in release builds.
    #[cfg(test)]
    pub fn push(&mut self, chunk: &str) -> Vec<String> {
        self.push_bytes(chunk.as_bytes())
    }

    /// Feeds in a chunk of raw bytes, returning all incremental text fragments parseable this time.
    pub fn push_bytes(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        if self.buffer.len() > MAX_BUFFER_BYTES {
            // Clear the buffer and set the flag: the caller aborts based on this, rather than growing memory while pretending all is normal
            self.buffer.clear();
            self.overflowed = true;
            return Vec::new();
        }

        let mut deltas = Vec::new();

        // Only process complete lines; the last segment (possibly half a line) stays in the buffer for the next chunk
        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            // This line is fully received, so decoding now won't cut any multibyte character
            let line: Vec<u8> = self.buffer.drain(..=index).collect();
            let text = String::from_utf8_lossy(&line).to_string();

            if let Some(delta) = self.parse_line(&text) {
                deltas.push(delta);
            }
        }
        deltas
    }

    /// Stream end: also process any last line that may remain in the buffer.
    pub fn finish(&mut self) -> Vec<String> {
        if self.buffer.iter().all(|byte| byte.is_ascii_whitespace()) {
            return Vec::new();
        }
        let remainder = std::mem::take(&mut self.buffer);
        let text = String::from_utf8_lossy(&remainder).to_string();
        self.parse_line(&text).into_iter().collect()
    }

    /// Accumulates this fragment's tool increment (if any). Must be called before taking the body.
    fn record_tool_fragments(&mut self, value: &serde_json::Value) {
        match self.protocol {
            AiProtocol::OpenAi => {
                // delta.tool_calls[]: accumulate by index, only the first fragment has id and name
                let Some(calls) = value
                    .get("choices")
                    .and_then(|choices| choices.get(0))
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|delta| delta.get("tool_calls"))
                    .and_then(|calls| calls.as_array())
                else {
                    return;
                };
                for fragment in calls {
                    let index = fragment
                        .get("index")
                        .and_then(|item| item.as_u64())
                        .unwrap_or(0) as usize;
                    let entry = self.partial_tools.entry(index).or_default();
                    if let Some(id) = fragment.get("id").and_then(|item| item.as_str()) {
                        entry.id = id.to_string();
                    }
                    if let Some(function) = fragment.get("function") {
                        if let Some(name) = function.get("name").and_then(|item| item.as_str()) {
                            entry.name.push_str(name);
                        }
                        if let Some(args) = function.get("arguments").and_then(|item| item.as_str())
                        {
                            entry.arguments.push_str(args);
                        }
                    }
                }
            }
            AiProtocol::Anthropic => {
                // content_block_start gives id/name, then a series of input_json_delta gives the argument fragments
                let index = value
                    .get("index")
                    .and_then(|item| item.as_u64())
                    .unwrap_or(0) as usize;
                if value.get("type").and_then(|item| item.as_str()) == Some("content_block_start") {
                    if let Some(block) = value.get("content_block") {
                        if block.get("type").and_then(|item| item.as_str()) == Some("tool_use") {
                            let entry = self.partial_tools.entry(index).or_default();
                            if let Some(id) = block.get("id").and_then(|item| item.as_str()) {
                                entry.id = id.to_string();
                            }
                            if let Some(name) = block.get("name").and_then(|item| item.as_str()) {
                                entry.name = name.to_string();
                            }
                        }
                    }
                } else if let Some(delta) = value.get("delta") {
                    if delta.get("type").and_then(|item| item.as_str()) == Some("input_json_delta")
                    {
                        if let Some(partial) =
                            delta.get("partial_json").and_then(|item| item.as_str())
                        {
                            self.partial_tools
                                .entry(index)
                                .or_default()
                                .arguments
                                .push_str(partial);
                        }
                    }
                }
            }
        }
    }

    /// Takes the completed accumulated tool calls. On argument parse failure it gives an empty object —
    /// the caller treats it as "no id" and won't use it for any operation.
    pub fn take_tool_calls(&mut self) -> Vec<AiToolCall> {
        std::mem::take(&mut self.partial_tools)
            .into_iter()
            .filter(|(_, call)| {
                !call.name.is_empty()
                    && (call.name == TOOL_SELECT_ITEMS
                        || call.name == TOOL_PROPOSE_DELETE
                        || call.name == TOOL_READ_ITEMS
                        || call.name == TOOL_READ_ITEM_DETAIL)
            })
            .map(|(_, call)| AiToolCall {
                call_id: call.id,
                name: call.name,
                arguments: serde_json::from_str(&call.arguments)
                    .unwrap_or_else(|_| serde_json::json!({})),
            })
            .collect()
    }

    /// Parses a single SSE line. Returns Some if this line contributed a piece of body text.
    fn parse_line(&mut self, line: &str) -> Option<String> {
        let trimmed = line.trim();
        // Blank lines, comment lines (like :heartbeat), and event: lines contain no body text
        if trimmed.is_empty() || trimmed.starts_with(':') || trimmed.starts_with("event:") {
            return None;
        }

        let payload = trimmed.strip_prefix("data:")?.trim();
        if payload == "[DONE]" {
            self.finished = true;
            return None;
        }

        let value: serde_json::Value = serde_json::from_str(payload).ok()?;

        // The server sometimes puts errors into the stream; these must also be recognized, otherwise the UI "freezes with no output"
        if let Some(error) = extract_stream_error(&value) {
            self.finished = true;
            return Some(format!("\n[服务端返回错误] {}", error));
        }

        // First accumulate this fragment's tool increment, then take the body — both may appear together
        self.record_tool_fragments(&value);

        // Thinking-mode chain of thought is collected separately, **not into the body** (see the reasoning field's description for the send-back rule)
        if let Some(fragment) = extract_reasoning_fragment(&value, self.protocol) {
            self.reasoning.push_str(&fragment);
        }

        match self.protocol {
            AiProtocol::OpenAi => parse_openai_delta(&value),
            AiProtocol::Anthropic => parse_anthropic_delta(&value),
        }
    }
}

/// OpenAI-compatible: choices[0].delta.content
fn parse_openai_delta(value: &serde_json::Value) -> Option<String> {
    let text = value
        .get("choices")?
        .get(0)?
        .get("delta")?
        .get("content")?
        .as_str()?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Anthropic：{"type":"content_block_delta","delta":{"type":"text_delta","text":"..."}}
fn parse_anthropic_delta(value: &serde_json::Value) -> Option<String> {
    let text = value.get("delta")?.get("text")?.as_str()?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Takes the thinking content in this fragment.
///
/// OpenAI-compatible protocols (DeepSeek etc.) put the chain of thought in `choices[0].delta.reasoning_content`,
/// **at the same level** as `content`; the body side reads only `content`, so it must be collected separately here.
/// Anthropic thinking is a separate content block, and our Anthropic path doesn't enable thinking, so it's not handled here.
fn extract_reasoning_fragment(value: &serde_json::Value, protocol: AiProtocol) -> Option<String> {
    match protocol {
        AiProtocol::OpenAi => value
            .get("choices")?
            .get(0)?
            .get("delta")?
            .get("reasoning_content")?
            .as_str()
            .map(|text| text.to_string()),
        AiProtocol::Anthropic => None,
    }
}

/// Extracts error info from streaming fragments (both providers may put error in any fragment).
fn extract_stream_error(value: &serde_json::Value) -> Option<String> {
    let error = value.get("error")?;
    if let Some(message) = error.get("message").and_then(|item| item.as_str()) {
        return Some(message.to_string());
    }
    error.as_str().map(|text| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_deltas_are_extracted_and_done_is_detected() {
        let mut decoder = StreamDecoder::new(AiProtocol::OpenAi);
        let deltas = decoder.push(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n\
             data: [DONE]\n",
        );
        assert_eq!(deltas, vec!["你".to_string(), "好".to_string()]);
        assert!(decoder.finished);
    }

    #[test]
    fn partial_lines_are_buffered_until_complete() {
        let mut decoder = StreamDecoder::new(AiProtocol::OpenAi);
        // This chunk deliberately splits a line in half: nothing should be parsed out
        let first = decoder.push("data: {\"choices\":[{\"delta\":{\"cont");
        assert!(first.is_empty(), "半行不该产出增量");
        // produced only after completion
        let second = decoder.push("ent\":\"完整\"}}]}\n");
        assert_eq!(second, vec!["完整".to_string()]);
    }

    #[test]
    fn anthropic_delta_uses_nested_text_field() {
        let mut decoder = StreamDecoder::new(AiProtocol::Anthropic);
        let deltas = decoder.push(
            "event: content_block_delta\n\
             data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"嗨\"}}\n",
        );
        assert_eq!(deltas, vec!["嗨".to_string()]);
    }

    #[test]
    fn openai_tool_call_fragments_are_reassembled_by_index() {
        // In real streaming, arguments arrive in fragments and only the first has id/name — this is the easiest place to get assembly wrong
        let mut decoder = StreamDecoder::new(AiProtocol::OpenAi);
        assert!(decoder
            .push("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"select_items\",\"arguments\":\"\"}}]}}]}\n")
            .is_empty());
        decoder.push("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"ids\\\":[\\\"Re\"}}]}}]}\n");
        decoder.push("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"cycleBin\\\"]}\"}}]}}]}\n");
        decoder.push("data: [DONE]\n");

        let calls = decoder.take_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "select_items");
        assert_eq!(calls[0].call_id, "call_1");
        // The reassembled arguments must yield the id when parsed
        assert_eq!(
            calls[0]
                .arguments
                .get("ids")
                .and_then(|v| v.as_array())
                .map(|a| a.len()),
            Some(1)
        );
    }

    #[test]
    fn anthropic_partial_json_is_reassembled() {
        let mut decoder = StreamDecoder::new(AiProtocol::Anthropic);
        decoder.push("data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"propose_delete\"}}\n");
        decoder.push("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"ids\\\":\"}}\n");
        decoder.push("data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"[\\\"WindowsTemp\\\"]}\"}}\n");

        let calls = decoder.take_tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "propose_delete");
        assert_eq!(
            calls[0]
                .arguments
                .get("ids")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str()),
            Some("WindowsTemp")
        );
    }

    #[test]
    fn unknown_tool_names_are_dropped_in_stream() {
        // Only recognize the two declared tools, discard everything else — to avoid "undeclared tool executed"
        let mut decoder = StreamDecoder::new(AiProtocol::OpenAi);
        decoder.push("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"x\",\"function\":{\"name\":\"run_shell\",\"arguments\":\"{}\"}}]}}]}\n");
        assert!(decoder.take_tool_calls().is_empty());
    }

    #[test]
    fn error_inside_stream_is_surfaced() {
        let mut decoder = StreamDecoder::new(AiProtocol::OpenAi);
        let deltas = decoder.push("data: {\"error\":{\"message\":\"额度不足\"}}\n");
        assert_eq!(deltas.len(), 1);
        assert!(deltas[0].contains("额度不足"));
        assert!(decoder.finished);
    }

    #[test]
    fn finish_flushes_a_trailing_line_without_newline() {
        let mut decoder = StreamDecoder::new(AiProtocol::OpenAi);
        assert!(decoder
            .push("data: {\"choices\":[{\"delta\":{\"content\":\"尾\"}}]}")
            .is_empty());
        assert_eq!(decoder.finish(), vec!["尾".to_string()]);
    }

    #[test]
    fn a_character_split_across_two_chunks_is_not_corrupted() {
        // Real networks fragment by bytes, and fragment boundaries have nothing to do with UTF-8 character boundaries.
        // Here we deliberately split a 3-byte Chinese character (E5 A5 BD) down the middle and feed it in: decoding must wait until the whole line is complete,
        // otherwise you get replacement characters — and this text is used for both live display and persistence, so a wrong character is permanent.
        let line = "data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n";
        let bytes = line.as_bytes();
        let split = bytes
            .iter()
            .position(|byte| *byte == 0xE5)
            .expect("这一行里应当有汉字的首字节")
            + 1;

        let mut decoder = StreamDecoder::new(AiProtocol::OpenAi);
        assert!(
            decoder.push_bytes(&bytes[..split]).is_empty(),
            "半行不该产出增量"
        );
        assert_eq!(decoder.push_bytes(&bytes[split..]), vec!["好".to_string()]);
    }

    #[test]
    fn a_flood_without_newlines_is_rejected_instead_of_growing_forever() {
        let mut decoder = StreamDecoder::new(AiProtocol::OpenAi);
        let flood = vec![b'a'; MAX_BUFFER_BYTES + 1];
        assert!(decoder.push_bytes(&flood).is_empty());
        assert!(decoder.overflowed, "顶到上限必须立标记，交给调用方报错中断");
    }

    #[test]
    fn reasoning_content_is_collected_without_leaking_into_the_text() {
        // In thinking mode reasoning_content is at the same level as content: the body takes only content,
        // while the chain of thought must be accumulated separately — it **must be sent back to the API** (400 otherwise), but shouldn't be shown to the user.
        let mut decoder = StreamDecoder::new(AiProtocol::OpenAi);
        let deltas = decoder.push(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"先看路径\"}}]}\n\
             data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"再判断用途\"}}]}\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"这是缓存目录。\"}}]}\n",
        );
        assert_eq!(
            deltas,
            vec!["这是缓存目录。".to_string()],
            "思维链不该混进正文"
        );
        assert_eq!(decoder.take_reasoning(), "先看路径再判断用途");
        assert_eq!(decoder.take_reasoning(), "", "取过一次就空了");
    }
}
