#[path = "../crates/aec-core/src/brand.rs"]
mod brand;

pub fn build(binary: &str) {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=../../crates/aec-core/src/brand.rs");
    println!("cargo:rerun-if-changed=../../packaging/windows-resource.rs");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let icon = out.join("app.ico");
    std::fs::write(&icon, brand::ico()).unwrap();
    let rc = out.join("app.rc");
    std::fs::write(
        &rc,
        format!(
            "1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    let resource = out.join("app.res");
    let kits = std::path::PathBuf::from(
        std::env::var_os("ProgramFiles(x86)").expect("Windows SDK required"),
    )
    .join("Windows Kits/10/bin");
    let arch = if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
        "arm64"
    } else {
        "x64"
    };
    let mut compilers: Vec<_> = std::fs::read_dir(kits)
        .unwrap()
        .flatten()
        .map(|e| e.path().join(arch).join("rc.exe"))
        .filter(|p| p.is_file())
        .collect();
    compilers.sort();
    let compiler = compilers.pop().expect("Windows SDK rc.exe required");
    assert!(
        std::process::Command::new(compiler)
            .arg("/nologo")
            .arg("/fo")
            .arg(&resource)
            .arg(rc)
            .status()
            .unwrap()
            .success(),
        "resource compilation failed"
    );
    println!("cargo:rustc-link-arg-bin={binary}={}", resource.display());
}
