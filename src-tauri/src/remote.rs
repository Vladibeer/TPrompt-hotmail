//! Малък HTTP сървър за дистанционно управление.
//! Само стандартна библиотека. Две употреби на един и същ сървър:
//!  1. Уеб дистанционно: телефонът отваря страница в браузъра (GET /) и управлява четенето на компютъра.
//!  2. Приложението (Android) праща текст, настройки и команди (POST /api/rx/send) и чете състоянието на
//!     дисплея на компютъра (GET /api/rx/state). Компютърът е втори екран на приложението.
//! Всичко се пази с таен код в адреса. Отговорите имат CORS, за да работи заявката от приложението.

use std::collections::hash_map::RandomState;
use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_CMDS: usize = 64;
const MAX_CMD_BODY: usize = 1024;
const MAX_RX_BODY: usize = 8 * 1024 * 1024;
const MAX_RX_QUEUE: usize = 512;
const MAX_RX_BYTES: usize = 32 * 1024 * 1024;
const FIRST_PORT: u16 = 8723;

#[derive(Clone, Debug)]
pub struct Info {
    pub ip: String,
    pub port: u16,
    pub token: String,
    pub url: String,
}

struct Inner {
    state: String,
    content: String,
    cmds: VecDeque<String>,
    /// Съобщения от приложението към дисплея (текст, настройки, команди), в реда на пристигане.
    rx: VecDeque<String>,
    rx_bytes: usize,
    /// Последното състояние на дисплея (JSON) за приложението.
    rx_state: String,
    /// Съобщения от компютъра към приложението (когато телефонът е вторият екран за четене).
    tx: VecDeque<String>,
    tx_bytes: usize,
    seen_web: Option<Instant>,
    seen_rx: Option<Instant>,
    /// Заявка от компютъра към приложението: "view" = започни да четеш на този екран. Брояч, за да се вижда нова заявка.
    want: String,
    want_seq: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Status {
    pub running: bool,
    /// Преди колко милисекунди е питал уеб дистанционното (None = никога).
    pub web_ago_ms: Option<u64>,
    /// Преди колко милисекунди е питало приложението (None = никога).
    pub rx_ago_ms: Option<u64>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub struct Remote {
    html: &'static str,
    inner: Mutex<Inner>,
    token: Mutex<String>,
    running: AtomicBool,
    port: AtomicU16,
}

fn random_token() -> String {
    let mut out = String::new();
    while out.len() < 12 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(12);
    out
}

/// IP адрес на компютъра в локалната мрежа (не изпраща нищо по мрежата).
pub fn local_ip() -> String {
    let probe = || -> Option<IpAddr> {
        let s = UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect("192.0.2.1:9").ok()?;
        Some(s.local_addr().ok()?.ip())
    };
    match probe() {
        Some(ip) if !ip.is_loopback() && !ip.is_unspecified() => ip.to_string(),
        _ => "127.0.0.1".to_string(),
    }
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    for part in query.split('&') {
        let mut kv = part.splitn(2, '=');
        if kv.next() == Some(key) {
            return Some(kv.next().unwrap_or(""));
        }
    }
    None
}

fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nAccess-Control-Allow-Private-Network: true\r\nAccess-Control-Max-Age: 600\r\nConnection: close\r\n\r\n",
        status,
        ctype,
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

impl Remote {
    pub fn new(html: &'static str) -> Arc<Remote> {
        Arc::new(Remote {
            html,
            inner: Mutex::new(Inner {
                state: "{}".to_string(),
                content: "{}".to_string(),
                cmds: VecDeque::new(),
                rx: VecDeque::new(),
                rx_bytes: 0,
                rx_state: String::new(),
                tx: VecDeque::new(),
                tx_bytes: 0,
                seen_web: None,
                seen_rx: None,
                want: String::new(),
                want_seq: 0,
            }),
            token: Mutex::new(random_token()),
            running: AtomicBool::new(false),
            port: AtomicU16::new(0),
        })
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn info(&self) -> Option<Info> {
        if !self.is_running() {
            return None;
        }
        let ip = local_ip();
        let port = self.port.load(Ordering::SeqCst);
        let token = self.token.lock().unwrap().clone();
        let url = format!("http://{}:{}/?t={}", ip, port, token);
        Some(Info { ip, port, token, url })
    }

    pub fn new_token(&self) {
        *self.token.lock().unwrap() = random_token();
        let mut g = self.inner.lock().unwrap();
        g.cmds.clear();
        g.rx.clear();
        g.rx_bytes = 0;
        g.rx_state.clear();
        g.tx.clear();
        g.tx_bytes = 0;
        g.want.clear();
        g.seen_web = None;
        g.seen_rx = None;
    }

    /// Компютърът слага съобщение за приложението (то го взема със следващата си заявка).
    pub fn push_tx(&self, msg: String) {
        if !self.is_running() || !msg.starts_with('{') {
            return;
        }
        let mut g = self.inner.lock().unwrap();
        g.tx_bytes += msg.len();
        g.tx.push_back(msg);
        while g.tx.len() > MAX_RX_QUEUE || g.tx_bytes > MAX_RX_BYTES {
            match g.tx.pop_front() {
                Some(old) => g.tx_bytes = g.tx_bytes.saturating_sub(old.len()),
                None => break,
            }
        }
    }

    /// Компютърът моли приложението да започне (view = true) или да спре да чете на своя екран.
    /// Приложението го вижда със следващата си заявка за състояние.
    pub fn set_want(&self, view: bool) {
        if !self.is_running() {
            return;
        }
        let mut g = self.inner.lock().unwrap();
        g.want = if view { "view".to_string() } else { String::new() };
        g.want_seq += 1;
    }

    pub fn clear_tx(&self) {
        let mut g = self.inner.lock().unwrap();
        g.tx.clear();
        g.tx_bytes = 0;
    }

    /// Връща запазен код, за да не се сменя QR кодът при всяко пускане. Невалиден код се пропуска.
    pub fn set_token(&self, t: &str) -> bool {
        let ok = (6..=64).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if ok {
            *self.token.lock().unwrap() = t.to_string();
        }
        ok
    }

    pub fn status(&self) -> Status {
        let g = self.inner.lock().unwrap();
        let ago = |t: Option<Instant>| t.map(|i| i.elapsed().as_millis() as u64);
        Status { running: self.is_running(), web_ago_ms: ago(g.seen_web), rx_ago_ms: ago(g.seen_rx) }
    }

    /// Приложението на компютъра взема съобщенията, пратени от приложението на телефона.
    pub fn rx_drain(&self) -> (Option<u64>, Vec<String>) {
        if !self.is_running() {
            return (None, Vec::new());
        }
        let mut g = self.inner.lock().unwrap();
        let ago = g.seen_rx.map(|i| i.elapsed().as_millis() as u64);
        g.rx_bytes = 0;
        (ago, g.rx.drain(..).collect())
    }

    /// Приложението на компютъра публикува състоянието на дисплея (позиция, пауза/старт, време).
    pub fn set_rx_state(&self, state: String) {
        if self.is_running() {
            self.inner.lock().unwrap().rx_state = state;
        }
    }

    pub fn start(self: &Arc<Self>) -> Result<Info, String> {
        if let Some(i) = self.info() {
            return Ok(i);
        }
        let mut listener = None;
        for p in FIRST_PORT..FIRST_PORT + 20 {
            if let Ok(l) = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), p)) {
                self.port.store(p, Ordering::SeqCst);
                listener = Some(l);
                break;
            }
        }
        let listener = match listener {
            Some(l) => l,
            None => return Err("Няма свободен порт за дистанционното.".to_string()),
        };
        self.running.store(true, Ordering::SeqCst);
        let me = Arc::clone(self);
        thread::spawn(move || {
            for conn in listener.incoming() {
                if !me.running.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = conn {
                    let me2 = Arc::clone(&me);
                    thread::spawn(move || me2.handle(stream));
                }
            }
        });
        self.info().ok_or_else(|| "Сървърът не стартира.".to_string())
    }

    pub fn stop(&self) {
        if !self.running.swap(false, Ordering::SeqCst) {
            return;
        }
        // събужда accept(), за да излезе нишката
        let p = self.port.load(Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(
            &SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), p),
            Duration::from_millis(500),
        );
        let mut g = self.inner.lock().unwrap();
        g.cmds.clear();
        g.state = "{}".to_string();
        g.content = "{}".to_string();
        g.rx.clear();
        g.rx_bytes = 0;
        g.rx_state.clear();
        g.tx.clear();
        g.tx_bytes = 0;
        g.want.clear();
        g.seen_web = None;
        g.seen_rx = None;
    }

    /// Текст и стил за превюто на телефона (променя се рядко, затова е отделно от състоянието).
    pub fn set_content(&self, content: String) {
        if self.is_running() {
            self.inner.lock().unwrap().content = content;
        }
    }

    /// Приложението праща текущото състояние и получава чакащите команди.
    pub fn sync(&self, state: String) -> Vec<String> {
        if !self.is_running() {
            return Vec::new();
        }
        let mut g = self.inner.lock().unwrap();
        g.state = state;
        g.cmds.drain(..).collect()
    }

    fn handle(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
        let mut buf: Vec<u8> = Vec::with_capacity(2048);
        let mut chunk = [0u8; 4096];
        let head_end;
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => return,
            }
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                head_end = p + 4;
                break;
            }
            if buf.len() > 8192 {
                respond(&mut stream, "431 Request Header Fields Too Large", "text/plain", b"too large");
                return;
            }
        }
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let first = lines.next().unwrap_or("");
        let mut parts = first.split(' ');
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("/");
        let mut content_len = 0usize;
        for l in lines {
            if let Some((k, v)) = l.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-length") {
                    content_len = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p, q),
            None => (target, ""),
        };

        // Предварителна заявка на браузъра (CORS): без код, без тяло
        if method == "OPTIONS" {
            respond(&mut stream, "204 No Content", "text/plain", b"");
            return;
        }

        let limit = if path == "/api/rx/send" { MAX_RX_BODY } else { MAX_CMD_BODY };
        if content_len > limit {
            respond(&mut stream, "413 Payload Too Large", "text/plain", b"too large");
            return;
        }
        let mut body: Vec<u8> = buf[head_end..].to_vec();
        while body.len() < content_len {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => body.extend_from_slice(&chunk[..n]),
                Err(_) => return,
            }
        }
        body.truncate(content_len);

        if method == "GET" && path == "/" {
            respond(&mut stream, "200 OK", "text/html; charset=utf-8", self.html.as_bytes());
            return;
        }

        let protected = matches!(
            (method, path),
            ("GET", "/api/state") | ("GET", "/api/content") | ("POST", "/api/cmd") | ("GET", "/api/rx/state") | ("POST", "/api/rx/send")
        );
        if !protected {
            respond(&mut stream, "404 Not Found", "text/plain", b"not found");
            return;
        }
        let ok = {
            let tok = self.token.lock().unwrap();
            query_param(query, "t").map(|t| t == tok.as_str()).unwrap_or(false)
        };
        if !ok {
            respond(&mut stream, "403 Forbidden", "application/json", b"{\"error\":\"bad token\"}");
            return;
        }

        match (method, path) {
            ("GET", "/api/state") | ("GET", "/api/content") => {
                let s = {
                    let mut g = self.inner.lock().unwrap();
                    g.seen_web = Some(Instant::now());
                    if path == "/api/content" { g.content.clone() } else { g.state.clone() }
                };
                respond(&mut stream, "200 OK", "application/json; charset=utf-8", s.as_bytes());
            }
            ("POST", "/api/cmd") => {
                let text = String::from_utf8_lossy(&body).trim().to_string();
                if text.starts_with('{') && text.ends_with('}') {
                    let mut g = self.inner.lock().unwrap();
                    g.seen_web = Some(Instant::now());
                    if g.cmds.len() >= MAX_CMDS {
                        g.cmds.pop_front();
                    }
                    g.cmds.push_back(text);
                    drop(g);
                    respond(&mut stream, "200 OK", "application/json", b"{\"ok\":true}");
                } else {
                    respond(&mut stream, "400 Bad Request", "application/json", b"{\"error\":\"bad body\"}");
                }
            }
            ("GET", "/api/rx/state") => {
                let out = {
                    let mut g = self.inner.lock().unwrap();
                    g.seen_rx = Some(Instant::now());
                    let st = if g.rx_state.is_empty() { "null".to_string() } else { g.rx_state.clone() };
                    // tx=1: приложението иска и съобщенията от компютъра (само един читател ги взема)
                    let tx = if query_param(query, "tx").is_some() {
                        g.tx_bytes = 0;
                        g.tx.drain(..).collect::<Vec<_>>().join(",")
                    } else {
                        String::new()
                    };
                    format!(
                        "{{\"alive\":true,\"now\":{},\"st\":{},\"tx\":[{}],\"want\":\"{}\",\"wseq\":{}}}",
                        now_ms(),
                        st,
                        tx,
                        g.want,
                        g.want_seq
                    )
                };
                respond(&mut stream, "200 OK", "application/json; charset=utf-8", out.as_bytes());
            }
            ("POST", "/api/rx/send") => {
                let text = String::from_utf8_lossy(&body).trim().to_string();
                let obj = text.starts_with('{') && text.ends_with('}');
                let arr = text.starts_with('[') && text.ends_with(']');
                if obj || arr {
                    let mut g = self.inner.lock().unwrap();
                    g.seen_rx = Some(Instant::now());
                    g.rx_bytes += text.len();
                    g.rx.push_back(text);
                    while g.rx.len() > MAX_RX_QUEUE || g.rx_bytes > MAX_RX_BYTES {
                        match g.rx.pop_front() {
                            Some(old) => g.rx_bytes = g.rx_bytes.saturating_sub(old.len()),
                            None => break,
                        }
                    }
                    drop(g);
                    respond(&mut stream, "200 OK", "application/json", b"{\"ok\":true}");
                } else {
                    respond(&mut stream, "400 Bad Request", "application/json", b"{\"error\":\"bad body\"}");
                }
            }
            _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
        }
    }
}
