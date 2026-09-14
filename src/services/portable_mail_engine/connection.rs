// SPDX-License-Identifier: GPL-3.0-or-later
//
// Network Connection layer for Portable Mail Engine (IMAP & SMTP).
// Uses GIO SocketClient and TLS for native cross-platform networking,
// and supports in-memory mocking for testing.

use gio::prelude::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

pub const DEFAULT_SOCKET_TIMEOUT_SECS: u32 = 30;

pub enum Transport {
    Network {
        _conn: gio::SocketConnection,
        data_in: gio::DataInputStream,
        out_stream: gio::OutputStream,
        cancellable: gio::Cancellable,
    },
    Mock {
        read_lines: VecDeque<String>,
        written: Vec<String>,
        cancellable: gio::Cancellable,
    },
}

pub struct MailConnection {
    transport: Transport,
}

impl MailConnection {
    pub fn connect(
        host: &str,
        port: u16,
        use_tls: bool,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        let cancellable = gio::Cancellable::new();
        if let Some(c) = cancelled {
            if c.load(Ordering::Relaxed) {
                cancellable.cancel();
                return Err("Verbindung wurde abgebrochen".into());
            }
        }

        let client = gio::SocketClient::new();
        client.set_timeout(DEFAULT_SOCKET_TIMEOUT_SECS);
        if use_tls {
            client.set_tls(true);
        }

        let address = format!("{}:{}", host, port);
        let conn = client
            .connect_to_host(&address, port, Some(&cancellable))
            .map_err(|e| format!("Verbindungsfehler zu {address}: {e}"))?;

        let input_stream = conn.input_stream();
        let output_stream = conn.output_stream();
        let data_in = gio::DataInputStream::new(&input_stream);

        Ok(Self {
            transport: Transport::Network {
                _conn: conn,
                data_in,
                out_stream: output_stream,
                cancellable,
            },
        })
    }

    pub fn new_mock(canned_responses: Vec<String>) -> Self {
        Self {
            transport: Transport::Mock {
                read_lines: VecDeque::from(canned_responses),
                written: Vec::new(),
                cancellable: gio::Cancellable::new(),
            },
        }
    }

    #[allow(dead_code)]
    pub fn cancellable(&self) -> gio::Cancellable {
        match &self.transport {
            Transport::Network { cancellable, .. } => cancellable.clone(),
            Transport::Mock { cancellable, .. } => cancellable.clone(),
        }
    }

    pub fn cancel(&self) {
        match &self.transport {
            Transport::Network { cancellable, .. } => cancellable.cancel(),
            Transport::Mock { cancellable, .. } => cancellable.cancel(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        match &self.transport {
            Transport::Network { cancellable, .. } => cancellable.is_cancelled(),
            Transport::Mock { cancellable, .. } => cancellable.is_cancelled(),
        }
    }

    pub fn starttls(&mut self, host: &str, port: u16) -> Result<(), String> {
        match &mut self.transport {
            Transport::Network {
                _conn,
                data_in,
                out_stream,
                cancellable,
            } => {
                if cancellable.is_cancelled() {
                    return Err("Operation abgebrochen".into());
                }
                let server_identity = gio::NetworkAddress::new(host, port);
                let tls_conn = gio::TlsClientConnection::new(_conn, Some(&server_identity))
                    .map_err(|e| format!("TLS-Verbindungsfehler: {e}"))?;
                tls_conn
                    .handshake(Some(cancellable))
                    .map_err(|e| format!("TLS-Handshake fehlgeschlagen: {e}"))?;

                *data_in = gio::DataInputStream::new(&tls_conn.input_stream());
                *out_stream = tls_conn.output_stream();
                Ok(())
            }
            Transport::Mock { cancellable, .. } => {
                if cancellable.is_cancelled() {
                    return Err("Operation abgebrochen".into());
                }
                Ok(())
            }
        }
    }

    pub fn read_line(&mut self) -> Result<String, String> {
        match &mut self.transport {
            Transport::Network { data_in, cancellable, .. } => {
                if cancellable.is_cancelled() {
                    return Err("Operation abgebrochen".into());
                }
                let line = data_in
                    .read_line_utf8(Some(cancellable))
                    .map_err(|e| format!("Fehler beim Lesen der Antwort: {e}"))?;
                line.map(|s| s.to_string())
                    .ok_or_else(|| "Verbindung unerwartet geschlossen".into())
            }
            Transport::Mock { read_lines, cancellable, .. } => {
                if cancellable.is_cancelled() {
                    return Err("Operation abgebrochen".into());
                }
                read_lines
                    .pop_front()
                    .ok_or_else(|| "Keine weiteren Mock-Antworten".into())
            }
        }
    }

    pub fn write_line(&mut self, line: &str) -> Result<(), String> {
        let mut msg = line.to_string();
        if !msg.ends_with("\r\n") {
            msg.push_str("\r\n");
        }
        self.write_all(msg.as_bytes())
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<(), String> {
        match &mut self.transport {
            Transport::Network { out_stream, cancellable, .. } => {
                if cancellable.is_cancelled() {
                    return Err("Operation abgebrochen".into());
                }
                out_stream
                    .write_all(bytes, Some(cancellable))
                    .map_err(|e| format!("Fehler beim Senden von Daten: {e}"))?;
                out_stream
                    .flush(Some(cancellable))
                    .map_err(|e| format!("Fehler beim Übertragen von Daten: {e}"))?;
                Ok(())
            }
            Transport::Mock { written, cancellable, .. } => {
                if cancellable.is_cancelled() {
                    return Err("Operation abgebrochen".into());
                }
                written.push(String::from_utf8_lossy(bytes).into_owned());
                Ok(())
            }
        }
    }

    pub fn read_exact(&mut self, len: usize) -> Result<Vec<u8>, String> {
        match &mut self.transport {
            Transport::Network { data_in, cancellable, .. } => {
                if cancellable.is_cancelled() {
                    return Err("Operation abgebrochen".into());
                }
                let mut buf = vec![0u8; len];
                let in_stream = data_in.upcast_ref::<gio::InputStream>();
                in_stream
                    .read_all(&mut buf, Some(cancellable))
                    .map_err(|e| format!("Fehler beim Lesen binärer Daten: {e}"))?;
                Ok(buf)
            }
            Transport::Mock { read_lines, cancellable, .. } => {
                if cancellable.is_cancelled() {
                    return Err("Operation abgebrochen".into());
                }
                let text = read_lines.pop_front().unwrap_or_default();
                let bytes = text.into_bytes();
                if bytes.len() >= len {
                    Ok(bytes[..len].to_vec())
                } else {
                    let mut b = bytes;
                    b.resize(len, 0);
                    Ok(b)
                }
            }
        }
    }

    pub fn mock_written(&self) -> Vec<String> {
        match &self.transport {
            Transport::Mock { written, .. } => written.clone(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_connection_cancellation() {
        let responses = vec!["* OK IMAP Ready".to_string(), "TAG OK LOGIN".to_string()];
        let mut conn = MailConnection::new_mock(responses);

        assert!(!conn.is_cancelled());
        let line = conn.read_line().unwrap();
        assert_eq!(line, "* OK IMAP Ready");

        // Cancel connection
        conn.cancel();
        assert!(conn.is_cancelled());

        // Subsequent operations must return cancellation error
        assert!(conn.read_line().is_err());
        assert!(conn.write_line("TAG NOOP").is_err());
        assert!(conn.read_exact(4).is_err());
    }

    #[test]
    fn test_default_timeout_constant() {
        assert_eq!(DEFAULT_SOCKET_TIMEOUT_SECS, 30);
    }
}
