use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSource {
    // 实现
    pub id: String,
    pub name: String,
    pub provider: String,
    pub base_url: String,
    pub username: Option<String>,
    pub root_path: String,
    pub enabled: bool,
    pub last_sync_at: Option<i64>,
    pub last_sync_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSourceCredentials {
    // 实现
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider: String,
    pub base_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub root_path: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub last_sync_at: Option<i64>,
    #[serde(default)]
    pub last_sync_error: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

impl RemoteSourceCredentials {
    /// 剥离密码字段，转成可下发给前端的公开视图。
    pub fn into_public(self) -> RemoteSource {
        RemoteSource {
            id: self.id,
            name: self.name,
            provider: self.provider,
            base_url: self.base_url,
            username: self.username,
            root_path: self.root_path,
            enabled: self.enabled,
            last_sync_at: self.last_sync_at,
            last_sync_error: self.last_sync_error,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

impl From<RemoteSourceCredentials> for RemoteSource {
    fn from(value: RemoteSourceCredentials) -> Self {
        value.into_public()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSourceInput {
    // 实现
    pub id: Option<String>,
    pub name: String,
    pub provider: String,
    pub base_url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub root_path: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSyncResult {
    // 实现
    pub source_id: String,
    pub indexed_files: usize,
    pub audio_files: usize,
    pub parsed_songs: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileEntry {
    // 实现
    pub remote_path: String,
    pub name: String,
    pub size: u64,
    pub etag: Option<String>,
    pub modified_at: Option<String>,
    pub is_dir: bool,
}

impl RemoteFileEntry {
    // RemoteFileEntry
    pub fn remote_uri(&self, source_id: &str) -> String {
        format!(
            "remote://{}/{}",
            source_id,
            self.remote_path.trim_start_matches('/')
        )
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCacheUsage {
    pub bytes: u64,
    pub files: usize,
    pub limit_bytes: u64,
}

/// 连接测试结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteConnectionResult {
    pub ok: bool,
    pub message: String,
}

/// 单文件下载进度事件负载（事件名 remote-download-progress）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDownloadProgress {
    pub uri: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub percent: Option<f64>,
    pub done: bool,
    pub failed: bool,
    pub message: Option<String>,
}

/// 同步进度事件负载（事件名 remote-sync-progress）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSyncProgress {
    pub source_id: String,
    pub phase: String,
    pub current: usize,
    pub total: usize,
    pub message: String,
    pub done: bool,
    pub failed: bool,
}

/// 解析远程源 JSON 为凭据结构。
pub fn parse_remote_source(json: &str) -> Result<RemoteSourceCredentials, String> {
    serde_json::from_str(json).map_err(|e| format!("WebDAV 源 JSON 无效: {e}"))
}
