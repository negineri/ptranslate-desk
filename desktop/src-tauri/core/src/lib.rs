//! デスクトップのアプリと単体のワーカー CLI が共有する、翻訳とワーカーの処理。
//! Tauri には依存しない。

pub mod engine;
#[cfg(target_os = "linux")]
pub mod host_env;
pub mod translation;
pub mod worker_agent;
pub mod worker_power;
pub mod worker_relay;
pub mod worker_sandbox;
pub mod worker_supervisor;
