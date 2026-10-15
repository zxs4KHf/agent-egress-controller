#![cfg_attr(not(test), windows_subsystem = "windows")]

fn valid_app_url(url: &str) -> bool {
    if url.len() > 1024 || !url.starts_with("http://127.0.0.1:") {
        return false;
    }
    let Some((authority, path)) = url[7..].split_once('/') else {
        return false;
    };
    authority
        .strip_prefix("127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| port != 0)
        && path.starts_with("app?t=")
        && !url.contains('@')
        && !url.contains('#')
}

#[cfg(not(windows))]
fn main() {
    eprintln!("aec-control runs on Windows only");
}

#[cfg(windows)]
fn main() {
    use std::ptr::null;

    use tao::dpi::LogicalSize;
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoop};
    use tao::platform::windows::WindowExtWindows;
    use tao::window::WindowBuilder;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, MessageBoxW, PostMessageW, SetForegroundWindow, ShowWindow, MB_ICONWARNING,
        MB_OK, SW_RESTORE, SW_SHOW, WM_CLOSE,
    };
    use wry::{WebContext, WebViewBuilder};

    let args: Vec<String> = std::env::args().skip(1).collect();
    // Maintenance entry point changes only owned shortcuts, never starts the network worker.
    if args.first().is_some_and(|arg| arg == "--create-shortcuts") {
        use aec_core::shell_identity::{create_shortcuts, ShortcutScope};
        let scope = match args.get(1).map(String::as_str) {
            None | Some("all") => ShortcutScope::All,
            Some("desktop") => ShortcutScope::Desktop,
            Some("start-menu") => ShortcutScope::StartMenu,
            _ => std::process::exit(2),
        };
        if args.len() > 2 {
            std::process::exit(2);
        }
        let result = std::env::current_exe()
            .map_err(|e| e.to_string())
            .and_then(|exe| create_shortcuts(&exe.with_file_name("aec-tray.exe"), scope));
        if result.is_err() {
            std::process::exit(1);
        }
        return;
    }
    if args.first().is_some_and(|arg| arg == "--quit") {
        let title = wide("Agent Egress Controller");
        let hwnd = unsafe { FindWindowW(null(), title.as_ptr()) };
        if !hwnd.is_null() {
            unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
        }
        return;
    }
    let url = args.first().cloned().unwrap_or_default();
    if let Err(error) = aec_core::shell_identity::initialize_control_process() {
        show_error(&error);
        return;
    }
    if !valid_app_url(&url) {
        show_error("入口地址无效。请从 Agent Egress 托盘打开看板。");
        return;
    }
    let local_origin = url
        .split_once("127.0.0.1:")
        .and_then(|(_, rest)| rest.split_once('/').map(|(authority, _)| authority))
        .map(|port| format!("http://127.0.0.1:{port}/"))
        .expect("validated loopback URL has a port");

    let mutex_name = wide("Local\\AgentEgressControllerControlWindow");
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 1, mutex_name.as_ptr()) };
    if mutex.is_null() {
        show_error("无法创建桌面控制中心。");
        return;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let title = wide("Agent Egress Controller");
        let hwnd = unsafe { FindWindowW(null(), title.as_ptr()) };
        if !hwnd.is_null() {
            unsafe {
                ShowWindow(hwnd, SW_RESTORE);
                ShowWindow(hwnd, SW_SHOW);
                SetForegroundWindow(hwnd);
            }
        }
        return;
    }

    let event_loop = EventLoop::new();
    let window = match WindowBuilder::new()
        .with_visible(false)
        .with_window_icon(
            tao::window::Icon::from_rgba(aec_core::brand::rgba(64, (15, 118, 110)), 64, 64).ok(),
        )
        .with_title("Agent Egress Controller")
        .with_inner_size(LogicalSize::new(1100.0, 780.0))
        .with_min_inner_size(LogicalSize::new(860.0, 600.0))
        .build(&event_loop)
    {
        Ok(window) => window,
        Err(error) => {
            show_error(&format!("桌面控制中心窗口创建失败：{error}"));
            return;
        }
    };
    let identity = std::env::current_exe()
        .map_err(|e| e.to_string())
        .and_then(|exe| aec_core::shell_identity::configure_control_window(window.hwnd(), &exe));
    if let Err(error) = identity {
        show_error(&error);
        return;
    }
    let webview_data = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
                .unwrap_or_else(|| std::path::PathBuf::from("."))
        })
        .join("AgentEgressController")
        .join("webview");
    let mut web_context = WebContext::new(Some(webview_data));
    let webview = match WebViewBuilder::with_web_context(&mut web_context)
        .with_url(&url)
        .with_navigation_handler(move |next| {
            next.starts_with(&local_origin) || next.starts_with("https://")
        })
        .build(&window)
    {
        Ok(webview) => webview,
        Err(error) => {
            show_error(&format!(
            "内嵌网页运行环境无法启动：{error}\n请安装 Microsoft Edge WebView2 Runtime 后重试。"
            ));
            return;
        }
    };

    // Do not expose a half-initialized window (or accept close before WebView2 is ready).
    window.set_visible(true);
    event_loop.run(move |event, _, control_flow| {
        let _keep_webview_alive = &webview;
        *control_flow = ControlFlow::Wait;
        if matches!(
            event,
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            }
        ) {
            *control_flow = ControlFlow::Exit;
        }
    });

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn show_error(message: &str) {
        let body = wide(message);
        let title = wide("Agent Egress Controller");
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                body.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONWARNING,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::valid_app_url;

    #[test]
    fn desktop_control_only_accepts_authenticated_loopback_app_urls() {
        assert!(valid_app_url(
            "http://127.0.0.1:12345/app?t=012345&tab=setup"
        ));
        for url in [
            "https://127.0.0.1:12345/app?t=secret",
            "http://localhost:12345/app?t=secret",
            "http://127.0.0.1:0/app?t=secret",
            "http://127.0.0.1:12345/setup?t=secret",
            "http://127.0.0.1:12345/app?t=secret#fragment",
            "http://127.0.0.1:12345@app.example/app?t=secret",
        ] {
            assert!(!valid_app_url(url), "accepted {url}");
        }
    }
}
