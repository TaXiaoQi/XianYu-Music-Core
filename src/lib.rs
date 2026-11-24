//! 弦予音乐共享核心（xianyu_core）。
//!
//! 独立仓库：从移动端 `XianYu-Music-Mobile/rust` 抽取的纯逻辑模块，
//! 不依赖 Tauri / flutter_rust_bridge / 桌面窗口，可被 Desktop（Tauri）、
//! Mobile / Watch（flutter_rust_bridge）等宿主跨平台复用。
//!
//! 各客户端仓库以 git submodule 引用本仓库（如 `rust/core`），
//! FFI 桥接层（api/ + frb_generated.rs）留在各端薄壳 crate 中。

pub mod audio_convert;
pub mod cloud;
pub mod custom_fonts;
pub mod database;
pub mod dlna;
pub mod error;
pub mod fallback_verify;
pub mod filename_meta;
pub mod host_crypto;
pub mod music;
pub mod player;
pub mod plugin_host;
pub mod plugins;
pub mod recognize;
pub mod remote;
pub mod security;
pub mod statistics;
pub mod toolbox;
