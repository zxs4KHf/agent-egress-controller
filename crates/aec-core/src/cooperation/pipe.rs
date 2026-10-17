//! Bounded HTTP reads over an already existing local Windows named pipe.
//! No impersonation, controller creation, authentication token, or mutation.
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_BUSY,
        GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    },
    Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, SECURITY_ANONYMOUS,
        SECURITY_SQOS_PRESENT,
    },
    System::{
        Pipes::{GetNamedPipeServerProcessId, WaitNamedPipeW},
        Threading::{CreateEventW, WaitForSingleObject},
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
    },
};

const LIMIT: usize = 1024 * 1024;
const READ_FAILED: &str = "COOP_CONTROLLER_READ_FAILED";
const INVALID: &str = "COOP_CONTROLLER_REPLY_INVALID";
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
pub(super) struct Connection {
    handle: Handle,
    deadline: Instant,
}
impl Connection {
    pub(super) fn open(name: &str) -> Result<Self, &'static str> {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        let open = || unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_ANONYMOUS,
                std::ptr::null_mut(),
            )
        };
        let mut handle = open();
        if handle == INVALID_HANDLE_VALUE && unsafe { GetLastError() } == ERROR_PIPE_BUSY {
            if unsafe { WaitNamedPipeW(wide.as_ptr(), 2000) } == 0 {
                return Err(READ_FAILED);
            }
            handle = open();
        }
        if handle == INVALID_HANDLE_VALUE {
            return Err(READ_FAILED);
        }
        Ok(Self {
            handle: Handle(handle),
            deadline,
        })
    }
    pub(super) fn pid(&self) -> Result<u32, &'static str> {
        let mut pid = 0;
        if unsafe { GetNamedPipeServerProcessId(self.handle.0, &mut pid) } == 0 || pid == 0 {
            return Err("COOP_CONTROLLER_OWNER_UNKNOWN");
        }
        Ok(pid)
    }
    // Complete or cancel every OVERLAPPED before releasing its event/buffer.
    fn transfer(&self, buffer: &mut [u8], write: bool) -> Result<usize, &'static str> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(READ_FAILED);
        }
        unsafe {
            let event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
            if event.is_null() {
                return Err(READ_FAILED);
            }
            let event = Handle(event);
            let mut operation: OVERLAPPED = std::mem::zeroed();
            operation.hEvent = event.0;
            let mut count = 0;
            let ok = if write {
                WriteFile(
                    self.handle.0,
                    buffer.as_ptr(),
                    buffer.len() as u32,
                    &mut count,
                    &mut operation,
                )
            } else {
                ReadFile(
                    self.handle.0,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    &mut count,
                    &mut operation,
                )
            };
            if ok == 0 {
                match GetLastError() {
                    ERROR_BROKEN_PIPE if !write => return Ok(0),
                    ERROR_IO_PENDING => {
                        let wait = WaitForSingleObject(
                            event.0,
                            remaining.as_millis().min(u128::from(u32::MAX - 1)) as u32,
                        );
                        if wait != WAIT_OBJECT_0 {
                            CancelIoEx(self.handle.0, &operation);
                            GetOverlappedResult(self.handle.0, &operation, &mut count, 1);
                            return Err(READ_FAILED);
                        }
                        if GetOverlappedResult(self.handle.0, &operation, &mut count, 0) == 0 {
                            if !write && GetLastError() == ERROR_BROKEN_PIPE {
                                return Ok(0);
                            }
                            return Err(READ_FAILED);
                        }
                    }
                    _ => return Err(READ_FAILED),
                }
            }
            Ok(count as usize)
        }
    }
    pub(super) fn get(&self, path: &str) -> Result<(u16, String), &'static str> {
        let mut request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .into_bytes();
        let mut offset = 0;
        while offset < request.len() {
            let count = self.transfer(&mut request[offset..], true)?;
            if count == 0 {
                return Err(READ_FAILED);
            }
            offset += count;
        }
        let mut raw = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let count = self.transfer(&mut buffer, false)?;
            if count == 0 {
                break;
            }
            if raw.len() + count > LIMIT {
                return Err(INVALID);
            }
            raw.extend_from_slice(&buffer[..count]);
        }
        response(&raw)
    }
}

fn response(raw: &[u8]) -> Result<(u16, String), &'static str> {
    let at = raw
        .windows(4)
        .position(|b| b == b"\r\n\r\n")
        .ok_or(INVALID)?;
    if at > 16 * 1024 {
        return Err(INVALID);
    }
    let header = std::str::from_utf8(&raw[..at]).map_err(|_| INVALID)?;
    let mut lines = header.split("\r\n");
    let first = lines.next().ok_or(INVALID)?;
    if !first.starts_with("HTTP/1.1 ") && !first.starts_with("HTTP/1.0 ") {
        return Err(INVALID);
    }
    let status = first
        .split_ascii_whitespace()
        .nth(1)
        .and_then(|n| n.parse::<u16>().ok())
        .filter(|n| (100..=599).contains(n))
        .ok_or(INVALID)?;
    let mut length = None;
    let mut chunked = false;
    for line in lines {
        let (key, value) = line.split_once(':').ok_or(INVALID)?;
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err(INVALID);
            }
            length = Some(value.trim().parse::<usize>().map_err(|_| INVALID)?);
        } else if key.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.trim().eq_ignore_ascii_case("chunked") {
                return Err(INVALID);
            }
            chunked = true;
        }
    }
    let mut body = &raw[at + 4..];
    let bytes = if chunked {
        if length.is_some() {
            return Err(INVALID);
        }
        let mut out = Vec::new();
        loop {
            let end = body.windows(2).position(|b| b == b"\r\n").ok_or(INVALID)?;
            let size = std::str::from_utf8(&body[..end]).map_err(|_| INVALID)?;
            let size = usize::from_str_radix(size, 16).map_err(|_| INVALID)?;
            body = &body[end + 2..];
            if size == 0 {
                if body != b"\r\n" {
                    return Err(INVALID);
                }
                break;
            }
            let end = size
                .checked_add(2)
                .filter(|n| *n <= body.len())
                .ok_or(INVALID)?;
            if &body[size..end] != b"\r\n" {
                return Err(INVALID);
            }
            out.extend_from_slice(&body[..size]);
            body = &body[end..];
        }
        out
    } else {
        if length.is_some_and(|n| n != body.len()) {
            return Err(INVALID);
        }
        body.to_vec()
    };
    Ok((status, String::from_utf8(bytes).map_err(|_| INVALID)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framed_json_accepts_complete_plain_and_chunked_responses() {
        for raw in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n{\r\n1\r\n}\r\n0\r\n\r\n"
                .as_slice(),
        ] {
            assert_eq!(response(raw).unwrap(), (200, "{}".into()));
        }
    }
    #[test]
    fn malformed_or_ambiguous_framing_never_becomes_runtime_evidence() {
        for raw in [
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n{}",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\n{}\r\n0\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n{}",
        ] {
            assert_eq!(response(raw.as_bytes()).unwrap_err(), INVALID);
        }
    }
}
