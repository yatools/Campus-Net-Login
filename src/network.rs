use std::ffi::c_void;
use std::mem::size_of;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_OPTION_RECEIVE_RESPONSE_TIMEOUT, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_LOCATION,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetOption, WinHttpSetTimeouts,
};
use windows::core::{Error as WindowsError, PCWSTR};
use zeroize::{Zeroize, Zeroizing};

use crate::AppResult;
use crate::settings::AppSettings;
use crate::wide::{pcwstr, to_wide, to_wide_zeroizing};

pub const PRIMARY_PROBE_URL: &str = "http://www.msftconnecttest.com/connecttest.txt";
pub const PRIMARY_EXPECTED_BODY: &str = "Microsoft Connect Test";
pub const CAMPUS_LOGIN_PAGE_URL: &str = "http://10.2.5.251/";
pub const PORTAL_ENDPOINT: &str = "http://10.2.5.251:801/eportal/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeOutcome {
    Online,
    CaptivePortal,
    Offline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginResponse {
    Accepted,
    Rejected(String),
    Unknown,
}

pub trait NetworkBackend {
    fn probe_internet(&mut self, settings: &AppSettings) -> ProbeOutcome;
    fn authentication_server_reachable(&mut self) -> bool;
    fn login(&mut self, submitted_account: &str, password: &str) -> AppResult<LoginResponse>;
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
        let agent = to_wide("CampusNetAutoLogin/2.1");
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

        let host = to_wide(&parsed.host);
        let connection = unsafe { WinHttpConnect(self.session.0, pcwstr(&host), parsed.port, 0) };
        let connection = InternetHandle::new(connection, "无法连接目标服务器")?;

        let verb = to_wide("GET");
        let path = to_wide_zeroizing(&parsed.path_and_query);
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

        unsafe {
            WinHttpSetTimeouts(request.0, timeout_ms, timeout_ms, timeout_ms, timeout_ms)
                .map_err(|error| format!("无法设置网络超时：{error}"))?;
        }

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

        let location = if (300..400).contains(&status_code) {
            query_location_header(request.0)
        } else {
            None
        };

        // Status-only probes must not wait for even one byte of a slow response body.
        if body_limit == 0 {
            return Ok(HttpResponse {
                status_code,
                location,
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

        Ok(HttpResponse {
            status_code,
            location,
            body,
        })
    }

    fn probe_internet_with_urls(&mut self, primary_url: &str, fallback_url: &str) -> ProbeOutcome {
        if let Ok(response) = self.get(primary_url, 512, 3000) {
            let trimmed = String::from_utf8_lossy(&response.body);
            let trimmed = trimmed.trim_start_matches('\u{feff}').trim();
            if response.status_code == 200
                && response.body.len() <= 128
                && trimmed == PRIMARY_EXPECTED_BODY
            {
                return ProbeOutcome::Online;
            }
            if (300..400).contains(&response.status_code)
                && response
                    .location
                    .as_deref()
                    .is_some_and(|loc| loc.contains("10.2.5.251"))
            {
                return ProbeOutcome::CaptivePortal;
            }
            if (200..300).contains(&response.status_code)
                && String::from_utf8_lossy(&response.body).contains("10.2.5.251")
            {
                return ProbeOutcome::CaptivePortal;
            }
        }

        if self
            .get(fallback_url, 0, 3000)
            .is_ok_and(|response| (200..300).contains(&response.status_code))
        {
            ProbeOutcome::Online
        } else {
            ProbeOutcome::Offline
        }
    }

    fn login_at(&mut self, url: &str) -> AppResult<LoginResponse> {
        let response = self
            .get(url, 4096, 5000)
            .map_err(|_| "无法连接校园网认证服务器，或登录请求超时。".to_string())?;
        if !(200..300).contains(&response.status_code) {
            return Err(format!("登录请求返回 HTTP {}。", response.status_code));
        }
        Ok(parse_login_response(&response.body))
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
    fn probe_internet(&mut self, settings: &AppSettings) -> ProbeOutcome {
        self.probe_internet_with_urls(PRIMARY_PROBE_URL, &settings.fallback_probe_url)
    }

    fn authentication_server_reachable(&mut self) -> bool {
        self.get(PORTAL_ENDPOINT, 0, 2000).is_ok()
    }

    fn login(&mut self, submitted_account: &str, password: &str) -> AppResult<LoginResponse> {
        let url = Zeroizing::new(build_login_url(submitted_account, password));
        self.login_at(&url)
    }

    fn logout(&mut self) -> AppResult<()> {
        self.logout_at(&build_logout_url())
    }
}

fn query_location_header(request: *mut c_void) -> Option<String> {
    const MAX_LOCATION_BYTES: usize = 32_768;
    let mut buffer = vec![0u16; 1024];
    let initial_bytes = u32::try_from(buffer.len() * size_of::<u16>()).ok()?;
    let mut byte_len = initial_bytes;
    let mut header_index = 0u32;
    let first = unsafe {
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            Some(buffer.as_mut_ptr().cast()),
            &mut byte_len,
            &mut header_index,
        )
    };
    if first.is_err() {
        let required_bytes = byte_len as usize;
        if required_bytes <= initial_bytes as usize || required_bytes > MAX_LOCATION_BYTES {
            return None;
        }
        buffer.resize(required_bytes.div_ceil(size_of::<u16>()), 0);
        byte_len = u32::try_from(buffer.len() * size_of::<u16>()).ok()?;
        header_index = 0;
        unsafe {
            WinHttpQueryHeaders(
                request,
                WINHTTP_QUERY_LOCATION,
                PCWSTR::null(),
                Some(buffer.as_mut_ptr().cast()),
                &mut byte_len,
                &mut header_index,
            )
        }
        .ok()?;
    }

    let char_count = (byte_len as usize / size_of::<u16>()).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..char_count]);
    let trimmed = text.trim_end_matches('\0').trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[derive(Debug)]
struct HttpResponse {
    status_code: u32,
    location: Option<String>,
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

fn extract_json_object(response_body: &[u8]) -> Option<serde_json::Value> {
    let body = String::from_utf8_lossy(response_body);
    let object_start = body.find('{')?;
    let object_end = body.rfind('}')?;
    if object_end <= object_start {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(&body[object_start..=object_end]).ok()
}

fn is_success_result(result: Option<&serde_json::Value>) -> bool {
    match result {
        Some(serde_json::Value::Number(number)) => number.as_i64() == Some(1),
        Some(serde_json::Value::String(text)) => {
            let trimmed = text.trim();
            trimmed == "1" || trimmed.eq_ignore_ascii_case("ok")
        }
        _ => false,
    }
}

fn is_failure_result(result: Option<&serde_json::Value>) -> bool {
    match result {
        Some(serde_json::Value::Number(number)) => number.as_i64() == Some(0),
        Some(serde_json::Value::String(text)) => {
            let trimmed = text.trim();
            trimmed == "0" || trimmed.eq_ignore_ascii_case("fail")
        }
        _ => false,
    }
}

fn try_decode_base64_message(trimmed: &str) -> Option<String> {
    if trimmed.len() < 4
        || !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
    {
        return None;
    }
    let bytes = STANDARD.decode(trimmed).ok()?;
    let decoded = String::from_utf8(bytes).ok()?;
    let decoded_trimmed = decoded.trim();
    if decoded_trimmed.is_empty()
        || !decoded_trimmed
            .chars()
            .all(|ch| !ch.is_control() || ch.is_whitespace())
    {
        return None;
    }
    Some(decoded_trimmed.to_string())
}

pub fn parse_login_response(response_body: &[u8]) -> LoginResponse {
    let Some(value) = extract_json_object(response_body) else {
        return LoginResponse::Unknown;
    };
    let result = value.get("result");
    if is_success_result(result) {
        return LoginResponse::Accepted;
    }
    if !is_failure_result(result) {
        return LoginResponse::Unknown;
    }

    let raw_msg = value
        .get("msg")
        .and_then(|field| field.as_str())
        .map(str::trim)
        .unwrap_or("");
    let ret_code = match value.get("ret_code") {
        Some(serde_json::Value::Number(number)) => Some(number.to_string()),
        Some(serde_json::Value::String(text)) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        _ => None,
    };

    let decoded = if raw_msg.is_empty() {
        String::new()
    } else {
        try_decode_base64_message(raw_msg).unwrap_or_else(|| raw_msg.to_string())
    };
    let message = decoded.trim();
    let is_readable = !message.is_empty()
        && message
            .chars()
            .all(|ch| !ch.is_control() || ch.is_whitespace());

    if is_readable {
        let lower = message.to_lowercase();
        if [
            "已经在线",
            "已在线",
            "already online",
            "in use",
            "ip already",
        ]
        .iter()
        .any(|pattern| lower.contains(pattern))
        {
            return LoginResponse::Accepted;
        }
        if [
            "userid error1",
            "ldap auth error",
            "password",
            "密码错误",
            "账号不存在",
            "用户名或密码",
        ]
        .iter()
        .any(|pattern| lower.contains(pattern))
        {
            return LoginResponse::Rejected(
                "账号或密码错误，请检查学号、密码和所选服务商。".to_string(),
            );
        }
        if ["userid error2", "欠费", "停机", "arrears"]
            .iter()
            .any(|pattern| lower.contains(pattern))
        {
            return LoginResponse::Rejected(
                "校园网账号可能已欠费或停机，请检查账户状态。".to_string(),
            );
        }
        if [
            "userid error3",
            "终端",
            "超限",
            "online num",
            "max",
            "limit",
        ]
        .iter()
        .any(|pattern| lower.contains(pattern))
        {
            return LoginResponse::Rejected(
                "在线终端数量已达上限，请先注销其他设备。".to_string(),
            );
        }

        let truncated: String = message.chars().take(120).collect();
        return LoginResponse::Rejected(format!("认证服务器拒绝登录：{truncated}"));
    }

    if let Some(code) = ret_code {
        LoginResponse::Rejected(format!("认证服务器拒绝登录（代码 {code}）。"))
    } else {
        LoginResponse::Rejected("认证服务器拒绝登录，请检查账号、密码或运营商。".to_string())
    }
}

fn parse_logout_success(response_body: &[u8]) -> bool {
    extract_json_object(response_body)
        .is_some_and(|value| is_success_result(value.get("result")))
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
    fn parse_login_response_handles_jsonp_base64_errors_and_already_online() {
        assert_eq!(
            parse_login_response(br#"dr1003({"result":"1","msg":"Portal protocol auth success!"})"#),
            LoginResponse::Accepted
        );
        assert_eq!(
            parse_login_response(br#"{"result":1}"#),
            LoginResponse::Accepted
        );
        assert_eq!(
            parse_login_response(br#"{"result":"OK"}"#),
            LoginResponse::Accepted
        );

        assert_eq!(
            parse_login_response(br#"dr1003({"result":0,"msg":"ldap auth error"})"#),
            LoginResponse::Rejected(
                "账号或密码错误，请检查学号、密码和所选服务商。".to_string()
            )
        );
        assert_eq!(
            parse_login_response(
                format!(
                    r#"dr1003({{"result":"0","msg":"{}","ret_code":1}})"#,
                    STANDARD.encode("userid error1")
                )
                .as_bytes()
            ),
            LoginResponse::Rejected(
                "账号或密码错误，请检查学号、密码和所选服务商。".to_string()
            )
        );
        assert_eq!(
            parse_login_response(
                format!(
                    r#"dr1003({{"result":"0","msg":"{}"}})"#,
                    STANDARD.encode("userid error2")
                )
                .as_bytes()
            ),
            LoginResponse::Rejected(
                "校园网账号可能已欠费或停机，请检查账户状态。".to_string()
            )
        );
        assert_eq!(
            parse_login_response(
                format!(
                    r#"dr1003({{"result":"0","msg":"{}"}})"#,
                    STANDARD.encode("userid error3")
                )
                .as_bytes()
            ),
            LoginResponse::Rejected(
                "在线终端数量已达上限，请先注销其他设备。".to_string()
            )
        );
        assert_eq!(
            parse_login_response(
                format!(
                    r#"dr1003({{"result":0,"msg":"{}"}})"#,
                    STANDARD.encode("认证失败，请联系管理员")
                )
                .as_bytes()
            ),
            LoginResponse::Rejected("认证服务器拒绝登录：认证失败，请联系管理员".to_string())
        );

        let long_cn = "中".repeat(130);
        let long_resp = parse_login_response(
            format!(r#"{{"result":0,"msg":"{long_cn}"}}"#).as_bytes(),
        );
        assert_eq!(
            long_resp,
            LoginResponse::Rejected(format!("认证服务器拒绝登录：{}", "中".repeat(120)))
        );

        assert_eq!(
            parse_login_response(
                format!(
                    r#"dr1003({{"result":0,"msg":"{}"}})"#,
                    STANDARD.encode("您已经在线，无需重复登录")
                )
                .as_bytes()
            ),
            LoginResponse::Accepted
        );
        assert_eq!(
            parse_login_response(br#"{"result":"fail","msg":"IP already in use"}"#),
            LoginResponse::Accepted
        );
        assert_eq!(
            parse_login_response(br#"{"result":0,"msg":"","ret_code":2}"#),
            LoginResponse::Rejected("认证服务器拒绝登录（代码 2）。".to_string())
        );
        assert_eq!(
            parse_login_response(br#"{"result":0,"msg":""}"#),
            LoginResponse::Rejected(
                "认证服务器拒绝登录，请检查账号、密码或运营商。".to_string()
            )
        );
        assert_eq!(
            parse_login_response(b"<html><body>unexpected</body></html>"),
            LoginResponse::Unknown
        );
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
        assert_eq!(response.location.as_deref(), Some("/must-not-follow"));
        assert!(response.body.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn probe_internet_detects_captive_portal_redirect_and_html_and_fallback() {
        let portal_redirect = TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect_addr = portal_redirect.local_addr().unwrap();
        let redirect_server = thread::spawn(move || {
            let (mut stream, _) = portal_redirect.accept().unwrap();
            read_request_headers(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: http://10.2.5.251/a79.htm\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });

        let mut client = WinHttpClient::new().unwrap();
        assert_eq!(
            client.probe_internet_with_urls(
                &format!("http://{redirect_addr}/connecttest.txt"),
                "http://127.0.0.1:1/unused"
            ),
            ProbeOutcome::CaptivePortal
        );
        redirect_server.join().unwrap();

        let portal_html = TcpListener::bind("127.0.0.1:0").unwrap();
        let html_addr = portal_html.local_addr().unwrap();
        let html_server = thread::spawn(move || {
            let (mut stream, _) = portal_html.accept().unwrap();
            read_request_headers(&mut stream);
            let body = b"<html><script>location.href='http://10.2.5.251/';</script></html>";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
        });
        assert_eq!(
            client.probe_internet_with_urls(
                &format!("http://{html_addr}/connecttest.txt"),
                "http://127.0.0.1:1/unused"
            ),
            ProbeOutcome::CaptivePortal
        );
        html_server.join().unwrap();

        let primary_other = TcpListener::bind("127.0.0.1:0").unwrap();
        let other_addr = primary_other.local_addr().unwrap();
        let fallback = TcpListener::bind("127.0.0.1:0").unwrap();
        let fallback_addr = fallback.local_addr().unwrap();
        let other_server = thread::spawn(move || {
            let (mut stream, _) = primary_other.accept().unwrap();
            read_request_headers(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 302 Found\r\nLocation: https://example.com/other\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let fallback_server = thread::spawn(move || {
            let (mut stream, _) = fallback.accept().unwrap();
            read_request_headers(&mut stream);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        assert_eq!(
            client.probe_internet_with_urls(
                &format!("http://{other_addr}/connecttest.txt"),
                &format!("http://{fallback_addr}/favicon.ico")
            ),
            ProbeOutcome::Online
        );
        other_server.join().unwrap();
        fallback_server.join().unwrap();
    }

    #[test]
    fn winhttp_login_parses_jsonp_rejection_from_local_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request_headers(&mut stream);
            let body = format!(
                r#"dr1003({{"result":"0","msg":"{}","ret_code":1}})"#,
                STANDARD.encode("userid error1")
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let mut client = WinHttpClient::new().unwrap();
        let result = client
            .login_at(&format!("http://{address}/eportal/?c=Portal&a=login"))
            .unwrap();
        assert_eq!(
            result,
            LoginResponse::Rejected(
                "账号或密码错误，请检查学号、密码和所选服务商。".to_string()
            )
        );
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
                assert_eq!(result.unwrap(), LoginResponse::Unknown);
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
