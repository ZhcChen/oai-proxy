use oai_proxy::proxy::{
    semantic_token::{EndpointKind, is_semantic_event},
    sse::SseParser,
};

#[test]
fn chat_completion_role_delta_is_not_semantic() {
    let event = parse_one(b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n");
    assert!(!is_semantic_event(EndpointKind::ChatCompletions, &event));
}

#[test]
fn chat_completion_content_delta_is_semantic() {
    let event = parse_one(b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n");
    assert!(is_semantic_event(EndpointKind::ChatCompletions, &event));
}

#[test]
fn responses_created_event_is_not_semantic() {
    let event = parse_one(
        b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\"}}\n\n",
    );
    assert!(!is_semantic_event(EndpointKind::Responses, &event));
}

#[test]
fn responses_in_progress_and_output_item_events_are_not_semantic() {
    for raw in [
        b"event: response.in_progress\ndata: {\"type\":\"response.in_progress\"}\n\n".as_slice(),
        b"event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\"}\n\n"
            .as_slice(),
        b"event: response.content_part.added\ndata: {\"type\":\"response.content_part.added\"}\n\n"
            .as_slice(),
    ] {
        let event = parse_one(raw);
        assert!(!is_semantic_event(EndpointKind::Responses, &event));
    }
}

#[test]
fn responses_output_text_delta_is_semantic() {
    let event = parse_one(
        b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
    );
    assert!(is_semantic_event(EndpointKind::Responses, &event));
}

#[test]
fn malformed_json_empty_delta_and_done_are_not_semantic() {
    for raw in [
        b"data: not-json\n\n".as_slice(),
        b"data: {\"choices\":[{\"delta\":{\"content\":\"\"}}]}\n\n".as_slice(),
        b"data: [DONE]\n\n".as_slice(),
        b": ping\n\n".as_slice(),
    ] {
        let event = parse_one(raw);
        assert!(!is_semantic_event(EndpointKind::Unknown, &event));
    }
}

#[test]
fn chat_reasoning_refusal_and_tool_arguments_are_semantic() {
    for raw in [
        b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}\n\n".as_slice(),
        b"data: {\"choices\":[{\"delta\":{\"refusal\":\"no\"}}]}\n\n".as_slice(),
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n"
            .as_slice(),
    ] {
        let event = parse_one(raw);
        assert!(is_semantic_event(EndpointKind::ChatCompletions, &event));
    }
}

fn parse_one(raw: &[u8]) -> oai_proxy::proxy::sse::SseEvent {
    let mut parser = SseParser::new();
    let events = parser.push(raw);
    assert_eq!(events.len(), 1);
    events.into_iter().next().unwrap()
}
