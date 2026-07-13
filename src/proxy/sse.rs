use bytes::{Bytes, BytesMut};

#[derive(Clone, Debug)]
pub struct SseEvent {
    pub raw: Bytes,
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buffer: BytesMut,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();

        while let Some((index, delimiter_len)) = find_event_delimiter(&self.buffer) {
            let raw = self.buffer.split_to(index + delimiter_len).freeze();
            events.push(parse_event(raw));
        }

        events
    }

    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    pub fn take_buffer(&mut self) -> Bytes {
        self.buffer.split().freeze()
    }
}

fn find_event_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = find_subsequence(buffer, b"\n\n").map(|index| (index, 2));
    let crlf = find_subsequence(buffer, b"\r\n\r\n").map(|index| (index, 4));

    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_event(raw: Bytes) -> SseEvent {
    let text = String::from_utf8_lossy(&raw);
    let mut event = None;
    let mut data_lines = Vec::new();

    for line in text.replace("\r\n", "\n").lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim_start().to_string());
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }

    SseEvent {
        raw,
        event,
        data: data_lines.join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_events_and_keeps_partial_buffer() {
        let mut parser = SseParser::new();
        let events = parser.push(
            b"event: response.created\ndata: {\"type\":\"response.created\"}\n\ndata: {\"x\":1}\n\npartial",
        );

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_deref(), Some("response.created"));
        assert_eq!(events[1].data, "{\"x\":1}");
        assert_eq!(parser.buffered_len(), "partial".len());
    }
}
