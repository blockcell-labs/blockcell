use std::io::{self, Write};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[test]
fn malformed_arguments_do_not_log_credentials() {
    let captured = Capture(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(captured.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let _: blockcell_core::types::ToolCallRequest = serde_json::from_value(serde_json::json!({
            "id": "test", "function": {"name": "encrypt", "arguments": "{\"password\":\"secret-p2-marker\","}
        })).unwrap();
        blockcell_core::types::ToolCallAccumulator {
            id: "test".into(),
            name: "encrypt".into(),
            arguments: "{\"password\":\"secret-p2-marker\",".into(),
        }
        .to_tool_call_request();
    });
    let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    assert!(output.contains("Failed to parse"));
    assert!(!output.contains("secret-p2-marker"), "{output}");
}
