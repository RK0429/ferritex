use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use ferritex_application::ports::PreviewTransportPort;

const LOOPBACK_HTTP_BASE: &str = "http://127.0.0.1/preview";
const LOOPBACK_WS_BASE: &str = "ws://127.0.0.1/preview";
const NO_STORE_CACHE_CONTROL: &str = "no-store, no-cache, must-revalidate";
const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024;
const BAD_REQUEST_RESPONSE: &[u8] = b"HTTP/1.1 400 Bad Request\r\n\r\n";
const NOT_FOUND_RESPONSE: &[u8] = b"HTTP/1.1 404 Not Found\r\n\r\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewDocumentResponse {
    pub bytes: Vec<u8>,
    pub cache_control: &'static str,
}

#[derive(Debug)]
pub struct LoopbackPreviewTransport {
    documents: Mutex<HashMap<String, Vec<u8>>>,
    listener: Option<TcpListener>,
    port: Option<u16>,
}

impl Default for LoopbackPreviewTransport {
    fn default() -> Self {
        Self {
            documents: Mutex::new(HashMap::new()),
            listener: None,
            port: None,
        }
    }
}

impl LoopbackPreviewTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind() -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| format!("failed to bind loopback preview transport: {error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| {
                format!("failed to inspect loopback preview transport address: {error}")
            })?
            .port();

        Ok(Self {
            documents: Mutex::new(HashMap::new()),
            listener: Some(listener),
            port: Some(port),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
            .expect("loopback preview transport is not bound to a TCP port")
    }

    pub fn serve_document(&self, session_id: &str) -> Option<PreviewDocumentResponse> {
        let documents = self
            .documents
            .lock()
            .expect("preview document store poisoned");
        documents
            .get(session_id)
            .map(|bytes| PreviewDocumentResponse {
                bytes: bytes.clone(),
                cache_control: NO_STORE_CACHE_CONTROL,
            })
    }

    pub fn serve_blocking(&self) {
        let Some(listener) = &self.listener else {
            return;
        };

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(error) = self.handle_connection(stream) {
                        tracing::warn!(error, "failed to serve preview loopback connection");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to accept preview loopback connection");
                }
            }
        }
    }

    pub fn start_background(self: &Arc<Self>) {
        if self.listener.is_none() {
            return;
        }

        let transport = Arc::clone(self);
        let (ready_tx, ready_rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = ready_tx.send(());
            transport.serve_blocking();
        });
        let _ = ready_rx.recv();
    }

    fn handle_connection(&self, mut stream: TcpStream) -> Result<(), String> {
        let request_line = {
            let reader_stream = stream
                .try_clone()
                .map_err(|error| format!("failed to clone preview loopback stream: {error}"))?;
            let mut reader = BufReader::new(reader_stream);
            match Self::read_request_line(&mut reader)
                .and_then(|request_line| Self::consume_headers(&mut reader).map(|_| request_line))
            {
                Ok(request_line) => request_line,
                Err(_) => {
                    let _ = Self::consume_headers(&mut reader);
                    stream.write_all(BAD_REQUEST_RESPONSE).map_err(|error| {
                        format!("failed to write preview loopback response: {error}")
                    })?;
                    stream.flush().map_err(|error| {
                        format!("failed to flush preview loopback response: {error}")
                    })?;
                    return Ok(());
                }
            }
        };
        let response = match Self::parse_document_request(&request_line) {
            Ok(session_id) => match self.serve_document(session_id) {
                Some(document) => Self::ok_response(document),
                None => NOT_FOUND_RESPONSE.to_vec(),
            },
            Err(()) => BAD_REQUEST_RESPONSE.to_vec(),
        };

        stream
            .write_all(&response)
            .map_err(|error| format!("failed to write preview loopback response: {error}"))?;
        stream
            .flush()
            .map_err(|error| format!("failed to flush preview loopback response: {error}"))
    }

    fn read_request_line(reader: &mut BufReader<TcpStream>) -> Result<String, String> {
        let mut request_line = String::new();
        let bytes_read = {
            let mut limited_reader = reader.by_ref().take(MAX_REQUEST_LINE_BYTES as u64);
            limited_reader
                .read_line(&mut request_line)
                .map_err(|error| format!("failed to read preview loopback request: {error}"))?
        };

        if request_line.is_empty() {
            return Err("preview loopback request did not contain a request line".to_string());
        }
        if bytes_read == MAX_REQUEST_LINE_BYTES && !request_line.ends_with('\n') {
            Self::discard_until_newline(reader)?;
            return Err("preview loopback request line exceeded the maximum size".to_string());
        }

        Ok(request_line)
    }

    fn consume_headers(reader: &mut BufReader<TcpStream>) -> Result<(), String> {
        loop {
            let mut header_line = String::new();
            let bytes_read = reader
                .read_line(&mut header_line)
                .map_err(|error| format!("failed to read preview loopback headers: {error}"))?;

            if bytes_read == 0 {
                return Err(
                    "preview loopback request ended before the header terminator".to_string(),
                );
            }
            if header_line == "\r\n" || header_line == "\n" {
                return Ok(());
            }
        }
    }

    fn discard_until_newline(reader: &mut BufReader<TcpStream>) -> Result<(), String> {
        loop {
            let mut discarded = Vec::new();
            let bytes_read = reader
                .read_until(b'\n', &mut discarded)
                .map_err(|error| format!("failed to discard preview loopback request: {error}"))?;

            if bytes_read == 0 || discarded.ends_with(b"\n") {
                return Ok(());
            }
        }
    }

    fn parse_document_request(request_line: &str) -> Result<&str, ()> {
        let trimmed = request_line.trim_end_matches(['\r', '\n']);
        let mut parts = trimmed.split_whitespace();
        let Some(method) = parts.next() else {
            return Err(());
        };
        let Some(path) = parts.next() else {
            return Err(());
        };
        let Some(version) = parts.next() else {
            return Err(());
        };

        if method != "GET" || version != "HTTP/1.1" || parts.next().is_some() {
            return Err(());
        }

        let Some(session_path) = path.strip_prefix("/preview/") else {
            return Err(());
        };
        let Some(session_id) = session_path.strip_suffix("/document") else {
            return Err(());
        };

        if session_id.is_empty() {
            return Err(());
        }

        Ok(session_id)
    }

    fn ok_response(document: PreviewDocumentResponse) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nCache-Control: {}\r\nContent-Length: {}\r\n\r\n",
            document.cache_control,
            document.bytes.len()
        )
        .into_bytes();
        response.extend_from_slice(&document.bytes);
        response
    }

    fn http_base(&self) -> String {
        match self.port {
            Some(port) => format!("http://127.0.0.1:{port}/preview"),
            None => LOOPBACK_HTTP_BASE.to_string(),
        }
    }

    fn ws_base(&self) -> String {
        match self.port {
            Some(port) => format!("ws://127.0.0.1:{port}/preview"),
            None => LOOPBACK_WS_BASE.to_string(),
        }
    }
}

impl PreviewTransportPort for LoopbackPreviewTransport {
    fn publish_pdf(&self, session_id: &str, pdf_bytes: &[u8]) -> Result<(), String> {
        let mut documents = self
            .documents
            .lock()
            .map_err(|_| "failed to acquire preview transport document store lock".to_string())?;
        documents.insert(session_id.to_string(), pdf_bytes.to_vec());

        tracing::info!(
            session_id,
            byte_len = pdf_bytes.len(),
            "preview pdf published to loopback transport"
        );

        Ok(())
    }

    fn session_url(&self, session_id: &str) -> String {
        format!("{}/{session_id}", self.http_base())
    }

    fn document_url(&self, session_id: &str) -> String {
        format!("{}/document", self.session_url(session_id))
    }

    fn events_url(&self, session_id: &str) -> String {
        format!("{}/{session_id}/events", self.ws_base())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpStream};
    use std::sync::Arc;

    use ferritex_application::ports::PreviewTransportPort;

    use super::LoopbackPreviewTransport;

    #[test]
    fn publish_and_serve_returns_pdf_with_no_store_header() {
        let transport = LoopbackPreviewTransport::new();
        let pdf = b"%PDF-1.4\nhello\n";

        transport
            .publish_pdf("preview-session-1", pdf)
            .expect("publish pdf");

        let response = transport
            .serve_document("preview-session-1")
            .expect("serve document");

        assert_eq!(response.bytes, pdf);
        assert_eq!(
            response.cache_control,
            "no-store, no-cache, must-revalidate"
        );
    }

    #[test]
    fn serve_unknown_session_returns_none() {
        let transport = LoopbackPreviewTransport::new();

        assert!(transport.serve_document("missing-session").is_none());
    }

    #[test]
    fn bind_listens_on_loopback() {
        let transport = LoopbackPreviewTransport::bind().expect("bind loopback transport");

        assert!(transport.listener.is_some());
        assert!(transport.port() > 0);
        assert!(transport
            .listener
            .as_ref()
            .expect("listener")
            .local_addr()
            .expect("listener addr")
            .ip()
            .is_loopback());
    }

    #[test]
    fn serve_returns_pdf_over_http() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let pdf = b"%PDF-1.4\nhello\n";

        transport
            .publish_pdf("preview-session-1", pdf)
            .expect("publish pdf");
        transport.start_background();

        let response = issue_get_request(transport.port(), "/preview/preview-session-1/document");

        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(response
            .windows("Cache-Control: no-store, no-cache, must-revalidate".len())
            .any(|window| window == b"Cache-Control: no-store, no-cache, must-revalidate"));

        let (_, body) = split_http_response(&response);
        assert_eq!(body, pdf);
    }

    #[test]
    fn serve_returns_404_for_unknown_session() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        transport.start_background();

        let response = issue_get_request(transport.port(), "/preview/missing-session/document");

        assert!(response.starts_with(b"HTTP/1.1 404 Not Found\r\n\r\n"));
    }

    #[test]
    fn request_line_too_large_returns_bad_request() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        transport.start_background();

        let mut stream =
            TcpStream::connect(("127.0.0.1", transport.port())).expect("connect to preview");
        let oversized_path = "a".repeat(9 * 1024);
        write!(
            stream,
            "GET /preview/{oversized_path}/document HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            transport.port()
        )
        .expect("write oversized request");
        stream.shutdown(Shutdown::Write).expect("shutdown write");

        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");

        assert!(response.starts_with(b"HTTP/1.1 400 Bad Request\r\n\r\n"));
    }

    fn issue_get_request(port: u16, path: &str) -> Vec<u8> {
        let mut stream =
            TcpStream::connect(("127.0.0.1", port)).expect("connect to loopback preview server");
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        )
        .expect("write request");
        stream.shutdown(Shutdown::Write).expect("shutdown write");

        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        response
    }

    fn split_http_response(response: &[u8]) -> (&[u8], &[u8]) {
        let separator = b"\r\n\r\n";
        let body_start = response
            .windows(separator.len())
            .position(|window| window == separator)
            .expect("response separator")
            + separator.len();
        response.split_at(body_start)
    }
}
