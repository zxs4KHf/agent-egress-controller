//! The one mihomo build this application runs. Changing it is a deliberate edit here.
//!
//! Source: GitHub release MetaCubeX/mihomo v1.19.31, asset `mihomo-windows-amd64-v1.19.31.zip`.
//! The archive digest is the one GitHub publishes for the asset; the executable digest was measured
//! after extraction on 2026-10-11 and is byte-identical to the legacy chain's `mihomo.exe`.

pub const VERSION: &str = "v1.19.31";
pub const ASSET: &str = "mihomo-windows-amd64-v1.19.31.zip";
pub const ASSET_URL: &str =
    "https://github.com/MetaCubeX/mihomo/releases/download/v1.19.31/mihomo-windows-amd64-v1.19.31.zip";
pub const ASSET_SHA256: &str = "38b2420799d9e7cde77ec1a19c7150dd17ca77f7fb82d9f62cb8763a307eee67";
pub const EXE_IN_ASSET: &str = "mihomo-windows-amd64.exe";
pub const EXE_SHA256: &str = "532ed88b3a37d72f6b5ee5d24a45ce5d8799f3ba34931d6a167d4345f7ebca50";
/// Installed under this name so it is never mistaken for the legacy `mihomo.exe` process.
pub const EXE_NAME: &str = "aec-mihomo.exe";
