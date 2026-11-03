// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Decodes an upstream byte stream to text across network reads. A read can end
//! inside a multibyte character, so decoding each one on its own corrupts it.

/// Most text a provider's translated stream may hold while waiting for the end of
/// an event. A single event can carry inline media, so this is generous, but an
/// upstream that never ends one can't grow the buffer without limit.
pub(crate) const MAX_BUFFERED_STREAM_BYTES: usize = 32 * 1024 * 1024;

/// Appends each read to a text buffer, holding back an incomplete trailing
/// character (at most 3 bytes) until the rest of it arrives.
#[derive(Default)]
pub(crate) struct Utf8StreamDecoder {
    incomplete: Vec<u8>,
}

impl Utf8StreamDecoder {
    pub(crate) fn decode_into(&mut self, chunk: &[u8], text: &mut String) {
        let joined;
        let mut bytes = if self.incomplete.is_empty() {
            chunk
        } else {
            self.incomplete.extend_from_slice(chunk);
            joined = std::mem::take(&mut self.incomplete);
            joined.as_slice()
        };
        loop {
            let error = match std::str::from_utf8(bytes) {
                Ok(valid) => {
                    text.push_str(valid);
                    return;
                }
                Err(error) => error,
            };
            let (valid, rest) = bytes.split_at(error.valid_up_to());
            text.push_str(&String::from_utf8_lossy(valid));
            let Some(invalid_len) = error.error_len() else {
                self.incomplete = rest.to_vec();
                return;
            };
            text.push(char::REPLACEMENT_CHARACTER);
            bytes = &rest[invalid_len..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str = "h\u{e9}llo w\u{f6}rld \u{2713} \u{65e5}\u{672c}";

    #[test]
    fn a_character_split_across_reads_survives_at_every_split_point() {
        let bytes = TEXT.as_bytes();
        for split in 0..=bytes.len() {
            let mut decoder = Utf8StreamDecoder::default();
            let mut text = String::new();
            decoder.decode_into(&bytes[..split], &mut text);
            decoder.decode_into(&bytes[split..], &mut text);
            assert_eq!(text, TEXT, "split at byte {split}");
        }
    }

    #[test]
    fn a_read_of_one_byte_at_a_time_survives() {
        let mut decoder = Utf8StreamDecoder::default();
        let mut text = String::new();
        for byte in TEXT.as_bytes() {
            decoder.decode_into(std::slice::from_ref(byte), &mut text);
        }
        assert_eq!(text, TEXT);
    }

    #[test]
    fn invalid_bytes_are_replaced_and_decoding_carries_on() {
        let mut decoder = Utf8StreamDecoder::default();
        let mut text = String::new();
        decoder.decode_into(b"a\xffb", &mut text);
        decoder.decode_into(b"c", &mut text);
        assert_eq!(text, "a\u{fffd}bc");
    }
}
