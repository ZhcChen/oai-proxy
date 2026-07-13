use serde_json::Value;

use super::sse::SseEvent;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointKind {
    ChatCompletions,
    Responses,
    Unknown,
}

impl EndpointKind {
    pub fn from_path(path: &str) -> Self {
        if path.ends_with("/v1/chat/completions") {
            Self::ChatCompletions
        } else if path.ends_with("/v1/responses") {
            Self::Responses
        } else {
            Self::Unknown
        }
    }
}

pub fn is_semantic_event(endpoint: EndpointKind, event: &SseEvent) -> bool {
    let data = event.data.trim();
    if data.is_empty() || data == "[DONE]" {
        return false;
    }

    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return false;
    };

    match endpoint {
        EndpointKind::ChatCompletions => is_chat_semantic(&value),
        EndpointKind::Responses => is_responses_semantic(event.event.as_deref(), &value),
        EndpointKind::Unknown => {
            is_chat_semantic(&value) || is_responses_semantic(event.event.as_deref(), &value)
        }
    }
}

fn is_chat_semantic(value: &Value) -> bool {
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
        return false;
    };

    choices.iter().any(|choice| {
        let Some(delta) = choice.get("delta") else {
            return false;
        };

        non_empty_string(delta.get("content"))
            || non_empty_string(delta.get("reasoning_content"))
            || non_empty_string(delta.get("refusal"))
            || tool_call_arguments(delta)
    })
}

fn is_responses_semantic(event_name: Option<&str>, value: &Value) -> bool {
    let event_type = event_name
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or_default();

    if matches!(
        event_type,
        "response.created"
            | "response.in_progress"
            | "response.completed"
            | "response.queued"
            | "response.output_item.added"
            | "response.content_part.added"
    ) {
        return false;
    }

    if event_type.ends_with(".delta") {
        return non_empty_string(value.get("delta"))
            || non_empty_string(value.get("text"))
            || non_empty_string(value.get("arguments"));
    }

    non_empty_string(value.get("delta"))
        || non_empty_string(value.get("output_text"))
        || non_empty_string(value.get("text"))
}

fn tool_call_arguments(delta: &Value) -> bool {
    delta
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls.iter().any(|call| {
                call.get("function")
                    .and_then(|function| function.get("arguments"))
                    .is_some_and(|value| non_empty_string(Some(value)))
            })
        })
        .unwrap_or(false)
}

fn non_empty_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .map(|text| !text.is_empty())
        .unwrap_or(false)
}
