//! Per-model transport selection for OpenCode Zen (and Zen-compatible gateways).
//!
//! Zen serves three wire dialects behind one base URL (see the model table in
//! opencode's `packages/web/src/content/docs/zen.mdx`):
//!
//! * `/responses` (OpenAI Responses API): `muse-spark-*`, `gpt-5*`/`*codex*`,
//!   `grok-4.*`, `grok-build-*`
//! * `/messages` (Anthropic-style): `claude-*`, `qwen3.*` — not implemented
//!   here; those models keep the previous chat-completions behavior.
//! * `/chat/completions` (default): everything else.
//!
//! Sending a Responses-only model (e.g. `muse-spark-1.3-contributor-free`) to
//! `/chat/completions` fails server-side with `500 Internal server error`, and
//! sending a chat-shaped body to `/responses` fails with `400
//! invalid_request_error`. So the transport must be chosen per model, the way
//! opencode itself does via each model's `api.npm`/`api.endpoint` metadata.

use jcode_message_types::{ContentBlock, Message, Role, ToolDefinition};
use serde_json::Value;

/// Wire dialect to speak for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZenTransport {
    Chat,
    Responses,
}

/// Profile ids served by OpenCode Zen.
fn is_zen_profile(profile_id: Option<&str>) -> bool {
    matches!(
        profile_id.map(str::to_ascii_lowercase).as_deref(),
        Some("opencode") | Some("opencode-go") | Some("opencode-zen") | Some("zen")
    )
}

/// True when `api_base` points at an OpenCode Zen gateway.
fn is_zen_base(api_base: &str) -> bool {
    api_base.to_ascii_lowercase().contains("opencode.ai/zen")
}

/// True when a Zen model id is served through the Responses API.
///
/// Mirrors the endpoint column of opencode's Zen model table:
/// `muse-spark-*`, GPT-5 family + Codex variants, Grok 4 + Grok Build.
pub fn zen_model_uses_responses_api(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    m.contains("muse-spark")
        || m.starts_with("gpt-5")
        || m.contains("codex")
        || m.starts_with("grok-")
}

/// Select the wire dialect for one request.
///
/// Only Zen profiles/bases are affected; every other provider always stays on
/// chat-completions regardless of model name.
pub fn zen_transport_for_model(
    profile_id: Option<&str>,
    api_base: &str,
    model: &str,
) -> ZenTransport {
    if (is_zen_profile(profile_id) || is_zen_base(api_base)) && zen_model_uses_responses_api(model)
    {
        ZenTransport::Responses
    } else {
        ZenTransport::Chat
    }
}

/// Map a jcode reasoning-effort setting onto the Responses API
/// `reasoning.effort` vocabulary (`none`/`minimal`/`low`/`medium`/`high`).
///
/// `None` (or `"none"`) means "omit the field" (server default). Unknown
/// values pass through untouched so future efforts keep working and the
/// server — not the client — validates them.
pub fn responses_reasoning_effort(effort: Option<&str>) -> Option<String> {
    let effort = effort?.trim().to_ascii_lowercase();
    if effort.is_empty() || effort == "none" {
        return None;
    }
    if effort == "max" || jcode_base::prompt::is_swarm_effort(&effort) {
        return Some("high".to_string());
    }
    Some(effort)
}

/// Convert a conversation into OpenAI Responses `instructions` + `input`.
///
/// * system prompt -> `instructions`
/// * user/assistant text -> message items with `input_text` parts
/// * assistant `ToolUse` -> standalone `function_call` items (with the exact
///   ids the matching `ToolResult` blocks reference)
/// * user `ToolResult` -> `function_call_output` items
/// * assistant `OpenAIReasoning` -> native `reasoning` items for replay
/// * history-only blocks (`Reasoning`, `ReasoningTrace`, `AnthropicThinking`,
///   `OpenAICompaction`) are never replayed.
pub fn build_responses_input(messages: &[Message], system: &str) -> (Option<String>, Vec<Value>) {
    let instructions = if system.trim().is_empty() {
        None
    } else {
        Some(system.to_string())
    };
    let mut input = Vec::new();

    for msg in messages {
        match msg.role {
            Role::User => {
                let mut parts = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            parts.push(serde_json::json!({
                                "type": "input_text",
                                "text": text,
                            }));
                        }
                        ContentBlock::Image { media_type, data } => {
                            parts.push(serde_json::json!({
                                "type": "input_image",
                                "image_url": format!("data:{};base64,{}", media_type, data),
                            }));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            input.push(serde_json::json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": content,
                            }));
                        }
                        // Never replayed to any provider.
                        ContentBlock::Reasoning { .. }
                        | ContentBlock::ReasoningTrace { .. }
                        | ContentBlock::AnthropicThinking { .. }
                        | ContentBlock::OpenAIReasoning { .. }
                        | ContentBlock::OpenAICompaction { .. }
                        | ContentBlock::ToolUse { .. } => {}
                    }
                }
                if !parts.is_empty() {
                    input.push(serde_json::json!({
                        "role": "user",
                        "content": parts,
                    }));
                }
            }
            Role::Assistant => {
                let mut parts = Vec::new();
                // Function calls and reasoning items must follow the assistant
                // message that produced them, so buffer them while scanning
                // blocks and flush after the message item.
                let mut trailing = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            parts.push(serde_json::json!({
                                "type": "output_text",
                                "text": text,
                            }));
                        }
                        ContentBlock::ToolUse {
                            id,
                            name,
                            input: arguments,
                            ..
                        } => {
                            let arguments = match arguments {
                                Value::String(s) => s.clone(),
                                v => serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
                            };
                            trailing.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": arguments,
                            }));
                        }
                        ContentBlock::OpenAIReasoning {
                            id,
                            summary,
                            encrypted_content,
                            status,
                        } => {
                            let mut item = serde_json::json!({
                                "type": "reasoning",
                                "id": id,
                                "summary": summary,
                            });
                            if let Some(encrypted) = encrypted_content {
                                item["encrypted_content"] = Value::String(encrypted.clone());
                            }
                            if let Some(status) = status {
                                item["status"] = Value::String(status.clone());
                            }
                            trailing.push(item);
                        }
                        // Never replayed to any provider.
                        ContentBlock::Reasoning { .. }
                        | ContentBlock::ReasoningTrace { .. }
                        | ContentBlock::AnthropicThinking { .. }
                        | ContentBlock::OpenAICompaction { .. }
                        | ContentBlock::ToolResult { .. }
                        | ContentBlock::Image { .. } => {}
                    }
                }
                if !parts.is_empty() {
                    input.push(serde_json::json!({
                        "role": "assistant",
                        "content": parts,
                    }));
                }
                input.extend(trailing);
            }
        }
    }

    (instructions, input)
}

/// Build Responses API `tools` (`function` items) from jcode tool definitions.
pub fn build_responses_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "name": t.name,
                "description": t.description,
                "parameters": jcode_provider_openrouter::request::sanitize_tool_parameters_schema(
                    &t.input_schema,
                ),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_message_types::ContentBlock;

    #[test]
    fn non_zen_providers_always_use_chat() {
        for model in ["muse-spark-1.3-contributor-free", "gpt-5-nano", "grok-4.5"] {
            assert_eq!(
                zen_transport_for_model(Some("openrouter"), "https://openrouter.ai/api/v1", model),
                ZenTransport::Chat,
            );
            assert_eq!(
                zen_transport_for_model(None, "https://example.com/v1", model),
                ZenTransport::Chat,
            );
        }
    }

    #[test]
    fn zen_responses_families_use_responses_api() {
        for model in [
            "muse-spark-1.3-contributor-free",
            "muse-spark-1.2",
            "muse-spark-1.2-contributor-free",
            "gpt-5",
            "gpt-5-nano",
            "gpt-5.4",
            "gpt-5.1-codex-max",
            "gpt-5.3-codex-spark",
            "grok-4.5",
            "grok-4.6",
            "grok-build-0.1",
        ] {
            assert_eq!(
                zen_transport_for_model(Some("opencode"), "https://opencode.ai/zen/v1", model),
                ZenTransport::Responses,
                "model {model}"
            );
        }
    }

    #[test]
    fn zen_chat_models_stay_on_chat() {
        for model in [
            "minimax-m2.7",
            "kimi-k3",
            "kimi-k2.5",
            "deepseek-v4-flash",
            "mimo-v2.5-free",
            "big-pickle",
            "glm-5.2",
            "claude-sonnet-4-5",
            "qwen3.6-plus",
        ] {
            assert_eq!(
                zen_transport_for_model(Some("opencode"), "https://opencode.ai/zen/v1", model),
                ZenTransport::Chat,
                "model {model}"
            );
        }
    }

    #[test]
    fn zen_go_profile_matches_by_base_too() {
        assert_eq!(
            zen_transport_for_model(
                Some("opencode-go"),
                "https://opencode.ai/zen/go/v1",
                "muse-spark-1.3-contributor-free"
            ),
            ZenTransport::Responses,
        );
    }

    #[test]
    fn effort_mapping_matches_responses_vocabulary() {
        assert_eq!(responses_reasoning_effort(None), None);
        assert_eq!(responses_reasoning_effort(Some("none")), None);
        assert_eq!(
            responses_reasoning_effort(Some("low")),
            Some("low".to_string())
        );
        assert_eq!(
            responses_reasoning_effort(Some("medium")),
            Some("medium".to_string())
        );
        assert_eq!(
            responses_reasoning_effort(Some("high")),
            Some("high".to_string())
        );
        assert_eq!(
            responses_reasoning_effort(Some("max")),
            Some("high".to_string())
        );
    }

    fn text_message(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    #[test]
    fn input_builder_round_trips_text_and_tool_history() {
        let messages = vec![
            text_message(Role::User, "list files"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "on it".to_string(),
                        cache_control: None,
                    },
                    ContentBlock::ToolUse {
                        id: "call_1".to_string(),
                        name: "bash".to_string(),
                        input: serde_json::json!({"cmd": "ls"}),
                        thought_signature: None,
                    },
                ],
                timestamp: None,
                tool_duration_ms: None,
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "a.txt".to_string(),
                    is_error: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            },
        ];
        let (instructions, input) = build_responses_input(&messages, "sys");
        assert_eq!(instructions.as_deref(), Some("sys"));
        let kinds: Vec<&str> = input
            .iter()
            .map(|item| {
                item.get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("message")
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "message",
                "message",
                "function_call",
                "function_call_output"
            ]
        );
        let call = &input[2];
        assert_eq!(call["call_id"], "call_1");
        assert_eq!(call["name"], "bash");
        assert_eq!(call["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(input[3]["call_id"], "call_1");
        assert_eq!(input[3]["output"], "a.txt");
    }

    #[test]
    fn input_builder_skips_history_only_blocks() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Reasoning {
                    text: "hidden".to_string(),
                },
                ContentBlock::ReasoningTrace {
                    text: "trace".to_string(),
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        }];
        let (instructions, input) = build_responses_input(&messages, "");
        assert_eq!(instructions, None);
        assert!(input.is_empty());
    }

    #[test]
    fn input_builder_replays_native_reasoning_items() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::OpenAIReasoning {
                id: "rs_1".to_string(),
                summary: vec!["s".to_string()],
                encrypted_content: Some("enc".to_string()),
                status: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }];
        let (_, input) = build_responses_input(&messages, "");
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["id"], "rs_1");
        assert_eq!(input[0]["encrypted_content"], "enc");
    }
}
