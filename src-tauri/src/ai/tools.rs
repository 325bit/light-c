// ============================================================================
// AI tool calling (function calling)
//
// The "capability" of a tool itself lives in the **module**; this file only handles two things:
//   1. Declaration: assemble the allowed tools into the request in the protocol's shape
//   2. Parsing: extract the call intent returned by the model and hand it to the frontend for execution
//
// By design there are deliberately only **two general-purpose tools**, both taking "the id given by the module itself" as a parameter:
//   select_items(ids)     —— P3a: change the checked state, no risk
//   propose_delete(ids)   —— P3b: request deletion, must be confirmed by the user before execution
//
// This way the backend **doesn't need to know any module's details** — which object the model operates on is determined by the
// actionable items with ids that the module gives in the summary. Adding a new module only requires providing targets; nothing on the tool side needs to change.
// ============================================================================

use super::presets::AiProtocol;
use serde::Serialize;
use serde_json::Value;

/// A request from the model to call a tool.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiToolCall {
    /// Call id given by the protocol (needed when Anthropic feeds results back; currently not fed back, kept for now)
    pub call_id: String,
    pub name: String,
    /// Arguments parsed into an object; on parse failure it is an empty object, and the caller treats it as "no id"
    pub arguments: Value,
}

/// Names of the allowed tools. The frontend decides which to pass based on "which actions the module registered".
pub const TOOL_SELECT_ITEMS: &str = "select_items";
pub const TOOL_PROPOSE_DELETE: &str = "propose_delete";
pub const TOOL_READ_ITEMS: &str = "read_items";
pub const TOOL_READ_ITEM_DETAIL: &str = "read_item_detail";

/// Description and parameter schema of a single tool (shared by both protocols, only the outer wrapper differs).
fn tool_spec(name: &str) -> Option<(&'static str, Value)> {
    match name {
        TOOL_SELECT_ITEMS => Some((
            "⚠️ 仅在用户明确要求勾选时使用（例如\"帮我勾上可安全删除的\"）。\
             用户问\"这是什么、有什么用、能不能删\"属于提问，**不要调用本工具**，先回答问题。\
             作用：把指定的项加入勾选，只改变勾选状态、不会删除任何东西；删除仍由用户在界面上确认。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "要勾选的项 id；必须是摘要里给出的 [id: xxx] 中的 xxx"
                    }
                },
                "required": ["ids"]
            }),
        )),
        TOOL_PROPOSE_DELETE => Some((
            "⚠️ 仅在用户明确要求删除时使用（例如\"帮我删掉回收站\"）。\
             用户问\"能不能删\"属于询问、不是指令，**不要调用本工具**，只回答并说明风险。\
             作用：申请删除指定的项；不会立即删除，界面会弹确认由用户决定。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "要删除的项 id；必须是摘要里给出的 [id: xxx] 中的 xxx"
                    }
                },
                "required": ["ids"]
            }),
        )),
        TOOL_READ_ITEMS => Some((
            "读取模块数据的下一页明细。\
             ⚠️ 这是**只读**操作：用户问「这个分类里有什么」「具体是哪些文件」时直接调用。\
             ⚠️ 一次只返回一页：请看末尾写明的「第 N 到 M 条，共 X 条」，\
             没看到全部就不要下「全部都…」这类结论；需要更多就再调一次并加大 offset。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "target_id": {
                        "type": "string",
                        "description": "要读取的分类 id（摘要里 [id: xxx] 中的 xxx）。\
                                        模块只有一条主列表时**留空即可**。"
                    },
                    "offset": { "type": "integer", "description": "从第几条开始，默认 0" },
                    "limit": { "type": "integer", "description": "这一页读多少条，默认 30、上限 100" }
                },
                "required": []
            }),
        )),
        TOOL_READ_ITEM_DETAIL => Some((
            "读取单个条目的完整信息（完整路径、大小、时间等）。\n             ⚠️ 只读操作，任何时候都可以调用。\n             item_id 用完整路径即可：可以来自摘要、read_items 的结果，**也可以是用户在自己问题里给出的那个路径**（例如行内「咨询 AI 助手」按钮带过来的路径）—— 不确定就先用 read_items 找一下。",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "item_id": { "type": "string", "description": "条目 id（通常就是完整路径）" }
                },
                "required": ["item_id"]
            }),
        )),
        // Ignore unrecognized tool names directly: a mistake by the frontend shouldn't fail the whole request
        _ => None,
    }
}

/// Tool declarations for the OpenAI-compatible protocol.
pub fn openai_tools(names: &[String]) -> Vec<Value> {
    names
        .iter()
        .filter_map(|name| {
            tool_spec(name).map(|(description, parameters)| {
                serde_json::json!({
                    "type": "function",
                    "function": { "name": name, "description": description, "parameters": parameters }
                })
            })
        })
        .collect()
}

/// Tool declarations for the Anthropic protocol (schema goes in input_schema, no function wrapper).
pub fn anthropic_tools(names: &[String]) -> Vec<Value> {
    names
        .iter()
        .filter_map(|name| {
            tool_spec(name).map(|(description, input_schema)| {
                serde_json::json!({
                    "name": name,
                    "description": description,
                    "input_schema": input_schema
                })
            })
        })
        .collect()
}

/// Extracts tool calls from a response.
///
/// OpenAI-compatible: choices[0].message.tool_calls[] = {id, function:{name, arguments}},
/// where arguments is **JSON in string form** and needs a second parse — this step is often missed.
/// Anthropic: blocks in content[] with type == "tool_use"; input is already an object.
pub fn parse_tool_calls(protocol: AiProtocol, value: &Value) -> Vec<AiToolCall> {
    match protocol {
        AiProtocol::OpenAi => parse_openai_tool_calls(value),
        AiProtocol::Anthropic => parse_anthropic_tool_calls(value),
    }
}

fn parse_arguments_string(raw: &str) -> Value {
    // Give an empty object on argument parse failure: the caller treats it as "no id" and won't delete by mistake
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({}))
}

fn parse_openai_tool_calls(value: &Value) -> Vec<AiToolCall> {
    let Some(calls) = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("tool_calls"))
        .and_then(|calls| calls.as_array())
    else {
        return Vec::new();
    };

    calls
        .iter()
        .filter_map(|call| {
            let name = call.get("function")?.get("name")?.as_str()?.to_string();
            let raw = call
                .get("function")
                .and_then(|function| function.get("arguments"))
                .and_then(|arguments| arguments.as_str())
                .unwrap_or("{}");
            Some(AiToolCall {
                call_id: call
                    .get("id")
                    .and_then(|id| id.as_str())
                    .unwrap_or_default()
                    .to_string(),
                name,
                arguments: parse_arguments_string(raw),
            })
        })
        .collect()
}

fn parse_anthropic_tool_calls(value: &Value) -> Vec<AiToolCall> {
    let Some(blocks) = value.get("content").and_then(|content| content.as_array()) else {
        return Vec::new();
    };

    blocks
        .iter()
        .filter(|block| block.get("type").and_then(|kind| kind.as_str()) == Some("tool_use"))
        .filter_map(|block| {
            Some(AiToolCall {
                call_id: block.get("id")?.as_str()?.to_string(),
                name: block.get("name")?.as_str()?.to_string(),
                arguments: block
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            })
        })
        .collect()
}

/// Extracts ids from arguments; filters out empty strings and non-strings so the module gets a clean list.
pub fn extract_ids(arguments: &Value) -> Vec<String> {
    arguments
        .get("ids")
        .and_then(|ids| ids.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(|id| id.trim())
                .filter(|id| !id.is_empty())
                .map(|id| id.to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Text fallback: not all models dutifully go through function calling
//
// Tested (deepseek's lightweight tier) it emits tool calls **as plain text**:
//
//   I'll read the item list.
//   <invoke name="read_items">
//   <parameter name="module">AI model space</parameter>
//   </invoke>
//
// There are three consequences, all subtle:
//   1. tool_calls is empty → the tool isn't executed;
//   2. this text is **shown verbatim to the user**, looking like the model is "performing a tool call";
//   3. the model then makes excuses for itself ("the system didn't give me a query interface").
//
// So we add a fallback layer here: recognize this form, restore it to a standard tool call, and remove it from the body.
// ⚠️ Only recognize tools we've declared, with limited alias mapping for parameter names — better to miss than to misrecognize.
// ---------------------------------------------------------------------------

/// Parameter alias → standard parameter name. What the model writes is "what it thinks it should be called", not our schema.
fn canonical_argument(tool: &str, raw_name: &str) -> Option<&'static str> {
    let name = raw_name.trim().to_ascii_lowercase();
    match tool {
        TOOL_READ_ITEMS => {
            // `path` / `folder` are aliases we've observed: the model wants to "read what's inside this folder",
            // so it writes the path as path (that's how the DSML case does it). Not recognizing it drops the parameter.
            if matches!(
                name.as_str(),
                "target_id" | "targetid" | "id" | "module" | "category" | "path" | "folder" | "dir"
            ) {
                Some("target_id")
            } else if matches!(name.as_str(), "offset" | "start") {
                Some("offset")
            } else if matches!(name.as_str(), "limit" | "count" | "size") {
                Some("limit")
            } else {
                None
            }
        }
        TOOL_READ_ITEM_DETAIL => {
            if matches!(
                name.as_str(),
                "item_id" | "itemid" | "id" | "path" | "item" | "file"
            ) {
                Some("item_id")
            } else {
                None
            }
        }
        TOOL_SELECT_ITEMS | TOOL_PROPOSE_DELETE => {
            if matches!(name.as_str(), "ids" | "id" | "items" | "targets") {
                Some("ids")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Normalizes the value of a `<parameter>` into a parameter value.
/// Lists like ids may be written by the model as multiple values separated by commas or newlines.
fn argument_value(canonical: &str, raw: &str) -> Value {
    let trimmed = raw.trim();
    if canonical == "ids" {
        let ids: Vec<Value> = trimmed
            .split(['\n', ',', '，'])
            .map(|item| item.trim().trim_matches('"').trim_matches('\'').to_string())
            .filter(|item| !item.is_empty())
            .map(Value::String)
            .collect();
        return serde_json::json!(ids);
    }
    if canonical == "offset" || canonical == "limit" {
        if let Ok(number) = trimmed.parse::<i64>() {
            return serde_json::json!(number);
        }
    }
    serde_json::json!(trimmed)
}

/// Normalizes DeepSeek's **DSML** markup into standard `<invoke>` / `<parameter>` shape.
///
/// In thinking mode with **no tools declared**, it won't (and has no tools to) go through function calling,
/// but expresses the same thing with its own markup:
///
/// ```text
/// <｜｜DSML｜｜ calls>
/// <｜｜DSML｜｜ invoke name="read_items">
/// <｜｜DSML｜｜ parameter name="path" string="true">C:\…\devin</｜｜DSML｜｜ parameter>
/// </｜｜DSML｜｜ invoke>
/// </｜｜DSML｜｜ calls>
/// ```
///
/// ⚠️ Not recognizing it has two consequences, both bad: **the tool isn't executed** and **the markup is shown verbatim to the user**.
/// After normalization we reuse the same parser — deliberately not writing a second parser.
fn normalize_dsml(content: &str) -> String {
    if !content.contains("DSML") {
        return content.to_string();
    }
    content
        // ⚠️ Order matters: the DSML prefix is followed by **one space** (`<｜｜DSML｜｜ invoke`),
        // so the spaced form must be replaced first — replace the unspaced form first and the remaining `< invoke` will never match.
        .replace("</｜｜DSML｜｜ ", "</")
        .replace("</｜｜DSML｜｜", "</")
        .replace("<｜｜DSML｜｜ ", "<")
        .replace("<｜｜DSML｜｜", "<")
        // The outer `<calls>` is just a wrapper (the parser only recognizes `<invoke>`); remove it so it doesn't linger in the body
        .replace("<calls>", "")
        .replace("</calls>", "")
}

/// `<tool_call>{"name": "...", "arguments": {...}}</tool_call>`
///
/// A third inline form: Hermes-style templates and some OpenAI-compatible gateways write calls as this **JSON text**.
/// Tested: deepseek-flash does this — if not recognized, the user sees a `<tool_call>` raw text in the chat history,
/// and that call **won't be executed** (ask "what is this driver" and the answer contains a directory-query call, which looks broken).
fn extract_json_style_tool_calls(content: &str) -> (Vec<AiToolCall>, String) {
    const OPEN: &str = "<tool_call>";
    const CLOSE: &str = "</tool_call>";

    let mut calls: Vec<AiToolCall> = Vec::new();
    let mut remaining = String::with_capacity(content.len());
    let mut cursor = 0usize;

    while let Some(open_rel) = content[cursor..].find(OPEN) {
        let open_at = cursor + open_rel;
        let Some(close_rel) = content[open_at..].find(CLOSE) else {
            // No closing tag = a half call, keep the body verbatim (consistent with how the invoke case is handled)
            break;
        };
        let body_end = open_at + close_rel;
        let body = &content[open_at + OPEN.len()..body_end];

        // Only recognize declared tools; don't execute if unrecognized
        let parsed = serde_json::from_str::<Value>(body.trim()).ok();
        let name = parsed
            .as_ref()
            .and_then(|value| value.get("name"))
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string();
        let declared = matches!(
            name.as_str(),
            TOOL_SELECT_ITEMS | TOOL_PROPOSE_DELETE | TOOL_READ_ITEMS | TOOL_READ_ITEM_DETAIL
        );

        if declared {
            let mut arguments = serde_json::Map::new();
            if let Some(raw) = parsed.as_ref().and_then(|value| value.get("arguments")) {
                // arguments may be an object or a (OpenAI-style) JSON string
                let object = match raw {
                    Value::String(text) => serde_json::from_str::<Value>(text).ok(),
                    other => Some(other.clone()),
                };
                if let Some(Value::Object(map)) = object {
                    for (key, value) in map {
                        // Use the same alias map as the invoke path, to avoid the two parsers disagreeing on parameter names
                        if let Some(canonical) = canonical_argument(&name, &key) {
                            arguments.insert(canonical.to_string(), value);
                        }
                    }
                }
            }
            calls.push(AiToolCall {
                call_id: String::new(),
                name,
                arguments: Value::Object(arguments),
            });
        }

        // Whether recognized or not, this `<tool_call>` must not stay in the body — it's not content meant for humans
        remaining.push_str(&content[cursor..open_at]);
        cursor = body_end + CLOSE.len();
    }
    remaining.push_str(&content[cursor..]);

    (calls, remaining)
}

/// Recognizes inline tool calls in the body, returning (list of calls, cleaned body).
///
/// Supports two forms: `<invoke name="x"><parameter name="y">…</parameter></invoke>` (including DSML variants),
/// and `<tool_call>{"name":…, "arguments":…}</tool_call>` (Hermes-style JSON text).
pub fn extract_inline_tool_calls(content: &str) -> (Vec<AiToolCall>, String) {
    // First normalize DeepSeek's DSML markup into the standard shape, so only one parser is needed afterwards
    let normalized = normalize_dsml(content);
    let content = normalized.as_str();

    let mut calls: Vec<AiToolCall> = Vec::new();
    let mut remaining = String::with_capacity(content.len());
    let mut cursor = 0usize;

    while let Some(open) = content[cursor..].find("<invoke") {
        let open_at = cursor + open;
        // If the opening tag doesn't even have '>': no complete call can follow, let the code outside the loop finish the remaining body
        let Some(tag_end) = content[open_at..]
            .find('>')
            .map(|relative| open_at + relative)
        else {
            break;
        };
        let name = attribute(&content[open_at..tag_end], "name");
        // The closing tag must also be present — missing name or missing closing tag both mean this isn't a call.
        // ⚠️ Here you **must restore the body before the opening tag verbatim**: earlier, this spot only advanced cursor without restoring the body,
        // so when the model emitted half a tag, the entire body before it was silently dropped (and that body must be displayed and stored).
        let (Some(name), Some(close_rel)) = (name, content[tag_end..].find("</invoke>")) else {
            remaining.push_str(&content[cursor..=tag_end]);
            cursor = tag_end + 1;
            continue;
        };
        let body_end = tag_end + close_rel;
        let body = &content[tag_end + 1..body_end];
        let block_end = body_end + "</invoke>".len();

        // Only recognize declared tools; discard the whole block for unrecognized ones (neither executed nor displayed)
        if matches!(
            name.as_str(),
            TOOL_SELECT_ITEMS | TOOL_PROPOSE_DELETE | TOOL_READ_ITEMS | TOOL_READ_ITEM_DETAIL
        ) {
            let mut arguments = serde_json::Map::new();
            let mut rest = body;
            while let Some(param_open) = rest.find("<parameter") {
                let Some(param_tag_end_rel) = rest[param_open..].find('>') else {
                    break;
                };
                let param_tag_end = param_open + param_tag_end_rel;
                let param_tag = &rest[param_open..param_tag_end];
                let Some(param_name) = attribute(param_tag, "name") else {
                    break;
                };
                let Some(param_close_rel) = rest[param_tag_end..].find("</parameter>") else {
                    break;
                };
                let param_value = &rest[param_tag_end + 1..param_tag_end + param_close_rel];
                rest = &rest[param_tag_end + param_close_rel + "</parameter>".len()..];
                if let Some(canonical) = canonical_argument(&name, &param_name) {
                    arguments.insert(
                        canonical.to_string(),
                        argument_value(canonical, param_value),
                    );
                }
            }
            calls.push(AiToolCall {
                call_id: String::new(),
                name,
                arguments: Value::Object(arguments),
            });
        }

        // Keep the part of the body already determined, skip the whole call block (content other than the leading hint line is handled separately)
        remaining.push_str(&content[cursor..open_at]);
        cursor = block_end;
    }
    remaining.push_str(&content[cursor..]);

    // Second pass: the `<tool_call>{json}</tool_call>` form (Hermes-style). Placed later because
    // its block structure is completely different from `<invoke>`; a separate scan for each is easier to read and test.
    let (json_calls, remaining) = extract_json_style_tool_calls(&remaining);
    calls.extend(json_calls);

    // Only collapse the extra blank lines left by "removing call blocks". **Do not filter out all blank lines** —
    // that would also delete the body's own blank lines, removing Markdown's paragraph separators.
    (calls, collapse_blank_lines(&remaining).trim().to_string())
}

/// Collapses consecutive blank lines into one: removing call blocks tends to leave stretches of whitespace.
fn collapse_blank_lines(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut consecutive_blank = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            consecutive_blank += 1;
            // Keep at most one blank line, discard the rest
            if consecutive_blank > 1 {
                continue;
            }
        } else {
            consecutive_blank = 0;
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Gets an attribute value from a tag, supporting double and single quotes.
///
/// ⚠️ Must look up by **attribute-name boundary**: a direct `find("name=")` would match `username=` or `data-name=`
/// and take their values as name.
fn attribute(tag: &str, key: &str) -> Option<String> {
    let needle = format!("{}=", key);
    // Try each candidate position from the front, accepting only the one "preceded by whitespace or at the very start of the tag"
    let mut search_from = 0usize;
    let start = loop {
        let found = tag[search_from..].find(&needle)? + search_from;
        let preceded_by_boundary = match tag[..found].chars().next_back() {
            None => true,
            Some(character) => character.is_whitespace(),
        };
        if preceded_by_boundary {
            break found + needle.len();
        }
        search_from = found + needle.len();
    };
    let rest = tag[start..].trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &rest[1..];
    let end = inner.find(quote)?;
    Some(inner[..end].trim().to_string())
}

// 测试放在文件末尾：clippy::items_after_test_module 要求测试模块之后不再有代码项，
// 同时让阅读顺序保持「实现 → 验证」。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_tool_calls_parse_arguments_from_string() {
        // arguments is JSON in string form — forget the second parse and you'll never get the ids
        let value = serde_json::json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_1",
                        "function": { "name": "select_items", "arguments": "{\"ids\":[\"RecycleBin\"]}" }
                    }]
                }
            }]
        });
        let calls = parse_tool_calls(AiProtocol::OpenAi, &value);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "select_items");
        assert_eq!(
            extract_ids(&calls[0].arguments),
            vec!["RecycleBin".to_string()]
        );
    }

    #[test]
    fn anthropic_tool_use_blocks_parse_input_as_object() {
        let value = serde_json::json!({
            "content": [
                { "type": "text", "text": "我来处理" },
                { "type": "tool_use", "id": "toolu_1", "name": "propose_delete", "input": { "ids": ["WindowsTemp"] } }
            ]
        });
        let calls = parse_tool_calls(AiProtocol::Anthropic, &value);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "propose_delete");
        assert_eq!(
            extract_ids(&calls[0].arguments),
            vec!["WindowsTemp".to_string()]
        );
    }

    #[test]
    fn plain_text_reply_yields_no_tool_calls() {
        let value = serde_json::json!({ "choices": [{ "message": { "content": "你好" } }] });
        assert!(parse_tool_calls(AiProtocol::OpenAi, &value).is_empty());
    }

    #[test]
    fn malformed_arguments_do_not_panic_and_yield_no_ids() {
        let value = serde_json::json!({
            "choices": [{ "message": { "tool_calls": [
                { "id": "call_1", "function": { "name": "select_items", "arguments": "not-json" } }
            ] } }]
        });
        let calls = parse_tool_calls(AiProtocol::OpenAi, &value);
        assert_eq!(calls.len(), 1);
        // Parse failure → empty object → no id → the caller won't use it to delete anything
        assert!(extract_ids(&calls[0].arguments).is_empty());
    }

    /// Tested: deepseek-flash writes calls as JSON text like `<tool_call>{"name":…}</tool_call>`.
    /// If not recognized, the user sees this raw text in chat and the call isn't executed at all (Evan reported this with a screenshot).
    #[test]
    fn json_style_tool_call_is_recognized_and_never_shown() {
        let raw = "这个驱动包可以安全清理。\n<tool_call>\n{\"name\": \"read_item_detail\", \"arguments\": {\"id\": \"oem7.inf\"}}\n</tool_call>";
        let (calls, cleaned) = extract_inline_tool_calls(raw);
        assert_eq!(calls.len(), 1, "应认出一次调用");
        assert_eq!(calls[0].name, "read_item_detail");
        assert_eq!(
            calls[0]
                .arguments
                .get("item_id")
                .and_then(|value| value.as_str()),
            Some("oem7.inf")
        );
        assert!(
            !cleaned.contains("tool_call"),
            "调用块不能留在正文里，实际: {}",
            cleaned
        );
        assert!(
            cleaned.contains("可以安全清理"),
            "正文必须保留，实际: {}",
            cleaned
        );
    }

    /// Unrecognized tool names: not executed, but that text must also disappear — the user shouldn't see tool syntax.
    #[test]
    fn unknown_json_tool_call_is_dropped_from_text() {
        let raw = "说明。\n<tool_call>{\"name\": \"rm_rf\", \"arguments\": {}}</tool_call>";
        let (calls, cleaned) = extract_inline_tool_calls(raw);
        assert!(calls.is_empty(), "未声明的工具不能执行");
        assert_eq!(cleaned, "说明。");
    }

    /// arguments written as a JSON string (OpenAI style) must also be accepted.
    #[test]
    fn json_style_tool_call_accepts_string_arguments() {
        let raw = "<tool_call>{\"name\": \"read_items\", \"arguments\": \"{\\\"path\\\": \\\"D:/models\\\"}\"}</tool_call>";
        let (calls, _) = extract_inline_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0]
                .arguments
                .get("target_id")
                .and_then(|value| value.as_str()),
            Some("D:/models")
        );
    }

    #[test]
    fn inline_xml_tool_call_is_recognized_and_stripped_from_text() {
        // Tested: deepseek's lightweight tier writes calls as text like this, which neither becomes tool_calls
        // nor is shown to the user verbatim. It must be recognized here and stripped from the body.
        let raw = "I'll read the item list.\n<invoke name=\"read_items\">\n<parameter name=\"module\">AI 模型空间</parameter>\n</invoke>";
        let (calls, cleaned) = extract_inline_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_items");
        // module is a parameter name the model made up; map it back to target_id
        assert_eq!(
            calls[0].arguments.get("target_id").and_then(|v| v.as_str()),
            Some("AI 模型空间")
        );
        // The XML must not remain in the body, otherwise the user would see "the model pretending to call a tool"
        assert!(!cleaned.contains("<invoke"));
        assert!(cleaned.contains("I'll read the item list"));
    }

    #[test]
    fn inline_list_parameter_splits_on_commas() {
        let raw = "<invoke name=\"propose_delete\"><parameter name=\"ids\">RecycleBin, WindowsTemp</parameter></invoke>";
        let (calls, _) = extract_inline_tool_calls(raw);
        assert_eq!(calls.len(), 1);
        let ids = calls[0]
            .arguments
            .get("ids")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn inline_unknown_tool_is_dropped_entirely() {
        // Undeclared tools: neither executed nor left in the body
        let raw = "看看这个 <invoke name=\"run_shell\"><parameter name=\"cmd\">rm -rf /</parameter></invoke>";
        let (calls, cleaned) = extract_inline_tool_calls(raw);
        assert!(calls.is_empty());
        assert!(!cleaned.contains("run_shell"));
    }

    #[test]
    fn plain_text_is_left_untouched() {
        let raw = "这是一个普通回答，里面没有任何工具调用。";
        let (calls, cleaned) = extract_inline_tool_calls(raw);
        assert!(calls.is_empty());
        assert_eq!(cleaned, raw);
    }

    #[test]
    fn unknown_tools_are_ignored_in_declarations() {
        let names = vec!["select_items".to_string(), "run_shell".to_string()];
        assert_eq!(openai_tools(&names).len(), 1);
        assert_eq!(anthropic_tools(&names).len(), 1);
    }

    #[test]
    fn declarations_use_each_protocol_shape() {
        let names = vec![TOOL_SELECT_ITEMS.to_string()];
        let openai = openai_tools(&names);
        assert!(
            openai[0].get("function").is_some(),
            "OpenAI 需要 function 外层"
        );
        let anthropic = anthropic_tools(&names);
        assert!(
            anthropic[0].get("input_schema").is_some(),
            "Anthropic 用 input_schema"
        );
        assert!(anthropic[0].get("function").is_none());
    }

    // ---- Edge cases of the hand-written parser: these have all caused problems before, or are easy to trip over when changing ----

    #[test]
    fn attributes_are_matched_on_whole_names_only() {
        // `username=` also contains "name=" — must match on attribute-name boundaries,
        // otherwise "admin" is taken as name and the real name can't be retrieved.
        let tag = "<invoke username=\"admin\" name=\"read_items\"";
        assert_eq!(attribute(tag, "name").as_deref(), Some("read_items"));
        assert_eq!(
            attribute("<invoke name=\"read_items\"", "name").as_deref(),
            Some("read_items")
        );
    }

    #[test]
    fn a_well_formed_inline_call_is_extracted_and_removed_from_the_body() {
        let content = "先说明一下。\n<invoke name=\"read_items\"><parameter name=\"offset\">0</parameter></invoke>\n后面还有话。";
        let (calls, cleaned) = extract_inline_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_items");
        assert_eq!(
            calls[0]
                .arguments
                .get("offset")
                .and_then(|value| value.as_i64()),
            Some(0)
        );
        assert!(cleaned.contains("先说明一下。"), "实际: {}", cleaned);
        assert!(cleaned.contains("后面还有话。"), "实际: {}", cleaned);
        assert!(
            !cleaned.contains("<invoke"),
            "调用块应被剔除，实际: {}",
            cleaned
        );
    }

    #[test]
    fn an_unclosed_invoke_tag_keeps_the_text_before_it() {
        // When the model emits half a tag, the body before the opening tag **must not be lost** —
        // earlier, that spot only advanced cursor without restoring the body, so the whole segment was swallowed (and the body must be displayed and stored).
        let content = "这一段必须留下来。\n<invoke name=\"read_items\">";
        let (calls, cleaned) = extract_inline_tool_calls(content);
        assert!(calls.is_empty(), "残缺的调用不该被执行");
        assert!(
            cleaned.contains("这一段必须留下来。"),
            "正文被吞了，实际: {}",
            cleaned
        );
    }

    #[test]
    fn paragraph_breaks_in_the_body_are_preserved() {
        // After removing the call block, only collapse extra blank lines; the body's own blank lines are Markdown paragraph separators and must be kept.
        let content = "第一段\n\n第二段\n\n<invoke name=\"read_items\"></invoke>";
        let (calls, cleaned) = extract_inline_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert!(
            cleaned.contains("第一段\n\n第二段"),
            "段落分隔被压掉了，实际: {:?}",
            cleaned
        );
    }

    #[test]
    fn an_unclosed_invoke_tag_does_not_swallow_the_tail() {
        // A fragment without even '>': the whole segment must be kept verbatim, don't drop content just because "the opening tag's end wasn't found"
        let content = "前面的正文。\n<invoke name=\"read_items\"";
        let (calls, cleaned) = extract_inline_tool_calls(content);
        assert!(calls.is_empty());
        assert!(cleaned.contains("前面的正文。"), "实际: {}", cleaned);
        assert!(
            cleaned.contains("read_items"),
            "残片本身也该留在正文里: {}",
            cleaned
        );
    }

    #[test]
    fn dsml_markup_is_recognized_and_removed_from_the_body() {
        // Tested (deepseek-flash in thinking mode + no tools declared) it uses its own DSML markup to express tool calls.
        // Forwarded verbatim like this — it must be recognized, and **must not be shown to the user**.
        let content = "我来查一下。\n\
             <｜｜DSML｜｜ calls>\n\
             <｜｜DSML｜｜ invoke name=\"read_items\">\n\
             <｜｜DSML｜｜ parameter name=\"path\" string=\"true\">C:\\Users\\chunyu\\AppData\\Roaming\\devin</｜｜DSML｜｜ parameter>\n\
             </｜｜DSML｜｜ invoke>\n\
             </｜｜DSML｜｜ calls>";
        let (calls, cleaned) = extract_inline_tool_calls(content);
        assert_eq!(
            calls.len(),
            1,
            "DSML 标记应当被认成一次调用，实际: {}",
            cleaned
        );
        assert_eq!(calls[0].name, "read_items");
        // `path` is an alias we've observed in the wild; not recognizing it loses the parameter
        assert_eq!(
            calls[0]
                .arguments
                .get("target_id")
                .and_then(|value| value.as_str()),
            Some("C:\\Users\\chunyu\\AppData\\Roaming\\devin")
        );
        assert!(!cleaned.contains("DSML"), "标记不该留在正文里: {}", cleaned);
        assert!(
            !cleaned.contains("invoke"),
            "标记不该留在正文里: {}",
            cleaned
        );
        assert!(cleaned.contains("我来查一下。"), "正文要留下: {}", cleaned);
    }

    #[test]
    fn dsml_wrapper_alone_leaves_no_leftovers() {
        // Only the outer wrapper with no real call: should produce no call, and shouldn't leave `<calls>` in the body
        let content = "说明文字\n<｜｜DSML｜｜ calls>\n</｜｜DSML｜｜ calls>";
        let (calls, cleaned) = extract_inline_tool_calls(content);
        assert!(calls.is_empty());
        assert!(
            !cleaned.contains("calls"),
            "包装标签不该留在正文里: {}",
            cleaned
        );
        assert!(cleaned.contains("说明文字"));
    }
}
