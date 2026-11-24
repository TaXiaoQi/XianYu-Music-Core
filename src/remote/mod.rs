pub mod cache;
pub mod repository;
pub mod scanner;
pub mod types;
pub mod webdav;

use std::time::{SystemTime, UNIX_EPOCH};

/// 获取当前 Unix 时间戳（秒）
pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
