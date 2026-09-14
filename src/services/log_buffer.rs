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
        } else if c != '\0' {
            out.push(c);
        }
    }
    out
}

pub fn append_log_line(line: &str) {
    let mut cleaned = strip_ansi_codes(line);
    if cleaned.contains('\0') {
        cleaned = cleaned.replace('\0', "");
    }
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
        buf.iter()
            .map(|s| {
                if s.contains('\0') {
                    s.replace('\0', "")
                } else {
                    s.clone()
                }
            })
            .collect()
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
            let line = line.trim_end().trim_matches('\0');
            if !line.is_empty() {
                append_log_line(line);
            }
        }
        let _ = std::io::stdout().write(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stdout().flush();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_buffer_strips_interior_nul_bytes() {
        append_log_line("UniquePrefixNulTest\0with\0nul\0bytes");
        let logs = recent_logs();
        assert!(logs.iter().all(|line| !line.contains('\0')));
        assert!(logs.iter().any(|line| line.contains("UniquePrefixNulTestwithnulbytes")));
    }

    #[test]
    fn test_log_buffer_strips_ansi_and_nul() {
        append_log_line("\x1b[32mUniquePrefixGreen\0Text\x1b[0m");
        let logs = recent_logs();
        assert!(logs.iter().all(|line| !line.contains('\0')));
        assert!(logs.iter().any(|line| line.contains("UniquePrefixGreenText")));
    }
}
