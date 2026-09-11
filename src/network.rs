use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_OPTION_RECEIVE_RESPONSE_TIMEOUT, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WinHttpSetTimeouts,
};
use windows::core::{Error as WindowsError, PCWSTR};
use zeroize::{Zeroize, Zeroizing};

use crate::AppResult;
use crate::settings::AppSettings;
use crate::wide::{pcwstr, to_wide};

pub const PRIMARY_PROBE_URL: &str = "http://www.msftconnecttest.com/connecttest.txt";
pub const PRIMARY_EXPECTED_BODY: &str = "Microsoft Connect Test";
pub const CAMPUS_LOGIN_PAGE_URL: &str = "http://10.2.5.251/";
pub const PORTAL_ENDPOINT: &str = "http://10.2.5.251:801/eportal/";

pub trait NetworkBackend {
    fn internet_available(&mut self, settings: &AppSettings) -> bool;
    fn authentication_server_reachable(&mut self) -> bool;
    fn login(&mut self, submitted_account: &str, password: &str) -> AppResult<()>;
    fn logout(&mut self) -> AppResult<()>;
}

struct InternetHandle(*mut c_void);

impl InternetHandle {
    fn new(value: *mut c_void, context: &str) -> AppResult<Self> {
        if value.is_null() {
            Err(format!("{context}：{}", WindowsError::from_thread()))
        } else {
            Ok(Self(value))
        }
    }
}

impl Drop for InternetHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            let _ = unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

pub struct WinHttpClient {
    session: InternetHandle,
}

impl WinHttpClient {
    pub fn new() -> AppResult<Self> {
        let agent = to_wide("CampusNetAutoLogin/2.0");
        let session = unsafe {
            WinHttpOpen(
                pcwstr(&agent),
                WINHTTP_ACCESS_TYPE_NO_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        };
        Ok(Self {
            session: InternetHandle::new(session, "无法初始化 Windows HTTP")?,
        })
    }

    fn get(&mut self, url: &str, body_limit: usize, timeout_ms: i32) -> AppResult<HttpResponse> {
        let parsed = ParsedUrl::parse(url)?;
        unsafe {
            WinHttpSetTimeouts(
                self.session.0,
                timeout_ms,
                timeout_ms,
                timeout_ms,
                timeout_ms,
            )
            .map_err(|error| format!("无法设置网络超时：{error}"))?;
        }

        let host = to_wide(&parsed.host);
        let connection = unsafe { WinHttpConnect(self.session.0, pcwstr(&host), parsed.port, 0) };
        let connection = InternetHandle::new(connection, "无法连接目标服务器")?;

        let verb = to_wide("GET");
        let path = Zeroizing::new(to_wide(&parsed.path_and_query));
        let flags = if parsed.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let request = unsafe {
            WinHttpOpenRequest(
                connection.0,
                pcwstr(&verb),
                pcwstr(&path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            )
        };
        let request = InternetHandle::new(request, "无法创建网络请求")?;

        let redirect_policy = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
        let redirect_bytes = unsafe {
            std::slice::from_raw_parts(
                (&redirect_policy as *const u32).cast::<u8>(),
                size_of::<u32>(),
            )
        };
        unsafe {
            WinHttpSetOption(
                Some(request.0.cast_const()),
                WINHTTP_OPTION_REDIRECT_POLICY,
                Some(redirect_bytes),
            )
            .map_err(|error| format!("无法禁用自动跳转：{error}"))?;
            let response_timeout = timeout_ms.max(1) as u32;
            let timeout_bytes = std::slice::from_raw_parts(
                (&response_timeout as *const u32).cast::<u8>(),
                size_of::<u32>(),
            );
            WinHttpSetOption(
                Some(request.0.cast_const()),
                WINHTTP_OPTION_RECEIVE_RESPONSE_TIMEOUT,
                Some(timeout_bytes),
            )
            .map_err(|error| format!("无法设置响应超时：{error}"))?;
            WinHttpSendRequest(request.0, None, None, 0, 0, 0)
                .map_err(|error| format!("无法发送网络请求：{error}"))?;
            WinHttpReceiveResponse(request.0, std::ptr::null_mut())
                .map_err(|error| format!("无法接收网络响应：{error}"))?;
        }

        let mut status_code = 0u32;
        let mut status_size = size_of::<u32>() as u32;
        let mut header_index = 0u32;
        unsafe {
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&mut status_code as *mut u32).cast()),
                &mut status_size,
                &mut header_index,
            )
            .map_err(|error| format!("无法读取 HTTP 状态：{error}"))?;
        }

        // Status-only probes must not wait for even one byte of a slow response body.
        if body_limit == 0 {
            return Ok(HttpResponse {
                status_code,
                body: Vec::new(),
            });
        }

        let mut body = Vec::with_capacity(body_limit.min(4096));
        while body.len() <= body_limit {
            let remaining = body_limit.saturating_add(1).saturating_sub(body.len());
            if remaining == 0 {
                break;
            }
            let mut buffer = [0u8; 512];
            let requested = remaining.min(buffer.len()) as u32;
            let mut read = 0u32;
            unsafe {
                WinHttpReadData(request.0, buffer.as_mut_ptr().cast(), requested, &mut read)
                    .map_err(|error| format!("无法读取网络响应：{error}"))?;
            }
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buffer[..read as usize]);
        }

        Ok(HttpResponse { status_code, body })
    }

    fn login_at(&mut self, url: &str) -> AppResult<()> {
        let response = self
            .get(url, 0, 5000)
            .map_err(|_| "无法连接校园网认证服务器，或登录请求超时。".to_string())?;
        if !(200..300).contains(&response.status_code) {
            return Err(format!("登录请求返回 HTTP {}。", response.status_code));
        }
        Ok(())
    }

    fn logout_at(&mut self, url: &str) -> AppResult<()> {
        let response = self
            .get(url, 4096, 5000)
            .map_err(|_| "无法连接校园网认证服务器，或注销请求超时。".to_string())?;
        if !(200..300).contains(&response.status_code) {
            return Err(format!("注销请求返回 HTTP {}。", response.status_code));
        }
        if response.body.len() > 4096 {
            return Err("认证服务器返回的注销结果过大。".to_string());
        }
        if !parse_logout_success(&response.body) {
            return Err("认证服务器未确认注销成功。".to_string());
        }
        Ok(())
    }
}

impl NetworkBackend for WinHttpClient {
    fn internet_available(&mut self, settings: &AppSettings) -> bool {
        if self
            .get(PRIMARY_PROBE_URL, 128, 3000)
            .ok()
            .is_some_and(|response| {
                response.status_code == 200
                    && response.body.len() <= 128
                    && String::from_utf8_lossy(&response.body).trim_start_matches('\u{feff}')
                        == PRIMARY_EXPECTED_BODY
            })
        {
            return true;
        }

        self.get(&settings.fallback_probe_url, 0, 3000)
            .map(|response| (200..300).contains(&response.status_code))
            .unwrap_or(false)
    }

    fn authentication_server_reachable(&mut self) -> bool {
        self.get(PORTAL_ENDPOINT, 0, 2000).is_ok()
    }

    fn login(&mut self, submitted_account: &str, password: &str) -> AppResult<()> {
        let url = Zeroizing::new(build_login_url(submitted_account, password));
        self.login_at(&url)
    }

    fn logout(&mut self) -> AppResult<()> {
        self.logout_at(&build_logout_url())
    }
}

#[derive(Debug)]
struct HttpResponse {
    status_code: u32,
    body: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedUrl {
    secure: bool,
    host: String,
    port: u16,
    path_and_query: String,
}

impl ParsedUrl {
    fn parse(value: &str) -> AppResult<Self> {
        let value = value.trim();
        if value.chars().any(char::is_control) {
            return Err("网络地址不能包含控制字符。".to_string());
        }
        let bytes = value.as_bytes();
        let (secure, default_port, rest) =
            if bytes.len() >= 8 && bytes[..8].eq_ignore_ascii_case(b"https://") {
                (true, 443u16, &value[8..])
            } else if bytes.len() >= 7 && bytes[..7].eq_ignore_ascii_case(b"http://") {
                (false, 80u16, &value[7..])
            } else {
                return Err("网络地址必须使用 HTTP 或 HTTPS。".to_string());
            };
        if rest.is_empty() || rest.starts_with('/') || rest.contains('@') || rest.contains('#') {
            return Err("网络地址缺少有效主机名。".to_string());
        }

        let (authority, path_and_query) = match rest.find(['/', '?']) {
            Some(index) if rest.as_bytes()[index] == b'?' => {
                (&rest[..index], format!("/{}", &rest[index..]))
            }
            Some(index) => (&rest[..index], rest[index..].to_string()),
            None => (rest, "/".to_string()),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => {
                if host.is_empty()
                    || port.is_empty()
                    || !port.bytes().all(|value| value.is_ascii_digit())
                {
                    return Err("网络地址端口无效。".to_string());
                }
                let port = port
                    .parse::<u16>()
                    .map_err(|_| "网络地址端口无效。".to_string())?;
                if port == 0 {
                    return Err("网络地址端口无效。".to_string());
                }
                (host, port)
            }
            _ => (authority, default_port),
        };
        if host.is_empty()
            || !host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        {
            return Err("网络地址主机名无效。".to_string());
        }
        Ok(Self {
            secure,
            host: host.to_string(),
            port,
            path_and_query,
        })
    }
}

impl Drop for ParsedUrl {
    fn drop(&mut self) {
        // Login query strings contain credentials, including after parsing the URL.
        self.path_and_query.zeroize();
    }
}

pub(crate) fn is_valid_https_url(value: &str) -> bool {
    ParsedUrl::parse(value).is_ok_and(|url| url.secure)
}

pub fn build_login_url(submitted_account: &str, password: &str) -> String {
    let encoded_password = Zeroizing::new(percent_encode(password));
    format!(
        "{PORTAL_ENDPOINT}?c=Portal&a=login&login_method=1&user_account={}&user_password={}",
        percent_encode(submitted_account),
        encoded_password.as_str()
    )
}

pub fn build_logout_url() -> String {
    format!("{PORTAL_ENDPOINT}?c=Portal&a=logout")
}

fn parse_logout_success(response_body: &[u8]) -> bool {
    let body = String::from_utf8_lossy(response_body);
    let Some(object_start) = body.find('{') else {
        return false;
    };
    let Some(object_end) = body.rfind('}') else {
        return false;
    };
    if object_end <= object_start {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&body[object_start..=object_end])
    else {
        return false;
    };
    match value.get("result") {
        Some(serde_json::Value::Number(number)) => number.as_i64() == Some(1),
        Some(serde_json::Value::String(text)) => text == "1" || text.eq_ignore_ascii_case("ok"),
        _ => false,
    }
}

pub fn percent_encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(char::from(*byte));
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                output.push('%');
                output.push(char::from(HEX[(byte >> 4) as usize]));
                output.push(char::from(HEX[(byte & 0x0f) as usize]));
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn read_request_headers(stream: &mut TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut buffer = [0u8; 512];
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "client closed before sending complete headers");
            request.extend_from_slice(&buffer[..read]);
            assert!(request.len() <= 8192, "request headers exceeded test limit");
        }
    }

    #[test]
    fn login_url_percent_encodes_credentials() {
        let url = build_login_url("user name@cmcc", "p@ss word&=");
        assert!(url.contains("user_account=user%20name%40cmcc"));
        assert!(url.contains("user_password=p%40ss%20word%26%3D"));
        assert!(!url.contains("p@ss word&="));
    }

    #[test]
    fn logout_url_and_response_require_explicit_success() {
        assert_eq!(CAMPUS_LOGIN_PAGE_URL, "http://10.2.5.251/");
        assert_eq!(
            build_logout_url(),
            "http://10.2.5.251:801/eportal/?c=Portal&a=logout"
        );
        assert!(parse_logout_success(br#"{"result":1}"#));
        assert!(parse_logout_success(br#"dr1003({"result":"ok"})"#));
        assert!(parse_logout_success(br#"{"result":"1"}"#));
        assert!(!parse_logout_success(br#"{"result":0}"#));
        assert!(!parse_logout_success(br#"{"result":"fail"}"#));
        assert!(!parse_logout_success(b"not json"));
    }

    #[test]
    fn parses_http_urls_without_external_url_runtime() {
        assert_eq!(
            ParsedUrl::parse("https://example.com:8443/ping?q=1").unwrap(),
            ParsedUrl {
                secure: true,
                host: "example.com".to_string(),
                port: 8443,
                path_and_query: "/ping?q=1".to_string(),
            }
        );
        assert_eq!(
            ParsedUrl::parse("http://10.2.5.251:801/eportal/")
                .unwrap()
                .port,
            801
        );
        assert_eq!(
            ParsedUrl::parse("HTTPS://example.com?probe=1")
                .unwrap()
                .path_and_query,
            "/?probe=1"
        );
        assert!(ParsedUrl::parse("ftp://example.com").is_err());
        assert!(ParsedUrl::parse("https://example.com:bad/ping").is_err());
    }

    #[test]
    fn winhttp_reads_local_probe_without_following_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.starts_with("GET /probe HTTP/1.1"));
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: /must-not-follow\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });

        let mut client = WinHttpClient::new().unwrap();
        let response = client
            .get(&format!("http://{address}/probe"), 128, 1_000)
            .unwrap();
        assert_eq!(response.status_code, 302);
        assert!(response.body.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn winhttp_logout_accepts_confirmed_jsonp_from_local_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let length = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.starts_with("GET /eportal/?c=Portal&a=logout HTTP/1.1"));
            let body = br#"dr1003({"result":1})"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });

        let mut client = WinHttpClient::new().unwrap();
        client
            .logout_at(&format!("http://{address}/eportal/?c=Portal&a=logout"))
            .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn winhttp_honors_receive_timeout_on_local_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (done_sender, done_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            if done_receiver.recv_timeout(Duration::from_secs(7)).is_ok() {
                return;
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
        });

        let mut client = WinHttpClient::new().unwrap();
        let started = Instant::now();
        let result = client.get(&format!("http://{address}/slow"), 16, 1_000);
        let elapsed = started.elapsed();
        let _ = done_sender.send(());
        server.join().unwrap();
        assert!(result.is_err());
        assert!(elapsed < Duration::from_secs(7));
    }

    #[test]
    fn status_only_probe_does_not_wait_for_a_stalled_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (done_sender, done_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request_headers(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 128\r\nConnection: close\r\n\r\n")
                .unwrap();
            // Hold the body until the client returns, avoiding timing-only assertions.
            let _ = done_receiver.recv_timeout(Duration::from_secs(5));
        });
        let mut client = WinHttpClient::new().unwrap();
        let result = client.get(&format!("http://{address}/probe"), 0, 500);
        let _ = done_sender.send(());
        server.join().unwrap();
        let response = result.expect("response headers alone must complete a status-only probe");
        assert_eq!(response.status_code, 200);
        assert!(response.body.is_empty());
    }

    #[test]
    fn login_rejects_http_errors_and_redirects() {
        for status in [200, 302, 403, 503] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                read_request_headers(&mut stream);
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            });
            let mut client = WinHttpClient::new().unwrap();
            let result = client.login_at(&format!("http://{address}/login"));
            server.join().unwrap();
            if status == 200 {
                assert!(result.is_ok());
            } else {
                assert_eq!(result.unwrap_err(), format!("登录请求返回 HTTP {status}。"));
            }
        }
    }

    #[test]
    fn bounded_body_keeps_one_extra_byte_to_detect_oversized_responses() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request_headers(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n0123456789",
                )
                .unwrap();
        });
        let mut client = WinHttpClient::new().unwrap();
        let result = client.get(&format!("http://{address}/probe"), 4, 1000);
        server.join().unwrap();
        assert_eq!(result.unwrap().body, b"01234");
    }
}
