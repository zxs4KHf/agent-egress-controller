//! Outbound HTTP for detection (SPEC 4.6). Every request either goes direct or through one named
//! proxy port, carries a fixed User-Agent, no cookies and no Referer.

use std::time::Duration;

pub const USER_AGENT: &str = concat!("AgentEgressController/", env!("CARGO_PKG_VERSION"));
/// Responses larger than this are truncated; detection endpoints answer with a few hundred bytes.
pub const MAX_BODY: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    Direct,
    /// Through the HTTP proxy listening on 127.0.0.1:<port>.
    Proxy(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub url: String,
    pub method: Method,
    pub route: Route,
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
}

impl Request {
    pub fn get(url: impl Into<String>, route: Route) -> Self {
        Self {
            url: url.into(),
            method: Method::Get,
            route,
            connect_timeout: Duration::from_secs(3),
            total_timeout: Duration::from_secs(8),
        }
    }
    pub fn head(url: impl Into<String>, route: Route) -> Self {
        Self {
            method: Method::Head,
            ..Self::get(url, route)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
    pub elapsed: Duration,
}

/// Why a request produced no response. Shown as the reason of an "unknown" or "fail" result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpError {
    Timeout,
    Connect(String),
    Other(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Timeout => f.write_str("timeout"),
            HttpError::Connect(m) => write!(f, "connect_failed: {m}"),
            HttpError::Other(m) => write!(f, "error: {m}"),
        }
    }
}

pub trait HttpClient {
    fn send(&self, req: &Request) -> Result<Response, HttpError>;
}

/// `scheme://host[:port]/path?query` split for WinHTTP.
#[derive(Debug, PartialEq, Eq)]
pub struct UrlParts {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn split_url(url: &str) -> Option<UrlParts> {
    let (secure, rest) = match url.strip_prefix("https://") {
        Some(r) => (true, r),
        None => (false, url.strip_prefix("http://")?),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => (h, p.parse().ok()?),
        _ => (authority, if secure { 443 } else { 80 }),
    };
    Some(UrlParts {
        secure,
        host: host.trim_matches(|c| c == '[' || c == ']').to_string(),
        port,
        path: path.to_string(),
    })
}

#[cfg(windows)]
pub use winhttp::WinHttpClient;

#[cfg(windows)]
mod winhttp {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};
    use std::time::Instant;

    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
        WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
        WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_NAMED_PROXY,
        WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_DISABLE_COOKIES, WINHTTP_DISABLE_REDIRECTS,
        WINHTTP_FLAG_SECURE, WINHTTP_OPTION_DISABLE_FEATURE, WINHTTP_QUERY_FLAG_NUMBER,
        WINHTTP_QUERY_STATUS_CODE,
    };

    use super::{
        split_url, HttpClient, HttpError, Method, Request, Response, Route, MAX_BODY, USER_AGENT,
    };

    const ERROR_WINHTTP_TIMEOUT: u32 = 12002;
    const ERROR_WINHTTP_NAME_NOT_RESOLVED: u32 = 12007;
    const ERROR_WINHTTP_CANNOT_CONNECT: u32 = 12029;
    const ERROR_WINHTTP_CONNECTION_ERROR: u32 = 12030;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    fn fail(what: &str) -> HttpError {
        match unsafe { GetLastError() } {
            ERROR_WINHTTP_TIMEOUT => HttpError::Timeout,
            c @ (ERROR_WINHTTP_NAME_NOT_RESOLVED
            | ERROR_WINHTTP_CANNOT_CONNECT
            | ERROR_WINHTTP_CONNECTION_ERROR) => HttpError::Connect(format!("{what} winhttp {c}")),
            c => HttpError::Other(format!("{what} winhttp {c}")),
        }
    }

    /// WinHTTP session per request: requests are few, and this keeps routes from leaking into
    /// each other through connection reuse.
    #[derive(Debug, Default, Clone, Copy)]
    pub struct WinHttpClient;

    impl HttpClient for WinHttpClient {
        fn send(&self, req: &Request) -> Result<Response, HttpError> {
            let url = split_url(&req.url)
                .ok_or_else(|| HttpError::Other(format!("unsupported url {}", req.url)))?;
            let ua = wide(USER_AGENT);
            let started = Instant::now();
            let session = match req.route {
                Route::Direct => Handle(unsafe {
                    WinHttpOpen(ua.as_ptr(), WINHTTP_ACCESS_TYPE_NO_PROXY, null(), null(), 0)
                }),
                Route::Proxy(port) => {
                    let proxy = wide(&format!("127.0.0.1:{port}"));
                    let bypass = wide("<local>");
                    Handle(unsafe {
                        WinHttpOpen(
                            ua.as_ptr(),
                            WINHTTP_ACCESS_TYPE_NAMED_PROXY,
                            proxy.as_ptr(),
                            bypass.as_ptr(),
                            0,
                        )
                    })
                }
            };
            if session.0.is_null() {
                return Err(fail("WinHttpOpen"));
            }
            let total = req.total_timeout.as_millis() as i32;
            let connect = req.connect_timeout.as_millis() as i32;
            unsafe { WinHttpSetTimeouts(session.0, connect, connect, total, total) };

            let host = wide(&url.host);
            let conn = Handle(unsafe { WinHttpConnect(session.0, host.as_ptr(), url.port, 0) });
            if conn.0.is_null() {
                return Err(fail("WinHttpConnect"));
            }
            let verb = wide(match req.method {
                Method::Get => "GET",
                Method::Head => "HEAD",
            });
            let path = wide(&url.path);
            let flags = if url.secure { WINHTTP_FLAG_SECURE } else { 0 };
            let request = Handle(unsafe {
                WinHttpOpenRequest(
                    conn.0,
                    verb.as_ptr(),
                    path.as_ptr(),
                    null(),
                    null(),
                    null(),
                    flags,
                )
            });
            if request.0.is_null() {
                return Err(fail("WinHttpOpenRequest"));
            }
            // No cookie jar and no redirects: a detection request must not pick up or follow state.
            let features: u32 = WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_REDIRECTS;
            unsafe {
                WinHttpSetOption(
                    request.0,
                    WINHTTP_OPTION_DISABLE_FEATURE,
                    &features as *const u32 as *const c_void,
                    4,
                )
            };

            if unsafe { WinHttpSendRequest(request.0, null(), 0, null(), 0, 0, 0) } == 0 {
                return Err(fail("WinHttpSendRequest"));
            }
            if unsafe { WinHttpReceiveResponse(request.0, null_mut()) } == 0 {
                return Err(fail("WinHttpReceiveResponse"));
            }
            let mut status = 0u32;
            let mut size = 4u32;
            if unsafe {
                WinHttpQueryHeaders(
                    request.0,
                    WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                    null(),
                    &mut status as *mut u32 as *mut c_void,
                    &mut size,
                    null_mut(),
                )
            } == 0
            {
                return Err(fail("WinHttpQueryHeaders"));
            }
            let mut body = Vec::new();
            if req.method == Method::Get {
                loop {
                    let mut avail = 0u32;
                    if unsafe { WinHttpQueryDataAvailable(request.0, &mut avail) } == 0 {
                        return Err(fail("WinHttpQueryDataAvailable"));
                    }
                    if avail == 0 || body.len() >= MAX_BODY {
                        break;
                    }
                    let mut buf = vec![0u8; avail as usize];
                    let mut read = 0u32;
                    if unsafe {
                        WinHttpReadData(
                            request.0,
                            buf.as_mut_ptr() as *mut c_void,
                            avail,
                            &mut read,
                        )
                    } == 0
                    {
                        return Err(fail("WinHttpReadData"));
                    }
                    body.extend_from_slice(&buf[..read as usize]);
                }
                body.truncate(MAX_BODY);
            }
            Ok(Response {
                status: status as u16,
                body: String::from_utf8_lossy(&body).into_owned(),
                elapsed: started.elapsed(),
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::{Read, Write};
        use std::net::TcpListener;

        /// Capture what actually goes on the wire for a direct request.
        #[test]
        fn sends_fixed_user_agent_without_cookie_or_referer() {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap();
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nSet-Cookie: a=b\r\nConnection: close\r\n\r\nhello").unwrap();
                String::from_utf8_lossy(&buf[..n]).into_owned()
            });
            let r = WinHttpClient
                .send(&Request::get(
                    format!("http://127.0.0.1:{port}/probe"),
                    Route::Direct,
                ))
                .unwrap();
            let seen = server.join().unwrap().to_ascii_lowercase();
            assert_eq!((r.status, r.body.as_str()), (200, "hello"));
            assert!(seen.starts_with("get /probe http/1.1"), "{seen}");
            assert!(
                seen.contains(&format!("user-agent: {}", USER_AGENT.to_ascii_lowercase())),
                "{seen}"
            );
            assert!(
                !seen.contains("cookie:") && !seen.contains("referer:"),
                "{seen}"
            );
        }

        #[test]
        fn refused_connection_is_reported() {
            let port = TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let e = WinHttpClient
                .send(&Request::get(
                    format!("http://127.0.0.1:{port}/"),
                    Route::Direct,
                ))
                .unwrap_err();
            assert!(
                matches!(e, HttpError::Connect(_) | HttpError::Other(_)),
                "{e:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_urls() {
        assert_eq!(
            split_url("https://claude.ai/cdn-cgi/trace"),
            Some(UrlParts {
                secure: true,
                host: "claude.ai".into(),
                port: 443,
                path: "/cdn-cgi/trace".into()
            })
        );
        assert_eq!(
            split_url("http://127.0.0.1:8080"),
            Some(UrlParts {
                secure: false,
                host: "127.0.0.1".into(),
                port: 8080,
                path: "/".into()
            })
        );
        assert_eq!(
            split_url("https://ipwho.is/1.2.3.4?x=1").unwrap().path,
            "/1.2.3.4?x=1"
        );
        assert_eq!(split_url("ftp://x/"), None);
        assert_eq!(split_url("https://user@host/"), None);
    }
}
