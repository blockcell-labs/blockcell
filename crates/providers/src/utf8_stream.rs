//! Incremental decoding shared by streaming adapters.
#[derive(Default)]
pub(crate) struct Utf8Stream {
    pending: Vec<u8>,
}
impl Utf8Stream {
    pub fn push(&mut self, bytes: &[u8]) -> Result<String, std::str::Utf8Error> {
        self.pending.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(text) => text.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => return Err(error),
        };
        let text = std::str::from_utf8(&self.pending[..valid])?.to_owned();
        self.pending.drain(..valid);
        Ok(text)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_split_preserves_text_and_tool_json() {
        let text = "data: {\"text\":\"中文🙂\",\"path\":\"报告.txt\"}\r\n\n";
        for split in 0..=text.len() {
            let mut decoder = Utf8Stream::default();
            let mut actual = decoder.push(&text.as_bytes()[..split]).unwrap();
            actual.push_str(&decoder.push(&text.as_bytes()[split..]).unwrap());
            assert_eq!(actual, text, "split={split}");
        }
    }
    #[test]
    fn byte_chunks_and_invalid_bytes() {
        let mut decoder = Utf8Stream::default();
        let mut actual = String::new();
        for byte in "中文🙂".bytes() {
            actual.push_str(&decoder.push(&[byte]).unwrap());
        }
        assert_eq!(actual, "中文🙂");
        assert!(decoder.push(&[0xff]).is_err());
    }
}
