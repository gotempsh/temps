// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Incremental SSE usage extraction. Output strings and arrays are consumed,
//! never buffered. Each stream retains at most 128 JSON frames and one 128-byte
//! token, independently of the size of a terminal Responses event.

#[derive(Clone, Copy)]
pub(super) enum UsageKind {
    ChatCompletions,
    Responses,
}

#[derive(Clone, Copy, PartialEq)]
enum Scope {
    Root,
    Response,
    Usage,
    Other,
}

#[derive(Clone, Copy, PartialEq)]
enum Key {
    Type,
    Response,
    Usage,
    Input,
    Output,
    Other,
}

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    KeyOrEnd,
    Key,
    Colon,
    ValueOrEnd,
    Value,
    CommaOrEnd,
}

struct Frame {
    object: bool,
    scope: Scope,
    expect: Expect,
    key: Key,
}

#[derive(Clone, Copy, PartialEq)]
enum Lex {
    Idle,
    String,
    Scalar,
}

#[derive(Clone, Copy)]
enum Number {
    Minus,
    Zero,
    Integer,
    Dot,
    Fraction,
    Exponent,
    Sign,
    ExponentDigits,
}

impl Number {
    fn next(self, byte: u8) -> Option<Self> {
        match (self, byte) {
            (Self::Minus, b'0') => Some(Self::Zero),
            (Self::Minus, b'1'..=b'9') | (Self::Integer, b'0'..=b'9') => Some(Self::Integer),
            (Self::Zero | Self::Integer, b'.') => Some(Self::Dot),
            (Self::Dot | Self::Fraction, b'0'..=b'9') => Some(Self::Fraction),
            (Self::Zero | Self::Integer | Self::Fraction, b'e' | b'E') => Some(Self::Exponent),
            (Self::Exponent, b'+' | b'-') => Some(Self::Sign),
            (Self::Exponent | Self::Sign | Self::ExponentDigits, b'0'..=b'9') => {
                Some(Self::ExponentDigits)
            }
            _ => None,
        }
    }

    fn complete(self) -> bool {
        matches!(
            self,
            Self::Zero | Self::Integer | Self::Fraction | Self::ExponentDigits
        )
    }
}

struct JsonUsage {
    kind: UsageKind,
    frames: Vec<Frame>,
    lex: Lex,
    number: Option<Number>,
    token: [u8; 128],
    len: usize,
    overflow: bool,
    escaped: bool,
    unicode_left: u8,
    invalid: bool,
    started: bool,
    terminal: bool,
    input: i64,
    output: i64,
}

impl JsonUsage {
    fn new(kind: UsageKind) -> Self {
        Self {
            kind,
            frames: Vec::new(),
            lex: Lex::Idle,
            number: None,
            token: [0; 128],
            len: 0,
            overflow: false,
            escaped: false,
            unicode_left: 0,
            invalid: false,
            started: false,
            terminal: false,
            input: 0,
            output: 0,
        }
    }

    fn token_byte(&mut self, byte: u8) {
        if self.len < self.token.len() {
            self.token[self.len] = byte;
            self.len += 1;
        } else {
            self.overflow = true;
        }
    }

    fn begin_token(&mut self, lex: Lex, byte: u8) {
        self.lex = lex;
        self.len = 0;
        self.overflow = false;
        self.number = match byte {
            b'-' => Some(Number::Minus),
            b'0' => Some(Number::Zero),
            b'1'..=b'9' => Some(Number::Integer),
            _ => None,
        };
        self.token_byte(byte);
    }

    fn finish_token(&mut self, string: bool) {
        self.lex = Lex::Idle;
        // Long strings can only be irrelevant keys/content: all keys and event
        // types we recognize fit in this buffer, including JSON \u escapes.
        let value = if let Some(number) = self.number {
            if !number.complete() {
                self.invalid = true;
                return;
            }
            // JSON numbers may exceed i64 (or even f64). They cannot supply
            // token counts, but must not prevent accounting for other fields.
            if self.overflow {
                None
            } else {
                serde_json::from_slice::<serde_json::Value>(&self.token[..self.len]).ok()
            }
        } else if self.overflow && string {
            None
        } else {
            match serde_json::from_slice::<serde_json::Value>(&self.token[..self.len]) {
                Ok(value) => Some(value),
                Err(_) => {
                    self.invalid = true;
                    return;
                }
            }
        };
        let Some(frame) = self.frames.last_mut() else {
            self.invalid = true;
            return;
        };
        if frame.object && matches!(frame.expect, Expect::KeyOrEnd | Expect::Key) {
            if !string {
                self.invalid = true;
                return;
            }
            frame.key = match value.as_ref().and_then(|v| v.as_str()) {
                Some("type") => Key::Type,
                Some("response") => Key::Response,
                Some("usage") => Key::Usage,
                Some("input_tokens") if matches!(self.kind, UsageKind::Responses) => Key::Input,
                Some("output_tokens") if matches!(self.kind, UsageKind::Responses) => Key::Output,
                Some("prompt_tokens") if matches!(self.kind, UsageKind::ChatCompletions) => {
                    Key::Input
                }
                Some("completion_tokens") if matches!(self.kind, UsageKind::ChatCompletions) => {
                    Key::Output
                }
                _ => Key::Other,
            };
            frame.expect = Expect::Colon;
            return;
        }
        if !matches!(frame.expect, Expect::Value | Expect::ValueOrEnd) {
            self.invalid = true;
            return;
        }
        let scope = frame.scope;
        let key = frame.key;
        frame.expect = Expect::CommaOrEnd;
        self.reset_field(scope, key);
        match (scope, key) {
            (Scope::Root, Key::Type) => {
                self.terminal = matches!(
                    value.as_ref().and_then(|v| v.as_str()),
                    Some("response.completed" | "response.incomplete" | "response.failed")
                );
            }
            (Scope::Usage, Key::Input) => self.input = value.and_then(|v| v.as_i64()).unwrap_or(0),
            (Scope::Usage, Key::Output) => {
                self.output = value.and_then(|v| v.as_i64()).unwrap_or(0)
            }
            _ => {}
        }
    }

    fn usage_parent(&self, scope: Scope) -> bool {
        matches!(
            (self.kind, scope),
            (UsageKind::Responses, Scope::Response) | (UsageKind::ChatCompletions, Scope::Root)
        )
    }

    // Match serde_json's last-key-wins behavior even when a repeated field is
    // null or another container. Old token counts must not survive replacement.
    fn reset_field(&mut self, scope: Scope, key: Key) {
        if (scope == Scope::Root
            && key == Key::Response
            && matches!(self.kind, UsageKind::Responses))
            || (self.usage_parent(scope) && key == Key::Usage)
        {
            self.input = 0;
            self.output = 0;
        }
        if scope == Scope::Root && key == Key::Type {
            self.terminal = false;
        }
        if scope == Scope::Usage && key == Key::Input {
            self.input = 0;
        }
        if scope == Scope::Usage && key == Key::Output {
            self.output = 0;
        }
    }

    fn open(&mut self, object: bool) {
        let scope = if let Some(frame) = self.frames.last_mut() {
            if !matches!(frame.expect, Expect::Value | Expect::ValueOrEnd) {
                self.invalid = true;
                return;
            }
            let parent = frame.scope;
            let key = frame.key;
            frame.expect = Expect::CommaOrEnd;
            self.reset_field(parent, key);
            if object
                && parent == Scope::Root
                && key == Key::Response
                && matches!(self.kind, UsageKind::Responses)
            {
                Scope::Response
            } else if object && self.usage_parent(parent) && key == Key::Usage {
                Scope::Usage
            } else {
                Scope::Other
            }
        } else if !self.started && object {
            self.started = true;
            Scope::Root
        } else {
            self.invalid = true;
            return;
        };
        if self.frames.len() == 128 {
            self.invalid = true;
            return;
        }
        self.frames.push(Frame {
            object,
            scope,
            expect: if object {
                Expect::KeyOrEnd
            } else {
                Expect::ValueOrEnd
            },
            key: Key::Other,
        });
    }

    fn push(&mut self, byte: u8) {
        if self.invalid {
            return;
        }
        if self.lex == Lex::String {
            self.token_byte(byte);
            if self.unicode_left > 0 {
                if !byte.is_ascii_hexdigit() {
                    self.invalid = true;
                }
                self.unicode_left -= 1;
            } else if self.escaped {
                self.escaped = false;
                if byte == b'u' {
                    self.unicode_left = 4;
                } else if !matches!(byte, b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') {
                    self.invalid = true;
                }
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == b'"' {
                self.finish_token(true);
            } else if byte < 0x20 {
                self.invalid = true;
            }
            return;
        }
        if self.lex == Lex::Scalar {
            if !matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']') {
                if let Some(number) = self.number {
                    if let Some(next) = number.next(byte) {
                        self.number = Some(next);
                    } else {
                        self.invalid = true;
                    }
                }
                self.token_byte(byte);
                if self.overflow && self.number.is_none() {
                    self.invalid = true;
                }
                return;
            }
            self.finish_token(false);
            if self.invalid {
                return;
            }
        }
        match byte {
            b' ' | b'\t' | b'\r' | b'\n' => {}
            b'"' => self.begin_token(Lex::String, byte),
            b'{' => self.open(true),
            b'[' => self.open(false),
            b'}' | b']' => {
                let valid = self.frames.last().is_some_and(|frame| {
                    frame.object == (byte == b'}')
                        && (frame.expect == Expect::CommaOrEnd
                            || (frame.object && frame.expect == Expect::KeyOrEnd)
                            || (!frame.object && frame.expect == Expect::ValueOrEnd))
                });
                if valid {
                    self.frames.pop();
                } else {
                    self.invalid = true;
                }
            }
            b':' | b',' => {
                if let Some(frame) = self.frames.last_mut() {
                    if byte == b':' && frame.expect == Expect::Colon {
                        frame.expect = Expect::Value;
                    } else if byte == b',' && frame.expect == Expect::CommaOrEnd {
                        frame.expect = if frame.object {
                            Expect::Key
                        } else {
                            Expect::Value
                        };
                        frame.key = Key::Other;
                    } else {
                        self.invalid = true;
                    }
                } else {
                    self.invalid = true;
                }
            }
            b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => self.begin_token(Lex::Scalar, byte),
            _ => self.invalid = true,
        }
    }

    fn usage(&self) -> Option<(i64, i64)> {
        (!self.invalid
            && self.started
            && self.frames.is_empty()
            && self.lex == Lex::Idle
            && (matches!(self.kind, UsageKind::ChatCompletions) || self.terminal)
            && (self.input > 0 || self.output > 0))
            .then_some((self.input, self.output))
    }
}

pub(super) struct SseUsage {
    json: JsonUsage,
    prefix: usize,
    ignored: bool,
}

impl SseUsage {
    pub(super) fn new(kind: UsageKind) -> Self {
        Self {
            json: JsonUsage::new(kind),
            prefix: 0,
            ignored: false,
        }
    }

    /// Call `record` for complete data lines without changing forwarded bytes.
    pub(super) fn push(&mut self, bytes: &[u8], mut record: impl FnMut(i64, i64)) {
        for &byte in bytes {
            if byte == b'\n' {
                if let Some((input, output)) = self.finish_line() {
                    record(input, output);
                }
            } else if !self.ignored {
                if self.prefix < 5 {
                    if byte == b"data:"[self.prefix] {
                        self.prefix += 1;
                    } else {
                        self.ignored = true;
                    }
                } else {
                    self.json.push(byte);
                }
            }
        }
    }

    pub(super) fn finish_line(&mut self) -> Option<(i64, i64)> {
        let usage = if self.prefix == 5 && !self.ignored {
            self.json.usage()
        } else {
            None
        };
        let kind = self.json.kind;
        // Reuse the bounded stack allocation across data lines.
        let mut frames = std::mem::take(&mut self.json.frames);
        frames.clear();
        self.json = JsonUsage::new(kind);
        self.json.frames = frames;
        self.prefix = 0;
        self.ignored = false;
        usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(line: &str, kind: UsageKind, chunk_size: usize) -> Option<(i64, i64)> {
        let mut parser = SseUsage::new(kind);
        let mut usage = None;
        for chunk in line.as_bytes().chunks(chunk_size) {
            parser.push(chunk, |input, output| usage = Some((input, output)));
        }
        parser.finish_line().or(usage)
    }

    #[test]
    fn fragmented_fields_escapes_and_reordering_match_serde() {
        let lines = [
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":9}}}"#,
            r#"{"response":{"output":[{"text":"café 😀 \"usage\":{\"input_tokens\":999} \\"}],"usage":{"input_tokens":7,"output_tokens":9}},"type":"response.failed"}"#,
            r#"{"response":{"usage":{"input_tokens":7,"output_tokens":9}},"type":"response.incomplete"}"#,
            r#"{"type":"response.completed","response":{"\u0075sage":{"input_tokens":7,"output_tokens":9,"details":{"input_tokens":999}}}}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":1,"input_tokens":7,"output_tokens":9}}}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":9}},"response":{"usage":null}}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":9,"input_tokens":null}}}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":9}},"type":"response.created"}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":9}},"type":{}}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":9},"usage":[]}}"#,
            r#"{"type":"response.created","response":{"usage":{"input_tokens":7,"output_tokens":9}}}"#,
        ];
        for json in lines {
            let value: serde_json::Value = serde_json::from_str(json).unwrap();
            let terminal = matches!(
                value["type"].as_str(),
                Some("response.completed" | "response.incomplete" | "response.failed")
            );
            let input = value["response"]["usage"]["input_tokens"]
                .as_i64()
                .unwrap_or(0);
            let output = value["response"]["usage"]["output_tokens"]
                .as_i64()
                .unwrap_or(0);
            let expected = (terminal && (input > 0 || output > 0)).then_some((input, output));
            for size in [1, 2, 3, 7, 4096] {
                for ending in ["", "\r\n\n"] {
                    assert_eq!(
                        extract(&format!("data: {json}{ending}"), UsageKind::Responses, size),
                        expected,
                        "{json}, chunk size {size}"
                    );
                }
            }
        }
    }

    #[test]
    fn malformed_and_nested_usage_are_not_accounted() {
        for json in [
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7}},}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7}}}{}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7}}"#,
            r#"{"type":"response.completed","response":{"output":[{"usage":{"input_tokens":7}}]}}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":07}}}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7}}]"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":7},"other":[1,]}}"#,
        ] {
            assert_eq!(
                extract(&format!("data: {json}\n"), UsageKind::Responses, 1),
                None,
                "{json}"
            );
        }
    }

    #[test]
    fn large_arrays_strings_and_keys_do_not_grow_parser_memory() {
        let mut parser = SseUsage::new(UsageKind::Responses);
        let mut recorded = Vec::new();
        parser.push(
            b"data: {\"type\":\"response.completed\",\"response\":{\"output\":[\"",
            |_, _| panic!("unfinished"),
        );
        let chunk = [b'x'; 8192];
        for _ in 0..4097 {
            parser.push(&chunk, |_, _| panic!("unfinished"));
        }
        parser.push(b"\",0", |_, _| panic!("unfinished"));
        for _ in 0..100_000 {
            parser.push(b",{\"usage\":{\"input_tokens\":999}}", |_, _| {
                panic!("nested usage")
            });
        }
        parser.push(b"],\"", |_, _| panic!("unfinished"));
        for _ in 0..32 {
            parser.push(&chunk, |_, _| panic!("unfinished"));
        }
        parser.push(
            b"\":\"ignored\",\"usage\":{\"input_tokens\":74122,\"output_tokens\":32989}}}\n\n",
            |input, output| recorded.push((input, output)),
        );
        assert_eq!(recorded, [(74122, 32989)]);
        assert!(parser.json.frames.capacity() <= 128);
        assert!(std::mem::size_of::<SseUsage>() < 1024);
    }

    #[test]
    fn ignores_non_data_lines_and_keeps_last_terminal_usage() {
        let line = "event: response.completed\n: comment\ndata: [DONE]\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":7}}}\ndata: {\"type\":\"response.failed\",\"response\":{\"usage\":{\"input_tokens\":9}}}\n";
        assert_eq!(extract(line, UsageKind::Responses, 1), Some((9, 0)));
        assert_eq!(
            extract(
                "data:{\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":9}}\n",
                UsageKind::ChatCompletions,
                1
            ),
            Some((7, 9))
        );
    }

    #[test]
    fn irrelevant_large_numbers_and_escaped_event_type_keep_usage() {
        let event_type: String = "response.completed"
            .chars()
            .map(|c| format!("\\u{:04x}", c as u32))
            .collect();
        let json = format!("data: {{\"type\":\"{event_type}\",\"response\":{{\"score\":{},\"usage\":{{\"input_tokens\":7,\"output_tokens\":9}}}}}}\n", "1".repeat(1024));
        assert_eq!(extract(&json, UsageKind::Responses, 1), Some((7, 9)));
        for number in ["1e9999", "0.5", "-2e-3", "12345678901234567890"] {
            let json = format!("data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":{number},\"output_tokens\":9}}}}}}\n");
            assert_eq!(extract(&json, UsageKind::Responses, 1), Some((0, 9)));
        }
    }
}
