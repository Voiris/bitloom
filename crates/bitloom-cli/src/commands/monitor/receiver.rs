use bitloom_config::MonitorRxMode;

const REPLACEMENT: &[u8] = "�".as_bytes();

pub(super) struct RxStream {
    renderer: Renderer,
}

enum Renderer {
    Utf8(Utf8Decoder),
    Ascii,
    Hex(HexRenderer),
    Raw,
}

#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

#[derive(Default)]
struct HexRenderer {
    offset: u64,
}

impl RxStream {
    pub(super) fn new(mode: MonitorRxMode) -> Self {
        let renderer = match mode {
            MonitorRxMode::Utf8 => Renderer::Utf8(Utf8Decoder::default()),
            MonitorRxMode::Ascii => Renderer::Ascii,
            MonitorRxMode::Hex => Renderer::Hex(HexRenderer::default()),
            MonitorRxMode::Raw => Renderer::Raw,
        };
        Self { renderer }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        match &mut self.renderer {
            Renderer::Utf8(decoder) => decoder.push(bytes),
            Renderer::Ascii => decode_ascii(bytes),
            Renderer::Hex(renderer) => renderer.push(bytes),
            Renderer::Raw => bytes.to_vec(),
        }
    }

    pub(super) fn finish(&mut self) -> Vec<u8> {
        match &mut self.renderer {
            Renderer::Utf8(decoder) => decoder.finish(),
            Renderer::Ascii | Renderer::Hex(_) | Renderer::Raw => Vec::new(),
        }
    }
}

impl Utf8Decoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(bytes);
        self.decode(false)
    }

    fn finish(&mut self) -> Vec<u8> {
        self.decode(true)
    }

    fn decode(&mut self, end_of_stream: bool) -> Vec<u8> {
        let mut output = Vec::with_capacity(self.pending.len());

        while !self.pending.is_empty() {
            match std::str::from_utf8(&self.pending) {
                Ok(_) => {
                    output.append(&mut self.pending);
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    output.extend_from_slice(&self.pending[..valid]);
                    self.pending.drain(..valid);

                    match error.error_len() {
                        Some(invalid) => {
                            output.extend_from_slice(REPLACEMENT);
                            self.pending.drain(..invalid);
                        }
                        None if end_of_stream => {
                            output.extend_from_slice(REPLACEMENT);
                            self.pending.clear();
                        }
                        None => break,
                    }
                }
            }
        }

        output
    }
}

impl HexRenderer {
    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut output = String::new();
        for row in bytes.chunks(16) {
            use std::fmt::Write as _;

            let _ = write!(output, "{:08x}  ", self.offset);
            for index in 0..16 {
                if index == 8 {
                    output.push(' ');
                }
                if let Some(byte) = row.get(index) {
                    let _ = write!(output, "{byte:02x} ");
                } else {
                    output.push_str("   ");
                }
            }
            output.push_str(" | ");
            for byte in row {
                output.push(if byte.is_ascii_graphic() || *byte == b' ' {
                    char::from(*byte)
                } else {
                    '.'
                });
            }
            for _ in row.len()..16 {
                output.push(' ');
            }
            output.push_str(" |\r\n");
            self.offset = self.offset.saturating_add(row.len() as u64);
        }
        output.into_bytes()
    }
}

fn decode_ascii(bytes: &[u8]) -> Vec<u8> {
    let invalid = bytes.iter().filter(|byte| !byte.is_ascii()).count();
    let mut output = Vec::with_capacity(bytes.len().saturating_add(invalid.saturating_mul(2)));
    for byte in bytes {
        if byte.is_ascii() {
            output.push(*byte);
        } else {
            output.extend_from_slice(REPLACEMENT);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_decoder_keeps_a_split_code_point_between_reads() {
        let mut stream = RxStream::new(MonitorRxMode::Utf8);
        assert_eq!(stream.push(&[0xf0, 0x9f]), b"");
        assert_eq!(stream.push(&[0x98]), b"");
        assert_eq!(stream.push(&[0x80, b'!']), "😀!".as_bytes());
        assert_eq!(stream.finish(), b"");
    }

    #[test]
    fn utf8_decoder_replaces_invalid_and_incomplete_sequences() {
        let mut stream = RxStream::new(MonitorRxMode::Utf8);
        assert_eq!(stream.push(&[b'a', 0xff, b'b']), "a�b".as_bytes());
        assert_eq!(stream.push(&[0xe2, 0x82]), b"");
        assert_eq!(stream.finish(), "�".as_bytes());
    }

    #[test]
    fn ascii_decoder_preserves_ascii_and_marks_high_bytes() {
        let mut stream = RxStream::new(MonitorRxMode::Ascii);
        assert_eq!(stream.push(&[b'A', b'\n', 0x80]), "A\n�".as_bytes());
    }

    #[test]
    fn hex_renderer_tracks_offsets_and_printable_bytes() {
        let mut stream = RxStream::new(MonitorRxMode::Hex);
        let first = String::from_utf8(stream.push(b"Hi\0")).unwrap();
        let second = String::from_utf8(stream.push(&[0xff])).unwrap();
        assert!(first.starts_with("00000000  48 69 00"));
        assert!(first.ends_with(" | Hi.              |\r\n"));
        assert!(second.starts_with("00000003  ff"));
        assert!(second.ends_with(" | .                |\r\n"));
        assert_eq!(first.len(), second.len());
    }

    #[test]
    fn raw_renderer_preserves_every_byte() {
        let bytes = [0, 0x1b, 0xff, b'\n'];
        let mut stream = RxStream::new(MonitorRxMode::Raw);
        assert_eq!(stream.push(&bytes), bytes);
    }
}
