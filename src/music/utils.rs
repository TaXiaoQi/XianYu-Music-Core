// 路径与数值层面的零散工具。
// Windows 反斜杠形态与 verbatim（`\\?\`）前缀的清理、网络盘判定、
// SQL LIKE 转义，以及数据库整型到窄整型的安全收窄。
// 以桌面端实现为主干回填共享仓库，并保留移动端 `descendant_prefixes` 增量。

use std::fs;

/// 支持进入曲库扫描的音频扩展名（含 QMC 加密壳格式）。
pub const SUPPORTED_LIBRARY_EXTENSIONS: &[&str] = &[
  "aac", "aif", "aiff", "ape", "dff", "dsf", "flac", "m4a", "m4b", "mp3", "mp4", "oga", "ogg",
  "opus", "wav", "wv", "mgg", "mgg0", "mggl", "mflac", "mflac0", "qmc0", "qmc2", "qmc3", "qmcflac",
  "qmcogg",
];

pub const CUE_FILE_EXTENSIONS: &[&str] = &["cue"];

// ---- Windows 路径前缀归一 ----

/// 去掉 canonicalize 产生的 `\\?\` / `\\?\UNC\` 前缀，统一成常规盘符或 UNC 形态。
fn strip_windows_path_prefixes(path: String) -> String {
  if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
    return format!(r"\\{rest}");
  }
  if let Some(rest) = path.strip_prefix(r"UNC\") {
    return format!(r"\\{rest}");
  }
  if let Some(rest) = path.strip_prefix(r"\\?\") {
    return rest.to_string();
  }
  path
}

fn canonicalize_or_keep(path_str: &str) -> String {
  match fs::canonicalize(path_str) {
    Ok(resolved) => {
      let rendered = resolved.to_string_lossy().into_owned();
      if cfg!(windows) {
        strip_windows_path_prefixes(rendered)
      } else {
        rendered
      }
    }
    Err(_) => {
      // 文件不存在时仅做斜杠风格规整（与移动端既有回退口径一致）。
      let s = path_str.to_string();
      if cfg!(windows) {
        s.replace('/', "\\")
      } else {
        s
      }
    }
  }
}

/// 规范化路径：优先取真实文件系统形态；文件不存在时仅做斜杠风格规整。
pub fn normalize_path(path_str: &str) -> String { // normalize_path
  if cfg!(windows) {
    canonicalize_or_keep(path_str)
  } else {
    match fs::canonicalize(path_str) {
      Ok(resolved) => resolved.to_string_lossy().into_owned(),
      Err(_) => path_str.to_string(),
    }
  }
}

/// 识别历史版本写入数据库的、缺失 `\\` 前缀的 UNC 路径（`UNC\host\share` 形态）。
pub fn legacy_unc_path(path_str: &str) -> Option<String> { // legacy_unc_path
  path_str
    .strip_prefix(r"\\")
    .filter(|path| !path.starts_with(r"?\") && !path.starts_with(r".\"))
    .map(|path| format!(r"UNC\{path}"))
}

/// 判定路径是否位于网络共享或远程映射盘上。
pub fn is_network_share_path(path_str: &str) -> bool { // is_network_share_path
  let path = path_str.trim();
  let looks_unc = path.starts_with(r"\\?\UNC\")
    || path.starts_with("//")
    || (path.starts_with(r"\\") && !path.starts_with(r"\\?\") && !path.starts_with(r"\\.\"));
  if looks_unc {
    return true;
  }

  #[cfg(target_os = "windows")]
  {
    use std::ffi::OsStr;
    use std::iter;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

    // 远程/映射驱动器在 Win32 中的类型码。
    const DRIVE_REMOTE: u32 = 4;

    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
      let drive_root = format!("{}:\\", bytes[0] as char);
      let wide = OsStr::new(&drive_root)
        .encode_wide()
        .chain(iter::once(0))
        .collect::<Vec<_>>();
      return unsafe { GetDriveTypeW(wide.as_ptr()) } == DRIVE_REMOTE;
    }
  }

  false
}

// ---- 路径匹配键（大小写不敏感比较，供端侧 UI 归属判断使用） ----
//
// 与 normalize_path（文件系统 canonicalize，取真实大小写）不同：
// 这组函数是纯字符串比较语义——反斜杠→正斜杠、去尾部斜杠、转小写。
// 端侧（Tauri 前端命令等）用它判断「歌曲是否属于某文件夹」「曲库根归属」，
// 规则必须只有一处实现，避免前端自行维护分隔符/大小写逻辑后产生分歧。

/// 归一化为大小写不敏感的比较键：`\`→`/`、去掉尾部连续 `/`、转小写。
pub fn path_match_key(path_str: &str) -> String {
  let trimmed = path_str.trim();
  let flipped = trimmed.replace('\\', "/");
  let stripped = flipped.trim_end_matches('/');
  stripped.to_lowercase()
}

/// candidate 归一化后与 root 相同，或位于 root 目录内部（`root/` 前缀且目录边界对齐）。
pub fn path_within_root_scope(candidate: &str, root: &str) -> bool {
  let scoped_root = path_match_key(root);
  let normalized_candidate = path_match_key(candidate);
  normalized_candidate == scoped_root
    || normalized_candidate.starts_with(&format!("{scoped_root}/"))
}

/// candidate 归一化后以 root 归一化结果为前缀（不要求目录边界对齐）。
pub fn path_under_root_prefix(candidate: &str, root: &str) -> bool {
  path_match_key(candidate).starts_with(&path_match_key(root))
}

/// 在根路径列表中找出包含 target_path 的最深根目录（归一化路径最长者）。
/// 多个根同时命中且长度并列时，保持列表原始顺序（取先出现者）。
pub fn find_deepest_owning_root<'a>(root_paths: &[&'a str], target_path: &str) -> Option<&'a str> {
  let mut deepest: Option<&'a str> = None;
  let mut deepest_length = -1i64;

  for root_path in root_paths {
    if !path_within_root_scope(target_path, root_path) {
      continue;
    }

    let root_length = path_match_key(root_path).len() as i64;
    if root_length > deepest_length {
      deepest = Some(root_path);
      deepest_length = root_length;
    }
  }

  deepest
}

// ---- SQL LIKE 辅助 ----

/// 转义 LIKE 通配符，配合 `ESCAPE '^'` 使用。
pub fn escape_like(input: &str) -> String { // escape_like
  input
    .replace('^', "^^")
    .replace('%', "^%")
    .replace('_', "^_")
}

/// 生成匹配某个文件夹全部后代路径的前向/反斜杠两个 LIKE 模式。
pub fn descendant_like_patterns(folder_path: &str) -> (String, String) { // descendant_like_patterns
  let ensure_trailing = |separator: char| {
    if folder_path.ends_with('/') || folder_path.ends_with('\\') {
      folder_path.to_string()
    } else {
      format!("{folder_path}{separator}")
    }
  };

  (
    format!("{}%", escape_like(&ensure_trailing('/'))),
    format!("{}%", escape_like(&ensure_trailing('\\'))),
  )
}

/// 生成原始的前向/反斜杠后代前缀（带尾部分隔符），配合大小写敏感的前缀比较使用：
/// `path = ?1 OR substr(path, 1, length(?2)) = ?2 OR substr(path, 1, length(?3)) = ?3`。
/// 避免 SQLite `LIKE` 对 ASCII 大小写不敏感导致大小写不同的目录互相串算。
pub fn descendant_prefixes(folder_path: &str) -> (String, String) {
  let trimmed = folder_path.trim_end_matches(['/', '\\']);
  (format!("{trimmed}/"), format!("{trimmed}\\"))
}

// ---- 扩展名 / 编码判定 ----

pub fn is_supported_library_extension(ext: &str) -> bool { // is_supported_library_extension
  SUPPORTED_LIBRARY_EXTENSIONS.contains(&ext)
}

pub fn is_cue_file_extension(ext: &str) -> bool { // is_cue_file_extension
  CUE_FILE_EXTENSIONS.contains(&ext)
}

/// 依据编码器名或容器名判断是否为无损音频。
pub fn is_lossless_audio(codec: Option<&str>, format: &str) -> bool { // is_lossless_audio
  let normalized = codec.unwrap_or(format).to_lowercase();
  matches!(
    normalized.as_str(),
    "aif" | "aiff" | "alac" | "ape" | "dff" | "dsd" | "dsf" | "flac" | "pcm" | "wav" | "wv"
  )
}

// ---- 整型收窄 ----

pub fn i64_to_bool(v: Option<i64>) -> bool {
  v.unwrap_or(0) != 0
}

pub fn i64_to_u64_opt(v: Option<i64>) -> Option<u64> {
  v.filter(|value| *value >= 0).map(|value| value as u64)
}

pub fn i64_to_u8_opt(v: Option<i64>) -> Option<u8> {
  v.filter(|value| *value >= 0 && *value <= u8::MAX as i64)
    .map(|value| value as u8)
}

pub fn clamp_i64_to_u32(v: i64) -> u32 {
  v.clamp(0, u32::MAX as i64) as u32
}

// ---- 格式分布统计（统计页使用） ----

/// 将容器/编码/扩展名归并到统计页展示用的格式桶。
pub fn format_distribution_bucket( // format_distribution_bucket
  container: Option<&str>,
  codec: Option<&str>,
  format: &str,
) -> &'static str {
  let codec = codec.unwrap_or_default().to_lowercase();
  let container = container.unwrap_or_default().to_lowercase();
  let format = format.to_lowercase();

  // 编码器优先级最高：同一编码可能装在不同容器里。
  let by_codec = match codec.as_str() {
    "flac" => Some("flac"),
    "mp3" => Some("mp3"),
    "alac" => Some("alac"),
    "aac" => Some("aac"),
    "vorbis" => Some("ogg"),
    "opus" => Some("opus"),
    "dsd" => Some("dsd"),
    _ => None,
  };
  if let Some(bucket) = by_codec {
    return bucket;
  }

  match container.as_str() {
    "wav" => "wav",
    "flac" => "flac",
    "mp3" => "mp3",
    "aiff" => "aiff",
    "ogg" => "ogg",
    "mp4" => "aac",
    "ape" => "ape",
    "wavpack" => "wv",
    _ => match format.as_str() {
      "flac" => "flac",
      "mp3" => "mp3",
      "alac" => "alac",
      "aif" | "aiff" => "aiff",
      "aac" | "m4a" | "m4b" | "mp4" => "aac",
      "ogg" | "oga" => "ogg",
      "opus" => "opus",
      "dsf" | "dff" => "dsd",
      "ape" => "ape",
      "wv" => "wv",
      _ => "other",
    },
  }
}

#[cfg(test)]
mod path_tests {
  use super::{
    find_deepest_owning_root, is_network_share_path, legacy_unc_path, path_match_key,
    path_under_root_prefix, path_within_root_scope, strip_windows_path_prefixes,
  };

  #[test]
  fn verbatim_unc_prefix_becomes_plain_unc() {
    assert_eq!(
      strip_windows_path_prefixes(r"\\?\UNC\NAS\Music\song.flac".to_string()),
      r"\\NAS\Music\song.flac"
    );
  }

  #[test]
  fn legacy_broken_unc_prefix_is_repaired() {
    assert_eq!(
      strip_windows_path_prefixes(r"UNC\NAS\Music\song.flac".to_string()),
      r"\\NAS\Music\song.flac"
    );
    assert_eq!(
      legacy_unc_path(r"\\NAS\Music"),
      Some(r"UNC\NAS\Music".to_string())
    );
  }

  #[test]
  fn unc_and_forward_slash_shares_detected_as_network() {
    assert!(is_network_share_path(r"\\NAS\Music\song.flac"));
    assert!(is_network_share_path(r"\\?\UNC\NAS\Music\song.flac"));
    assert!(is_network_share_path("//NAS/Music/song.flac"));
    assert!(!is_network_share_path(r"C:\Music\song.flac"));
  }

  #[test]
  fn match_key_is_case_insensitive_and_separator_uniform() {
    assert_eq!(path_match_key(r"C:\Music\A\song.flac"), "c:/music/a/song.flac");
    assert_eq!(path_match_key("C:/Music/A/"), "c:/music/a");
    assert_eq!(path_match_key("C:/Music/A///"), "c:/music/a");
    assert_eq!(path_match_key(""), "");
  }

  #[test]
  fn within_root_scope_requires_directory_boundary() {
    assert!(path_within_root_scope(r"C:\Music", "C:/Music"));
    assert!(path_within_root_scope(r"C:\Music\a.flac", "C:/Music"));
    assert!(path_within_root_scope(r"C:\MUSIC\Sub\b.flac", r"c:\music"));
    // 目录边界不应对齐：/Music2 不能算在 /Music 内
    assert!(!path_within_root_scope(r"C:\Music2\a.flac", "C:/Music"));
    assert!(!path_within_root_scope(r"D:\Music\a.flac", "C:/Music"));
  }

  #[test]
  fn under_root_prefix_allows_partial_segment() {
    assert!(path_under_root_prefix(r"C:\Music2", r"C:\Music"));
    assert!(!path_under_root_prefix(r"D:\Music2", r"C:\Music"));
  }

  #[test]
  fn deepest_owning_root_prefers_longest_match() {
    let roots = [r"D:\Library", r"D:\Library\Classical"];
    assert_eq!(
      find_deepest_owning_root(&roots, r"d:\library\classical\beethoven.flac"),
      Some(r"D:\Library\Classical")
    );
    assert_eq!(
      find_deepest_owning_root(&roots, r"d:\library\pop\a.flac"),
      Some(r"D:\Library")
    );
    // 并列长度保持原始顺序；无命中返回 None
    assert_eq!(find_deepest_owning_root(&roots, r"E:\Elsewhere\a.flac"), None);
  }
}
