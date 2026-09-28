/// Stateful normalizer for bytes written to a Windows pseudoconsole.
///
/// `ConPTY` expects Enter as carriage return and Backspace as DEL. All other
/// bytes, including UTF-8 and terminal control sequences, pass unchanged.
pub(crate) struct WindowsTtyInputNormalizer {
    previous_was_cr: bool,
    at_line_start: bool,
}

impl Default for WindowsTtyInputNormalizer {
    fn default() -> Self {
        Self {
            previous_was_cr: false,
            at_line_start: true,
        }
    }
}

impl WindowsTtyInputNormalizer {
    pub(crate) fn normalize(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut normalized = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            match byte {
                b'\x08' => normalized.push(b'\x7f'),
                b'\n' if !self.previous_was_cr => normalized.push(b'\r'),
                b'\n' => {}
                _ => normalized.push(byte),
            }
            self.previous_was_cr = byte == b'\r';
            self.at_line_start = matches!(byte, b'\r' | b'\n');
        }
        normalized
    }

    /// Windows cooked console input recognizes Ctrl+Z followed by Enter as
    /// EOF. Closing the ConPTY transport pipe alone does not deliver console
    /// EOF. Finish pending input first so Ctrl+Z starts on its own line.
    pub(crate) fn eof_sequence(&self) -> &'static [u8] {
        if self.at_line_start {
            b"\x1a\r"
        } else {
            b"\r\x1a\r"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WindowsTtyInputNormalizer;

    #[test]
    fn preserves_raw_control_bytes_except_terminal_key_normalization() {
        let mut normalizer = WindowsTtyInputNormalizer::default();
        assert_eq!(
            normalizer.normalize(b"a\x1b\x03\x04\x08\n"),
            b"a\x1b\x03\x04\x7f\r"
        );
    }

    #[test]
    fn eof_starts_on_a_new_console_line_without_adding_a_blank_line() {
        let mut normalizer = WindowsTtyInputNormalizer::default();
        assert_eq!(normalizer.eof_sequence(), b"\x1a\r");
        normalizer.normalize(b"pending");
        assert_eq!(normalizer.eof_sequence(), b"\r\x1a\r");
        normalizer.normalize(b"\r");
        assert_eq!(normalizer.eof_sequence(), b"\x1a\r");
        normalizer.normalize(b"\n");
        assert_eq!(normalizer.eof_sequence(), b"\x1a\r");
    }

    #[test]
    fn utf8_is_preserved_across_input_chunks() {
        let bytes = "你好, Windows ConPTY\n".as_bytes();
        let mut normalizer = WindowsTtyInputNormalizer::default();
        let mut output = normalizer.normalize(&bytes[..2]);
        output.extend(normalizer.normalize(&bytes[2..]));
        assert_eq!(output, "你好, Windows ConPTY\r".as_bytes());
    }

    #[test]
    fn collapses_crlf_across_writes() {
        let mut normalizer = WindowsTtyInputNormalizer::default();
        assert_eq!(normalizer.normalize(b"line\r"), b"line\r");
        assert!(normalizer.normalize(b"\n").is_empty());
    }
}
