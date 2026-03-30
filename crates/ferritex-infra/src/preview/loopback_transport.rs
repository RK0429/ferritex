use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use ferritex_application::ports::{
    EventsSessionError, PreviewTransportPort, TransportRevisionEvent, TransportViewStateUpdate,
};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentLookupResult {
    Found(PreviewDocumentResponse),
    Invalidated,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopbackRequest<'a> {
    GetDocument(&'a str),
    GetEvents(&'a str),
}

#[derive(Debug, Deserialize)]
struct LoopbackViewStatePayload {
    page_number: usize,
    zoom: f64,
    viewport_offset_y: f64,
}

#[derive(Debug, Serialize)]
struct LoopbackRevisionEventPayload<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    session_id: &'a str,
    target_input: &'a str,
    target_jobname: &'a str,
    revision: u64,
    page_count: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct LoopbackRequestHeaders {
    connection: Option<String>,
    upgrade: Option<String>,
    sec_websocket_key: Option<String>,
    sec_websocket_version: Option<String>,
}

pub struct LoopbackPreviewTransport {
    documents: Mutex<HashMap<String, Vec<u8>>>,
    invalidated_sessions: Mutex<HashSet<String>>,
    pending_events: Mutex<HashMap<String, Vec<TransportRevisionEvent>>>,
    pending_view_updates: Mutex<HashMap<String, Vec<TransportViewStateUpdate>>>,
    event_subscribers: Mutex<HashMap<String, Vec<TcpStream>>>,
    view_state_handler: Mutex<Option<Arc<dyn Fn(&str, &TransportViewStateUpdate) + Send + Sync>>>,
    listener: Option<TcpListener>,
    port: Option<u16>,
}

impl std::fmt::Debug for LoopbackPreviewTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopbackPreviewTransport")
            .field("documents", &"<mutex>")
            .field("invalidated_sessions", &"<mutex>")
            .field("pending_events", &"<mutex>")
            .field("pending_view_updates", &"<mutex>")
            .field("event_subscribers", &"<mutex>")
            .field("view_state_handler", &"<handler>")
            .field("listener", &self.listener)
            .field("port", &self.port)
            .finish()
    }
}

impl Default for LoopbackPreviewTransport {
    fn default() -> Self {
        Self {
            documents: Mutex::new(HashMap::new()),
            invalidated_sessions: Mutex::new(HashSet::new()),
            pending_events: Mutex::new(HashMap::new()),
            pending_view_updates: Mutex::new(HashMap::new()),
            event_subscribers: Mutex::new(HashMap::new()),
            view_state_handler: Mutex::new(None),
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
            invalidated_sessions: Mutex::new(HashSet::new()),
            pending_events: Mutex::new(HashMap::new()),
            pending_view_updates: Mutex::new(HashMap::new()),
            event_subscribers: Mutex::new(HashMap::new()),
            view_state_handler: Mutex::new(None),
            listener: Some(listener),
            port: Some(port),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
            .expect("loopback preview transport is not bound to a TCP port")
    }

    pub fn serve_document(&self, session_id: &str) -> DocumentLookupResult {
        let invalidated = self
            .invalidated_sessions
            .lock()
            .expect("preview invalidated sessions store poisoned");
        if invalidated.contains(session_id) {
            return DocumentLookupResult::Invalidated;
        }
        drop(invalidated);

        let documents = self
            .documents
            .lock()
            .expect("preview document store poisoned");
        match documents.get(session_id) {
            Some(bytes) => DocumentLookupResult::Found(PreviewDocumentResponse {
                bytes: bytes.clone(),
                cache_control: NO_STORE_CACHE_CONTROL,
            }),
            None => DocumentLookupResult::Unknown,
        }
    }

    pub fn serve_blocking(self: &Arc<Self>) {
        let Some(listener) = &self.listener else {
            return;
        };

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let transport = Arc::clone(self);
                    thread::spawn(move || {
                        if let Err(error) = transport.handle_connection(stream) {
                            tracing::warn!(error, "failed to serve preview loopback connection");
                        }
                    });
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

    pub fn set_view_state_handler(
        &self,
        handler: Arc<dyn Fn(&str, &TransportViewStateUpdate) + Send + Sync>,
    ) {
        let mut registered_handler = self
            .view_state_handler
            .lock()
            .expect("preview view state handler poisoned");
        *registered_handler = Some(handler);
    }

    fn handle_connection(&self, mut stream: TcpStream) -> Result<(), String> {
        let reader_stream = stream
            .try_clone()
            .map_err(|error| format!("failed to clone preview loopback stream: {error}"))?;
        let mut reader = BufReader::new(reader_stream);

        let request_line = match Self::read_request_line(&mut reader) {
            Ok(line) => line,
            Err(_) => {
                let _ = Self::consume_headers(&mut reader);
                Self::write_response(&mut stream, BAD_REQUEST_RESPONSE)?;
                return Ok(());
            }
        };

        let headers = match Self::consume_headers(&mut reader) {
            Ok(headers) => headers,
            Err(_) => {
                Self::write_response(&mut stream, BAD_REQUEST_RESPONSE)?;
                return Ok(());
            }
        };

        match Self::parse_loopback_request(&request_line) {
            Ok(LoopbackRequest::GetDocument(session_id)) => {
                let response = match self.serve_document(session_id) {
                    DocumentLookupResult::Found(document) => Self::ok_response(document),
                    DocumentLookupResult::Invalidated => Self::gone_response(session_id),
                    DocumentLookupResult::Unknown => NOT_FOUND_RESPONSE.to_vec(),
                };
                Self::write_response(&mut stream, &response)?;
            }
            Ok(LoopbackRequest::GetEvents(session_id)) => {
                self.handle_events_websocket(stream, session_id, headers)?;
                return Ok(());
            }
            Err(()) => {
                Self::write_response(&mut stream, BAD_REQUEST_RESPONSE)?;
            }
        }

        Ok(())
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

    fn consume_headers(
        reader: &mut BufReader<TcpStream>,
    ) -> Result<LoopbackRequestHeaders, String> {
        let mut headers = LoopbackRequestHeaders::default();
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
                return Ok(headers);
            }

            let trimmed = header_line.trim_end_matches(['\r', '\n']);
            if let Some((name, value)) = trimmed.split_once(':') {
                let value = value.trim().to_string();
                if name.eq_ignore_ascii_case("connection") {
                    headers.connection = Some(value);
                } else if name.eq_ignore_ascii_case("upgrade") {
                    headers.upgrade = Some(value);
                } else if name.eq_ignore_ascii_case("sec-websocket-key") {
                    headers.sec_websocket_key = Some(value);
                } else if name.eq_ignore_ascii_case("sec-websocket-version") {
                    headers.sec_websocket_version = Some(value);
                }
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

    fn parse_loopback_request(request_line: &str) -> Result<LoopbackRequest<'_>, ()> {
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

        if version != "HTTP/1.1" || parts.next().is_some() {
            return Err(());
        }

        let Some(session_path) = path.strip_prefix("/preview/") else {
            return Err(());
        };
        if let Some(session_id) = session_path.strip_suffix("/document") {
            if method == "GET" && !session_id.is_empty() {
                return Ok(LoopbackRequest::GetDocument(session_id));
            }
            return Err(());
        }
        if let Some(session_id) = session_path.strip_suffix("/events") {
            if method == "GET" && !session_id.is_empty() {
                return Ok(LoopbackRequest::GetEvents(session_id));
            }
            return Err(());
        }

        Err(())
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

    fn gone_response(session_id: &str) -> Vec<u8> {
        let body = format!(
            "{{\"error\":\"session_expired\",\"session_id\":\"{session_id}\",\"recovery\":\"bootstrap a new preview session via POST /preview/session\"}}"
        );
        format!(
            "HTTP/1.1 410 Gone\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn write_response(stream: &mut TcpStream, response: &[u8]) -> Result<(), String> {
        stream
            .write_all(response)
            .map_err(|error| format!("failed to write preview loopback response: {error}"))?;
        stream
            .flush()
            .map_err(|error| format!("failed to flush preview loopback response: {error}"))
    }

    fn handle_events_websocket(
        &self,
        mut stream: TcpStream,
        session_id: &str,
        headers: LoopbackRequestHeaders,
    ) -> Result<(), String> {
        match self.events_session_error(session_id) {
            Ok(()) => {}
            Err(EventsSessionError::Expired { .. }) => {
                Self::write_response(&mut stream, &Self::gone_response(session_id))?;
                return Ok(());
            }
            Err(EventsSessionError::Unknown { .. }) => {
                Self::write_response(&mut stream, NOT_FOUND_RESPONSE)?;
                return Ok(());
            }
        }

        if !Self::is_websocket_upgrade_request(&headers) {
            Self::write_response(&mut stream, BAD_REQUEST_RESPONSE)?;
            return Ok(());
        }

        let Some(client_key) = headers.sec_websocket_key.as_deref() else {
            Self::write_response(&mut stream, BAD_REQUEST_RESPONSE)?;
            return Ok(());
        };

        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
            ws_accept_key(client_key)
        );
        Self::write_response(&mut stream, response.as_bytes())?;

        let pending_events = {
            let mut pending_events = self.pending_events.lock().map_err(|_| {
                "failed to acquire preview transport pending events lock".to_string()
            })?;
            std::mem::take(pending_events.entry(session_id.to_string()).or_default())
        };

        for event in &pending_events {
            let payload = Self::revision_event_payload(event)?;
            ws_write_text(&mut stream, &payload)
                .map_err(|error| format!("failed to write preview websocket frame: {error}"))?;
        }

        let mut subscribers = self.event_subscribers.lock().map_err(|_| {
            "failed to acquire preview transport event subscribers lock".to_string()
        })?;
        subscribers.entry(session_id.to_string()).or_default().push(
            stream
                .try_clone()
                .map_err(|error| format!("failed to clone websocket subscriber: {error}"))?,
        );
        drop(subscribers);

        let local_addr = stream.local_addr().ok();
        let peer_addr = stream.peer_addr().ok();
        loop {
            let (opcode, payload) = match ws_read_frame(&mut stream) {
                Ok(frame) => frame,
                Err(error) => {
                    tracing::debug!(session_id, error, "preview websocket reader finished");
                    break;
                }
            };

            match opcode {
                WS_OP_TEXT => {
                    let payload: LoopbackViewStatePayload = serde_json::from_slice(&payload)
                        .map_err(|error| {
                            format!("failed to parse preview view-state update: {error}")
                        })?;
                    let update = TransportViewStateUpdate {
                        page_number: payload.page_number,
                        zoom: payload.zoom,
                        viewport_offset_y: payload.viewport_offset_y,
                    };
                    {
                        let handler = self
                            .view_state_handler
                            .lock()
                            .expect("preview view state handler poisoned");
                        if let Some(ref handler) = *handler {
                            handler(session_id, &update);
                        }
                    }
                    if self.submit_view_update(session_id, &update).is_err() {
                        let _ = ws_write_close(&mut stream, 1008);
                        break;
                    }
                }
                WS_OP_PING => {
                    ws_write_pong(&mut stream, &payload).map_err(|error| {
                        format!("failed to write preview websocket pong: {error}")
                    })?;
                }
                WS_OP_CLOSE => {
                    let code = if payload.len() >= 2 {
                        u16::from_be_bytes([payload[0], payload[1]])
                    } else {
                        1000
                    };
                    let _ = ws_write_close(&mut stream, code);
                    break;
                }
                _ => {
                    let _ = ws_write_close(&mut stream, 1003);
                    break;
                }
            }
        }

        self.remove_event_subscriber(session_id, local_addr, peer_addr);
        Ok(())
    }

    fn revision_event_payload(event: &TransportRevisionEvent) -> Result<Vec<u8>, String> {
        serde_json::to_vec(&LoopbackRevisionEventPayload {
            kind: "revision",
            session_id: &event.session_id,
            target_input: &event.target_input,
            target_jobname: &event.target_jobname,
            revision: event.revision,
            page_count: event.page_count,
        })
        .map_err(|error| format!("failed to serialize preview revision event: {error}"))
    }

    fn is_websocket_upgrade_request(headers: &LoopbackRequestHeaders) -> bool {
        let connection_ok = headers.connection.as_deref().is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
        });
        let upgrade_ok = headers
            .upgrade
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
        let version_ok = headers
            .sec_websocket_version
            .as_deref()
            .is_some_and(|value| value == "13");
        connection_ok && upgrade_ok && version_ok
    }

    fn ws_base(&self) -> String {
        match self.port {
            Some(port) => format!("ws://127.0.0.1:{port}/preview"),
            None => LOOPBACK_WS_BASE.to_string(),
        }
    }

    fn remove_event_subscriber(
        &self,
        session_id: &str,
        local_addr: Option<std::net::SocketAddr>,
        peer_addr: Option<std::net::SocketAddr>,
    ) {
        let Some(local_addr) = local_addr else {
            return;
        };
        let Some(peer_addr) = peer_addr else {
            return;
        };

        let mut subscribers = self
            .event_subscribers
            .lock()
            .expect("preview event subscribers store poisoned");
        let Some(streams) = subscribers.get_mut(session_id) else {
            return;
        };
        streams.retain(|subscriber| {
            subscriber.local_addr().ok() != Some(local_addr)
                || subscriber.peer_addr().ok() != Some(peer_addr)
        });
        if streams.is_empty() {
            subscribers.remove(session_id);
        }
    }

    fn close_event_subscribers(&self, session_id: &str) {
        let subscribers = self
            .event_subscribers
            .lock()
            .expect("preview event subscribers store poisoned")
            .remove(session_id);
        if let Some(mut subscribers) = subscribers {
            for mut subscriber in subscribers.drain(..) {
                let _ = ws_write_close(&mut subscriber, 1001);
                let _ = subscriber.shutdown(Shutdown::Both);
            }
        }
    }

    fn http_base(&self) -> String {
        match self.port {
            Some(port) => format!("http://127.0.0.1:{port}/preview"),
            None => LOOPBACK_HTTP_BASE.to_string(),
        }
    }

    fn ensure_events_session_registered(&self, session_id: &str) {
        self.pending_events
            .lock()
            .expect("preview pending events store poisoned")
            .entry(session_id.to_string())
            .or_default();
        self.pending_view_updates
            .lock()
            .expect("preview pending view updates store poisoned")
            .entry(session_id.to_string())
            .or_default();
    }

    fn events_session_error(&self, session_id: &str) -> Result<(), EventsSessionError> {
        if self
            .invalidated_sessions
            .lock()
            .expect("preview invalidated sessions store poisoned")
            .contains(session_id)
        {
            return Err(EventsSessionError::Expired {
                session_id: session_id.to_string(),
            });
        }

        let known_by_document = self
            .documents
            .lock()
            .expect("preview document store poisoned")
            .contains_key(session_id);
        if known_by_document {
            return Ok(());
        }

        let known_by_events = self
            .pending_events
            .lock()
            .expect("preview pending events store poisoned")
            .contains_key(session_id);
        if known_by_events {
            return Ok(());
        }

        let known_by_view_updates = self
            .pending_view_updates
            .lock()
            .expect("preview pending view updates store poisoned")
            .contains_key(session_id);
        if known_by_view_updates {
            return Ok(());
        }

        Err(EventsSessionError::Unknown {
            session_id: session_id.to_string(),
        })
    }
}

impl PreviewTransportPort for LoopbackPreviewTransport {
    fn publish_pdf(&self, session_id: &str, pdf_bytes: &[u8]) -> Result<(), String> {
        self.ensure_events_session_registered(session_id);
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

    fn publish_revision_event(&self, event: &TransportRevisionEvent) -> Result<(), String> {
        self.check_events_session(&event.session_id)
            .map_err(|error| format!("failed to publish preview revision event: {error:?}"))?;

        let delivered_to_subscriber = {
            let mut subscribers = self.event_subscribers.lock().map_err(|_| {
                "failed to acquire preview transport event subscribers lock".to_string()
            })?;
            if let Some(streams) = subscribers.get_mut(&event.session_id) {
                let mut active_streams = Vec::with_capacity(streams.len());
                let mut delivered = false;
                for mut subscriber in streams.drain(..) {
                    if Self::revision_event_payload(event)
                        .and_then(|payload| {
                            ws_write_text(&mut subscriber, &payload).map_err(|error| {
                                format!("failed to write preview websocket frame: {error}")
                            })
                        })
                        .is_ok()
                    {
                        delivered = true;
                        active_streams.push(subscriber);
                    }
                }
                *streams = active_streams;
                delivered
            } else {
                false
            }
        };

        if !delivered_to_subscriber {
            let mut pending_events = self.pending_events.lock().map_err(|_| {
                "failed to acquire preview transport pending events lock".to_string()
            })?;
            pending_events
                .entry(event.session_id.clone())
                .or_default()
                .push(event.clone());
        }

        tracing::debug!(
            session_id = event.session_id.as_str(),
            revision = event.revision,
            page_count = event.page_count,
            delivered_to_subscriber,
            "preview revision event queued in loopback transport"
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
        self.ensure_events_session_registered(session_id);
        format!("{}/{session_id}/events", self.ws_base())
    }

    fn check_events_session(&self, session_id: &str) -> Result<(), EventsSessionError> {
        self.events_session_error(session_id)
    }

    fn submit_view_update(
        &self,
        session_id: &str,
        update: &TransportViewStateUpdate,
    ) -> Result<(), EventsSessionError> {
        self.check_events_session(session_id)?;

        let mut pending_view_updates = self
            .pending_view_updates
            .lock()
            .expect("preview pending view updates store poisoned");
        pending_view_updates
            .entry(session_id.to_string())
            .or_default()
            .push(update.clone());

        tracing::debug!(
            session_id,
            page_number = update.page_number,
            zoom = update.zoom,
            viewport_offset_y = update.viewport_offset_y,
            "preview view-state update queued in loopback transport"
        );

        Ok(())
    }

    fn take_pending_events(
        &self,
        session_id: &str,
    ) -> Result<Vec<TransportRevisionEvent>, EventsSessionError> {
        self.check_events_session(session_id)?;

        let mut pending_events = self
            .pending_events
            .lock()
            .expect("preview pending events store poisoned");
        let events = pending_events.entry(session_id.to_string()).or_default();
        Ok(std::mem::take(events))
    }

    fn take_pending_view_updates(
        &self,
        session_id: &str,
    ) -> Result<Vec<TransportViewStateUpdate>, EventsSessionError> {
        self.check_events_session(session_id)?;

        let mut pending_view_updates = self
            .pending_view_updates
            .lock()
            .expect("preview pending view updates store poisoned");
        let updates = pending_view_updates
            .entry(session_id.to_string())
            .or_default();
        Ok(std::mem::take(updates))
    }

    fn notify_session_invalidated(&self, session_id: &str) {
        let mut invalidated = self
            .invalidated_sessions
            .lock()
            .expect("preview invalidated sessions store poisoned");
        invalidated.insert(session_id.to_string());
        drop(invalidated);

        self.pending_events
            .lock()
            .expect("preview pending events store poisoned")
            .remove(session_id);
        self.close_event_subscribers(session_id);
        self.pending_view_updates
            .lock()
            .expect("preview pending view updates store poisoned")
            .remove(session_id);

        let mut documents = self
            .documents
            .lock()
            .expect("preview document store poisoned");
        documents.remove(session_id);

        tracing::info!(
            session_id,
            "preview session marked as invalidated in transport"
        );
    }
}

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bit_len = (data.len() as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for block in padded.chunks_exact(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut result = [0u8; 20];
    for (i, val) in h.iter().enumerate() {
        result[i * 4..i * 4 + 4].copy_from_slice(&val.to_be_bytes());
    }
    result
}

fn base64_encode(input: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            CHARS[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn ws_accept_key(client_key: &str) -> String {
    let mut input = client_key.to_string();
    input.push_str("258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64_encode(&sha1_digest(input.as_bytes()))
}

fn ws_write_text(writer: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len();
    writer.write_all(&[0x81])?;
    if len < 126 {
        writer.write_all(&[len as u8])?;
    } else if len < 65536 {
        writer.write_all(&[126])?;
        writer.write_all(&(len as u16).to_be_bytes())?;
    } else {
        writer.write_all(&[127])?;
        writer.write_all(&(len as u64).to_be_bytes())?;
    }
    writer.write_all(payload)?;
    writer.flush()
}

fn ws_write_close(writer: &mut impl Write, code: u16) -> std::io::Result<()> {
    let payload = code.to_be_bytes();
    writer.write_all(&[0x88, 2])?;
    writer.write_all(&payload)?;
    writer.flush()
}

fn ws_write_pong(writer: &mut impl Write, payload: &[u8]) -> std::io::Result<()> {
    let len = payload.len();
    writer.write_all(&[0x8A])?;
    if len < 126 {
        writer.write_all(&[len as u8])?;
    } else {
        writer.write_all(&[126])?;
        writer.write_all(&(len as u16).to_be_bytes())?;
    }
    writer.write_all(payload)?;
    writer.flush()
}

const WS_OP_TEXT: u8 = 1;
const WS_OP_CLOSE: u8 = 8;
const WS_OP_PING: u8 = 9;

fn ws_read_frame(reader: &mut impl Read) -> Result<(u8, Vec<u8>), String> {
    let mut head = [0u8; 2];
    reader
        .read_exact(&mut head)
        .map_err(|e| format!("ws read head: {e}"))?;
    if (head[0] & 0x80) == 0 {
        return Err("ws fragmented frames are not supported".to_string());
    }
    let opcode = head[0] & 0x0F;
    let masked = (head[1] & 0x80) != 0;
    let len1 = (head[1] & 0x7F) as usize;
    let payload_len = if len1 < 126 {
        len1
    } else if len1 == 126 {
        let mut b = [0u8; 2];
        reader
            .read_exact(&mut b)
            .map_err(|e| format!("ws read len16: {e}"))?;
        u16::from_be_bytes(b) as usize
    } else {
        let mut b = [0u8; 8];
        reader
            .read_exact(&mut b)
            .map_err(|e| format!("ws read len64: {e}"))?;
        usize::try_from(u64::from_be_bytes(b))
            .map_err(|_| "ws payload too large for platform".to_string())?
    };
    if !masked {
        return Err("ws client frame must be masked".to_string());
    }

    let mut mask = [0u8; 4];
    reader
        .read_exact(&mut mask)
        .map_err(|e| format!("ws read mask: {e}"))?;
    let mut payload = vec![0u8; payload_len];
    reader
        .read_exact(&mut payload)
        .map_err(|e| format!("ws read payload: {e}"))?;
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % mask.len()];
    }
    Ok((opcode, payload))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use ferritex_application::ports::{
        EventsSessionError, PreviewTransportPort, TransportRevisionEvent, TransportViewStateUpdate,
    };
    use serde_json::Value;

    use super::{
        ws_accept_key, DocumentLookupResult, LoopbackPreviewTransport, WS_OP_CLOSE, WS_OP_PING,
        WS_OP_TEXT,
    };

    fn revision_event(
        session_id: &str,
        revision: u64,
        page_count: usize,
    ) -> TransportRevisionEvent {
        TransportRevisionEvent {
            session_id: session_id.to_string(),
            target_input: "chapter.tex".to_string(),
            target_jobname: "chapter".to_string(),
            revision,
            page_count,
        }
    }

    fn view_update(
        page_number: usize,
        zoom: f64,
        viewport_offset_y: f64,
    ) -> TransportViewStateUpdate {
        TransportViewStateUpdate {
            page_number,
            zoom,
            viewport_offset_y,
        }
    }

    #[test]
    fn publish_and_serve_returns_pdf_with_no_store_header() {
        let transport = LoopbackPreviewTransport::new();
        let pdf = b"%PDF-1.4\nhello\n";

        transport
            .publish_pdf("preview-session-1", pdf)
            .expect("publish pdf");

        let result = transport.serve_document("preview-session-1");

        match result {
            DocumentLookupResult::Found(response) => {
                assert_eq!(response.bytes, pdf);
                assert_eq!(
                    response.cache_control,
                    "no-store, no-cache, must-revalidate"
                );
            }
            other => panic!("expected Found, got {:?}", other),
        }
    }

    #[test]
    fn serve_unknown_session_returns_unknown() {
        let transport = LoopbackPreviewTransport::new();

        assert_eq!(
            transport.serve_document("missing-session"),
            DocumentLookupResult::Unknown
        );
    }

    #[test]
    fn serve_invalidated_session_returns_invalidated() {
        let transport = LoopbackPreviewTransport::new();
        let pdf = b"%PDF-1.4\nhello\n";

        transport
            .publish_pdf("preview-session-1", pdf)
            .expect("publish pdf");
        transport.notify_session_invalidated("preview-session-1");

        assert_eq!(
            transport.serve_document("preview-session-1"),
            DocumentLookupResult::Invalidated
        );
    }

    #[test]
    fn invalidated_session_is_distinct_from_unknown() {
        let transport = LoopbackPreviewTransport::new();

        assert_eq!(
            transport.serve_document("never-created"),
            DocumentLookupResult::Unknown
        );

        transport.notify_session_invalidated("explicitly-invalidated");
        assert_eq!(
            transport.serve_document("explicitly-invalidated"),
            DocumentLookupResult::Invalidated
        );
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
    fn serve_returns_410_for_invalidated_session() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let pdf = b"%PDF-1.4\nhello\n";

        transport
            .publish_pdf("preview-session-1", pdf)
            .expect("publish pdf");
        transport.notify_session_invalidated("preview-session-1");
        transport.start_background();

        let response = issue_get_request(transport.port(), "/preview/preview-session-1/document");

        assert!(response.starts_with(b"HTTP/1.1 410 Gone\r\n"));
        let (_, body) = split_http_response(&response);
        let body_str = std::str::from_utf8(body).expect("response body utf-8");
        assert!(body_str.contains("session_expired"));
        assert!(body_str.contains("preview-session-1"));
        assert!(body_str.contains("POST /preview/session"));
    }

    #[test]
    fn check_events_session_returns_ok_for_registered_session() {
        let transport = LoopbackPreviewTransport::new();
        let session_id = "preview-session-1";

        let _ = transport.events_url(session_id);

        assert_eq!(transport.check_events_session(session_id), Ok(()));
    }

    #[test]
    fn events_url_uses_ws_scheme() {
        let transport = LoopbackPreviewTransport::new();

        assert_eq!(
            transport.events_url("preview-session-1"),
            "ws://127.0.0.1/preview/preview-session-1/events"
        );
    }

    #[test]
    fn check_events_session_returns_unknown_for_unregistered_session() {
        let transport = LoopbackPreviewTransport::new();

        assert_eq!(
            transport.check_events_session("missing-session"),
            Err(EventsSessionError::Unknown {
                session_id: "missing-session".to_string(),
            })
        );
    }

    #[test]
    fn check_events_session_returns_expired_after_invalidation() {
        let transport = LoopbackPreviewTransport::new();
        let session_id = "preview-session-1";

        let _ = transport.events_url(session_id);
        transport.notify_session_invalidated(session_id);

        assert_eq!(
            transport.check_events_session(session_id),
            Err(EventsSessionError::Expired {
                session_id: session_id.to_string(),
            })
        );
    }

    #[test]
    fn publish_revision_event_queues_event_for_active_session() {
        let transport = LoopbackPreviewTransport::new();
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        let event = revision_event(session_id, 1, 4);

        transport
            .publish_revision_event(&event)
            .expect("publish revision event");

        assert_eq!(
            transport
                .take_pending_events(session_id)
                .expect("take pending events"),
            vec![event]
        );
    }

    #[test]
    fn publish_revision_event_returns_error_for_unknown_session() {
        let transport = LoopbackPreviewTransport::new();
        let event = revision_event("missing-session", 1, 4);

        let error = transport
            .publish_revision_event(&event)
            .expect_err("unknown session");

        assert!(error.contains("Unknown"));
    }

    #[test]
    fn take_pending_events_drains_revision_queue() {
        let transport = LoopbackPreviewTransport::new();
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        let first = revision_event(session_id, 1, 3);
        let second = revision_event(session_id, 2, 5);

        transport
            .publish_revision_event(&first)
            .expect("publish first revision");
        transport
            .publish_revision_event(&second)
            .expect("publish second revision");

        assert_eq!(
            transport
                .take_pending_events(session_id)
                .expect("take first batch"),
            vec![first, second]
        );
        assert_eq!(
            transport
                .take_pending_events(session_id)
                .expect("take second batch"),
            Vec::<TransportRevisionEvent>::new()
        );
    }

    #[test]
    fn submit_view_update_queues_update_for_active_session() {
        let transport = LoopbackPreviewTransport::new();
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        let update = view_update(7, 1.5, 140.0);

        transport
            .submit_view_update(session_id, &update)
            .expect("submit view update");

        assert_eq!(
            transport
                .take_pending_view_updates(session_id)
                .expect("take pending view updates"),
            vec![update]
        );
    }

    #[test]
    fn take_pending_view_updates_drains_update_queue() {
        let transport = LoopbackPreviewTransport::new();
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        let first = view_update(3, 1.1, 10.0);
        let second = view_update(5, 1.8, 90.0);

        transport
            .submit_view_update(session_id, &first)
            .expect("submit first view update");
        transport
            .submit_view_update(session_id, &second)
            .expect("submit second view update");

        assert_eq!(
            transport
                .take_pending_view_updates(session_id)
                .expect("take first batch"),
            vec![first, second]
        );
        assert_eq!(
            transport
                .take_pending_view_updates(session_id)
                .expect("take second batch"),
            Vec::<TransportViewStateUpdate>::new()
        );
    }

    #[test]
    fn invalidation_clears_pending_events_and_view_updates() {
        let transport = LoopbackPreviewTransport::new();
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        transport
            .publish_revision_event(&revision_event(session_id, 1, 4))
            .expect("publish revision event");
        transport
            .submit_view_update(session_id, &view_update(2, 1.2, 50.0))
            .expect("submit view update");

        transport.notify_session_invalidated(session_id);

        assert!(transport
            .pending_events
            .lock()
            .expect("pending events lock")
            .get(session_id)
            .is_none());
        assert!(transport
            .pending_view_updates
            .lock()
            .expect("pending view updates lock")
            .get(session_id)
            .is_none());
        assert_eq!(
            transport.take_pending_events(session_id),
            Err(EventsSessionError::Expired {
                session_id: session_id.to_string(),
            })
        );
        assert_eq!(
            transport.take_pending_view_updates(session_id),
            Err(EventsSessionError::Expired {
                session_id: session_id.to_string(),
            })
        );
    }

    #[test]
    fn events_websocket_upgrade_returns_101_and_accept_header() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        transport.start_background();

        let (_stream, response) =
            open_websocket(transport.port(), "/preview/preview-session-1/events");
        let (head, body) = split_http_response(&response);
        let head = std::str::from_utf8(head).expect("websocket handshake utf-8");

        assert!(head.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
        assert!(head.contains("Upgrade: websocket\r\n"));
        assert!(head.contains("Connection: Upgrade\r\n"));
        assert!(head.contains(&format!(
            "Sec-WebSocket-Accept: {}\r\n",
            ws_accept_key("dGhlIHNhbXBsZSBub25jZQ==")
        )));
        assert!(body.is_empty());
    }

    #[test]
    fn events_websocket_without_upgrade_returns_400() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        transport.start_background();

        let response = issue_get_request(transport.port(), "/preview/preview-session-1/events");

        assert!(response.starts_with(b"HTTP/1.1 400 Bad Request\r\n\r\n"));
    }

    #[test]
    fn events_websocket_returns_404_for_unknown_session() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        transport.start_background();

        let response = issue_websocket_upgrade_request_once(
            transport.port(),
            "/preview/missing-session/events",
        );

        assert!(response.starts_with(b"HTTP/1.1 404 Not Found\r\n\r\n"));
    }

    #[test]
    fn events_websocket_returns_410_for_invalidated_session() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        transport.notify_session_invalidated(session_id);
        transport.start_background();

        let response = issue_websocket_upgrade_request_once(
            transport.port(),
            "/preview/preview-session-1/events",
        );

        assert!(response.starts_with(b"HTTP/1.1 410 Gone\r\n"));
    }

    #[test]
    fn events_websocket_drains_pending_revision_events_on_connect() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        transport
            .publish_revision_event(&revision_event(session_id, 2, 5))
            .expect("queue revision event");
        transport.start_background();

        let (_stream, response) =
            open_websocket(transport.port(), "/preview/preview-session-1/events");
        let (_, body) = split_http_response(&response);
        let frames = parse_server_ws_frames(body);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, WS_OP_TEXT);
        let payload: Value =
            serde_json::from_slice(&frames[0].1).expect("parse revision event payload");
        assert_eq!(payload["type"], "revision");
        assert_eq!(payload["session_id"], session_id);
        assert_eq!(payload["revision"], 2);
        assert_eq!(payload["page_count"], 5);
        assert_eq!(
            transport
                .take_pending_events(session_id)
                .expect("pending queue after drain"),
            Vec::<TransportRevisionEvent>::new()
        );
    }

    #[test]
    fn publish_revision_event_pushes_to_connected_websocket() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        transport.start_background();

        let (mut stream, response) =
            open_websocket(transport.port(), "/preview/preview-session-1/events");
        let (_, body) = split_http_response(&response);
        assert!(body.is_empty());
        wait_for_subscriber(&transport, session_id);

        transport
            .publish_revision_event(&revision_event(session_id, 3, 7))
            .expect("push revision event");

        let pushed = read_server_ws_frame(&mut stream);
        assert_eq!(pushed.0, WS_OP_TEXT);
        let payload: Value =
            serde_json::from_slice(&pushed.1).expect("parse live revision payload");
        assert_eq!(payload["type"], "revision");
        assert_eq!(payload["revision"], 3);
        assert_eq!(payload["page_count"], 7);
        assert_eq!(
            transport
                .take_pending_events(session_id)
                .expect("pending queue after live push"),
            Vec::<TransportRevisionEvent>::new()
        );
    }

    #[test]
    fn events_websocket_text_frame_accepts_view_state_and_stores_update() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        transport.start_background();

        let (mut stream, _response) =
            open_websocket(transport.port(), "/preview/preview-session-1/events");
        write_masked_ws_frame(
            &mut stream,
            WS_OP_TEXT,
            br#"{"page_number":7,"zoom":1.5,"viewport_offset_y":140.0}"#,
        );
        wait_for_view_updates(&transport, session_id, 1);

        assert_eq!(
            transport
                .take_pending_view_updates(session_id)
                .expect("stored view updates"),
            vec![view_update(7, 1.5, 140.0)]
        );
    }

    #[test]
    fn ws_view_state_reaches_registered_handler() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let session_id = "preview-session-1";
        transport
            .publish_pdf(session_id, b"%PDF-1.4\nhello\n")
            .expect("publish pdf");

        let received = Arc::new(Mutex::new(Vec::<(String, TransportViewStateUpdate)>::new()));
        let received_clone = Arc::clone(&received);
        transport.set_view_state_handler(Arc::new(move |sid, update| {
            received_clone
                .lock()
                .expect("received updates lock")
                .push((sid.to_string(), update.clone()));
        }));

        transport.start_background();

        let (mut stream, _response) =
            open_websocket(transport.port(), "/preview/preview-session-1/events");
        write_masked_ws_frame(
            &mut stream,
            WS_OP_TEXT,
            br#"{"page_number":7,"zoom":1.5,"viewport_offset_y":120.0}"#,
        );

        std::thread::sleep(Duration::from_millis(100));

        let updates = received.lock().expect("received updates lock");
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, session_id);
        assert_eq!(updates[0].1.page_number, 7);
        assert!((updates[0].1.zoom - 1.5).abs() < f64::EPSILON);
        assert!((updates[0].1.viewport_offset_y - 120.0).abs() < f64::EPSILON);
    }

    #[test]
    fn events_websocket_ping_receives_pong() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        transport.start_background();

        let (mut stream, _response) =
            open_websocket(transport.port(), "/preview/preview-session-1/events");
        write_masked_ws_frame(&mut stream, WS_OP_PING, b"ping");

        let frame = read_server_ws_frame(&mut stream);
        assert_eq!(frame.0, 0xA);
        assert_eq!(frame.1, b"ping");
    }

    #[test]
    fn notify_session_invalidated_cleans_up_connected_websocket_subscribers() {
        let transport =
            Arc::new(LoopbackPreviewTransport::bind().expect("bind loopback transport"));
        let session_id = "preview-session-1";
        let _ = transport.events_url(session_id);
        transport.start_background();

        let (mut stream, _response) =
            open_websocket(transport.port(), "/preview/preview-session-1/events");
        wait_for_subscriber(&transport, session_id);

        transport.notify_session_invalidated(session_id);

        let close = read_server_ws_frame(&mut stream);
        assert_eq!(close.0, WS_OP_CLOSE);
        assert_eq!(close.1, 1001u16.to_be_bytes());
        assert!(transport
            .event_subscribers
            .lock()
            .expect("event subscribers lock")
            .get(session_id)
            .is_none());
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

    fn issue_websocket_upgrade_request_once(port: u16, path: &str) -> Vec<u8> {
        let mut stream =
            TcpStream::connect(("127.0.0.1", port)).expect("connect to loopback preview server");
        write_websocket_handshake(&mut stream, port, path);
        stream.shutdown(Shutdown::Write).expect("shutdown write");

        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        response
    }

    fn open_websocket(port: u16, path: &str) -> (TcpStream, Vec<u8>) {
        let mut stream =
            TcpStream::connect(("127.0.0.1", port)).expect("connect to loopback preview server");
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("set read timeout");
        write_websocket_handshake(&mut stream, port, path);
        let response = read_available(&mut stream);
        (stream, response)
    }

    fn write_websocket_handshake(stream: &mut TcpStream, port: u16, path: &str) {
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        )
        .expect("write websocket handshake");
    }

    fn read_available(stream: &mut TcpStream) -> Vec<u8> {
        let mut response = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return response,
                Ok(read) => response.extend_from_slice(&chunk[..read]),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return response;
                }
                Err(error) => panic!("read available response: {error}"),
            }
        }
    }

    fn parse_server_ws_frames(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut cursor = std::io::Cursor::new(bytes);
        let mut frames = Vec::new();
        while (cursor.position() as usize) < bytes.len() {
            frames.push(read_server_ws_frame_from_reader(&mut cursor));
        }
        frames
    }

    fn read_server_ws_frame(stream: &mut TcpStream) -> (u8, Vec<u8>) {
        read_server_ws_frame_from_reader(stream)
    }

    fn read_server_ws_frame_from_reader(reader: &mut impl Read) -> (u8, Vec<u8>) {
        let mut head = [0u8; 2];
        reader.read_exact(&mut head).expect("read ws frame head");
        let opcode = head[0] & 0x0F;
        let len1 = (head[1] & 0x7F) as usize;
        assert_eq!(head[1] & 0x80, 0, "server frames must not be masked");
        let payload_len = if len1 < 126 {
            len1
        } else if len1 == 126 {
            let mut b = [0u8; 2];
            reader.read_exact(&mut b).expect("read ws len16");
            u16::from_be_bytes(b) as usize
        } else {
            let mut b = [0u8; 8];
            reader.read_exact(&mut b).expect("read ws len64");
            usize::try_from(u64::from_be_bytes(b)).expect("payload fits usize")
        };
        let mut payload = vec![0u8; payload_len];
        reader
            .read_exact(&mut payload)
            .expect("read ws frame payload");
        (opcode, payload)
    }

    fn write_masked_ws_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) {
        let mask = [0x11, 0x22, 0x33, 0x44];
        let len = payload.len();
        let mut header = vec![0x80 | opcode];
        if len < 126 {
            header.push(0x80 | len as u8);
        } else if len < 65536 {
            header.push(0x80 | 126);
            header.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            header.push(0x80 | 127);
            header.extend_from_slice(&(len as u64).to_be_bytes());
        }
        header.extend_from_slice(&mask);
        stream.write_all(&header).expect("write ws frame header");
        let masked_payload: Vec<u8> = payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()])
            .collect();
        stream
            .write_all(&masked_payload)
            .expect("write ws frame payload");
        stream.flush().expect("flush ws frame");
    }

    fn wait_for_subscriber(transport: &LoopbackPreviewTransport, session_id: &str) {
        for _ in 0..20 {
            if transport
                .event_subscribers
                .lock()
                .expect("event subscribers lock")
                .get(session_id)
                .is_some_and(|subscribers| !subscribers.is_empty())
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        panic!("subscriber was not registered for session {session_id}");
    }

    fn wait_for_view_updates(
        transport: &LoopbackPreviewTransport,
        session_id: &str,
        expected_len: usize,
    ) {
        for _ in 0..20 {
            if transport
                .pending_view_updates
                .lock()
                .expect("pending view updates lock")
                .get(session_id)
                .is_some_and(|updates| updates.len() == expected_len)
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        panic!("view updates were not stored for session {session_id}");
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
