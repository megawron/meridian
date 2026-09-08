use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

static LOG_BUFFER: OnceLock<Arc<Mutex<VecDeque<String>>>> = OnceLock::new();

pub fn log_buffer() -> Arc<Mutex<VecDeque<String>>> {
    LOG_BUFFER
        .get_or_init(|| Arc::new(Mutex::new(VecDeque::with_capacity(1000))))
        .clone()
}

fn strip_ansi_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_escape = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_escape = true;
        } else if in_escape {
            if c.is_ascii_alphabetic() {
                in_escape = false;
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn append_log_line(line: &str) {
    let cleaned = strip_ansi_codes(line);
    let buffer = log_buffer();
    if let Ok(mut buf) = buffer.lock() {
        if buf.len() >= 1000 {
            buf.pop_front();
        }
        buf.push_back(cleaned);
    }
}

pub fn recent_logs() -> Vec<String> {
    let buffer = log_buffer();
    if let Ok(buf) = buffer.lock() {
        buf.iter().cloned().collect()
    } else {
        Vec::new()
    }
}

pub fn clear_logs() {
    let buffer = log_buffer();
    if let Ok(mut buf) = buffer.lock() {
        buf.clear();
    }
}

pub struct LogBufferWriter;

impl std::io::Write for LogBufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        for line in text.lines() {
            let line = line.trim_end();
            if !line.is_empty() {
                append_log_line(line);
            }
        }
        std::io::stdout().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush()
    }
}
