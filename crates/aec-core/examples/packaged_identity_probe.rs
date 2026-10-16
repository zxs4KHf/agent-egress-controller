//! Isolated launch-contract fixture: no tray, credentials, network or global settings.
#[cfg(windows)]
fn main() {
    if let Some(code) = aec_core::independent_launch::enter().unwrap() {
        std::process::exit(code);
    }
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let args: Vec<_> = aec_core::independent_launch::arguments().collect();
    let mut length = 0;
    let result = unsafe { GetCurrentPackageFullName(&mut length, std::ptr::null_mut()) };
    let independent = result == 15700; // APPMODEL_ERROR_NO_PACKAGE, checked independently.
    std::fs::write(
        &args[0],
        serde_json::to_vec(&serde_json::json!({
            "independent": independent,
            "reexecuted": std::env::args_os().any(|arg| arg == "--aec-independent-launch"),
            "arguments": args.iter().skip(1).map(|s| s.to_string_lossy()).collect::<Vec<_>>()
        }))
        .unwrap(),
    )
    .unwrap();
    if !independent {
        std::process::exit(1);
    }
    if args.iter().any(|arg| arg == "exit-37") {
        std::process::exit(37);
    }
}

#[cfg(not(windows))]
fn main() {}
