// music/covers.rs - 封面磁盘缓存（弦予原创实现）：缩略图与全尺寸封面双缓存，
// 内容寻址 + 别名索引 + 无图负缓存三件套。
// 缓存键 = 音频文件（归一化路径 + 长度 + 修改时间）的哈希；
// 多首歌曲内嵌同一张图时，实体文件共享，别名文件各指一份。
//
// 核心缓存逻辑以桌面端实现为权威合并；移动端专属入口（SAF fd/路径别名提取、
// 云盘原图直存）保留在文件后半部分。

use super::tags::{find_embedded_picture, read_tagged_file_from_path};
use super::types::{FULL_COVER_IMAGE_CONCURRENCY_LIMIT, THUMBNAIL_IMAGE_CONCURRENCY_LIMIT};
use crate::music::utils::normalize_path;
use crate::remote::cache::{ensure_cached_path, is_remote_uri};
use image::{DynamicImage, ImageFormat};
use lofty::picture::MimeType;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

// 缓存目录总配额；超出后按最久未使用顺序淘汰。
// 移动端降档：封面可随时从音频标签重新提取，1GB 上限足够。
#[cfg(any(target_os = "android", target_os = "ios"))]
const COVER_CACHE_QUOTA_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB
#[cfg(not(any(target_os = "android", target_os = "ios")))]
const COVER_CACHE_QUOTA_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4 GB

const THUMB_SIDE_PIXELS: u32 = 150;
const FULL_SIDE_PIXELS: u32 = 800;
// 全尺寸封面缓存的版本号：嵌入文件名，便于日后整体失效。
const FULL_CACHE_EPOCH: &str = "v3";
const FULL_FALLBACK_FORMAT: &str = "png";
// 全尺寸实体可能以这几种扩展名存在，探测与清理都要遍历。
const FULL_FORMAT_PROBES: [&str; 5] = ["jpg", "png", "webp", "gif", "bmp"];
const ALIAS_MARKER_EXT: &str = "ref";

// 三种可落盘头像的魔数签名。
const JPEG_MAGIC: [u8; 3] = [0xFF, 0xD8, 0xFF];
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// 缩略图并发信号量（全局共享，限制解码/缩放时的内存占用）。
fn thumbnail_semaphore() -> &'static tokio::sync::Semaphore {
    static SEM: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    SEM.get_or_init(|| tokio::sync::Semaphore::new(THUMBNAIL_IMAGE_CONCURRENCY_LIMIT))
}

/// 高清封面并发信号量（全局共享）。
fn full_cover_semaphore() -> &'static tokio::sync::Semaphore {
    static SEM: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    SEM.get_or_init(|| tokio::sync::Semaphore::new(FULL_COVER_IMAGE_CONCURRENCY_LIMIT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_cover_edge_is_capped_for_now_playing_memory() {
        assert_eq!(FULL_SIDE_PIXELS, 800);
    }
}

/// 封面缓存目录（`{cache_root}/covers`）。
pub fn get_cover_cache_dir(cache_root: &Path) -> PathBuf {
    let dir = cache_root.join("covers");
    if !dir.exists() {
        let _ = fs::create_dir_all(&dir);
    }
    dir
}

// ---------- 无图负缓存（对齐 RwaS CoilArtworkRuntime 的「无图缓存 TTL」） ----------

// 对提取不到内嵌封面的音频，在 TTL 窗口内不再重复尝试（避免列表滚动/重扫时
// 对无封面文件反复 FFI + 解码 + 写盘），TTL 过后才允许重试，以便用户补写
// 封面后能回流。
const MISS_TTL_SECS: u64 = 3600;
const MISS_CACHE_CAPACITY: usize = 100_000;

type MissLedger = HashMap<String, u64>;

fn miss_ledger() -> &'static Mutex<MissLedger> {
    static LEDGER: OnceLock<Mutex<MissLedger>> = OnceLock::new();
    LEDGER.get_or_init(|| Mutex::new(HashMap::new()))
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

// 记录一次「这曲子没图」；台账超限时整体清空重建。
fn record_miss(key: &str, at_secs: u64) {
    if let Ok(mut ledger) = miss_ledger().lock() {
        ledger.insert(key.to_owned(), at_secs);
        if ledger.len() > MISS_CACHE_CAPACITY {
            ledger.clear();
        }
    }
}

// TTL 之内查过且无图：返回 true，调用方直接放弃。
fn miss_recorded_recently(key: &str, now_secs: u64) -> bool {
    miss_ledger()
        .lock()
        .ok()
        .and_then(|ledger| ledger.get(key).copied())
        .map(|at| now_secs.saturating_sub(at) < MISS_TTL_SECS)
        .unwrap_or(false)
}

/// 清空无图负缓存（用户主动重扫/清缓存时释放记忆，允许立即可重试提取）。
pub fn clear_no_cover_negative_cache() {
    if let Ok(mut ledger) = miss_ledger().lock() {
        ledger.clear();
    }
}

// ---------- 指纹与缓存文件体检 ----------

// 源文件指纹：归一化路径 + 长度 + 修改时间（纳秒），喂给 SHA-256。
fn fingerprint_source_file(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalize_path(&path.to_string_lossy()).as_bytes());

    if let Ok(stat) = fs::metadata(path) {
        let touched = stat.modified().unwrap_or(SystemTime::now());
        let elapsed = touched
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        hasher.update(stat.len().to_be_bytes());
        hasher.update(elapsed.as_nanos().to_be_bytes());
    }

    hex::encode(hasher.finalize())
}

// 图片内容指纹：同一张内嵌图在所有歌曲间共享实体。
fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

// 存在且非空才算健康；半截文件一律视同缺失。
fn usable_cache_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|stat| stat.is_file() && stat.len() > 0)
        .unwrap_or(false)
}

// 能否真的解码成图像——文件大小合格不代表内容合格。
fn image_readable(path: &Path) -> bool {
    image::open(path).is_ok()
}

// ---------- 临时文件 + 原子落盘 ----------

// 与目标同目录、同名的临时兄弟文件，带纳秒级随机后缀。
fn temp_sibling_of(dest: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let file_name = dest.file_name().unwrap_or_default().to_string_lossy();
    dest.with_file_name(format!("{file_name}.{nonce}.tmp"))
}

// 写临时文件 → 原子换名。若目标已有一张能解码的旧图则保留旧图；
// 换名失败但目标可解码时同样按旧图成功处理。
fn swap_into_place(
    produce: impl FnOnce(&mut BufWriter<fs::File>) -> std::io::Result<()>,
    dest: &Path,
) -> Option<String> {
    let temp_path = temp_sibling_of(dest);
    let mut writer = BufWriter::new(fs::File::create(&temp_path).ok()?);

    if produce(&mut writer).is_err() || writer.flush().is_err() {
        drop(writer);
        let _ = fs::remove_file(&temp_path);
        return None;
    }
    drop(writer);

    if dest.exists() {
        if image_readable(dest) {
            let _ = fs::remove_file(&temp_path);
            return Some(dest.to_string_lossy().into_owned());
        }
        let _ = fs::remove_file(dest);
    }

    if fs::rename(&temp_path, dest).is_err() {
        let _ = fs::remove_file(&temp_path);
        if image_readable(dest) {
            return Some(dest.to_string_lossy().into_owned());
        }
        return None;
    }

    Some(dest.to_string_lossy().into_owned())
}

// 原始字节直存。
fn persist_raw_bytes(bytes: &[u8], dest: &Path) -> Option<String> {
    swap_into_place(|writer| writer.write_all(bytes), dest)
}

// 渲染后的图像按指定格式编码落盘。
fn persist_rendered_image(img: &DynamicImage, format: ImageFormat, dest: &Path) -> Option<String> {
    swap_into_place(
        |writer| {
            img.write_to(writer, format)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
        },
        dest,
    )
}

// ---------- 缓存命名与别名索引 ----------

// 缩略图实体固定以 jpg 落地（重编码后与源格式无关）。
fn thumb_entity_name(body_key: &str) -> String {
    format!("{body_key}_thumb_{THUMB_SIDE_PIXELS}.jpg")
}

// 全尺寸实体的文件名主干（不含扩展名）。
fn full_entity_stem(body_key: &str) -> String {
    format!("{body_key}_full_{FULL_CACHE_EPOCH}")
}

fn thumb_alias_file(cache_dir: &Path, source_key: &str) -> PathBuf {
    cache_dir.join(format!(
        "{source_key}_thumb_{THUMB_SIDE_PIXELS}.{ALIAS_MARKER_EXT}"
    ))
}

fn full_alias_file(cache_dir: &Path, source_key: &str) -> PathBuf {
    cache_dir.join(format!(
        "{source_key}_full_{FULL_CACHE_EPOCH}.{ALIAS_MARKER_EXT}"
    ))
}

// 内嵌图片的 MIME → 缓存扩展名；认不出的 MIME 一律放弃。
fn mime_to_extension(mime: Option<&MimeType>) -> Option<&'static str> {
    let Some(mime) = mime else {
        return None;
    };
    if mime == &MimeType::Jpeg {
        return Some("jpg");
    }
    if mime == &MimeType::Png {
        return Some("png");
    }
    if mime == &MimeType::Gif {
        return Some("gif");
    }
    if mime == &MimeType::Bmp {
        return Some("bmp");
    }
    match mime {
        MimeType::Unknown(value) if value.eq_ignore_ascii_case("image/webp") => Some("webp"),
        _ => None,
    }
}

// 读别名指向的实体；别名为空、实体损坏时顺手清掉坏文件。
fn read_alias_target(cache_dir: &Path, alias: &Path) -> Option<String> {
    let raw = fs::read_to_string(alias).ok()?;
    let target_name = raw.trim();
    if target_name.is_empty() {
        let _ = fs::remove_file(alias);
        return None;
    }

    let entity = cache_dir.join(target_name);
    if usable_cache_file(&entity) {
        return Some(entity.to_string_lossy().into_owned());
    }

    let _ = fs::remove_file(alias);
    if entity.exists() {
        let _ = fs::remove_file(&entity);
    }
    None
}

// 把别名文件指向某个实体（内容即实体文件名）。
fn bind_alias(alias: &Path, entity: &Path) -> Option<()> {
    let entity_name = entity.file_name()?.to_string_lossy().into_owned();
    persist_raw_bytes(entity_name.as_bytes(), alias)?;
    Some(())
}

// 清掉给定主干下所有解不开的全尺寸实体。
fn prune_unreadable_full_entities(cache_dir: &Path, stem: &str) {
    for ext in FULL_FORMAT_PROBES {
        let candidate = cache_dir.join(format!("{stem}.{ext}"));
        if candidate.exists() && !image_readable(&candidate) {
            let _ = fs::remove_file(candidate);
        }
    }
}

// 在已知扩展名里找第一个还健康的全尺寸实体。
fn hunt_existing_full_entity(cache_dir: &Path, stem: &str) -> Option<String> {
    for ext in FULL_FORMAT_PROBES {
        let candidate = cache_dir.join(format!("{stem}.{ext}"));
        if !candidate.exists() {
            continue;
        }
        if usable_cache_file(&candidate) {
            return Some(candidate.to_string_lossy().into_owned());
        }
        let _ = fs::remove_file(candidate);
    }
    None
}

// ---------- 缩略图构建 ----------

enum ThumbnailOutcome {
    Ready(String),
    // 读不出标签或没有内嵌图，或图解不开：应记入负缓存。
    NoPictureEmbedded,
    // 有图但落盘失败：保持静默，不污染负缓存。
    NotStorable,
}

// 统一骨架：别名命中 → 负缓存检查 → 制造实体并绑定别名。
// `hash_source` 参与指纹计算（真实文件含长度/修改时间；SAF content URI
// 等稳定主键按字符串本身哈希），`read_path` 是标签读取来源。
fn resolve_or_fabricate_thumbnail(
    source_key: &str,
    hash_source: &Path,
    read_path: &Path,
    cache_dir: &Path,
) -> Option<String> {
    let source_key_hash = fingerprint_source_file(hash_source);
    let alias = thumb_alias_file(cache_dir, &source_key_hash);

    if let Some(hit) = read_alias_target(cache_dir, &alias) {
        return Some(hit);
    }

    let seen_at = current_unix_seconds();
    let identity = normalize_path(source_key);
    if miss_recorded_recently(&identity, seen_at) {
        return None;
    }

    match fabricate_thumbnail(read_path, cache_dir) {
        ThumbnailOutcome::Ready(entity) => {
            let _ = bind_alias(&alias, Path::new(&entity));
            Some(entity)
        }
        ThumbnailOutcome::NoPictureEmbedded => {
            record_miss(&identity, seen_at);
            None
        }
        ThumbnailOutcome::NotStorable => None,
    }
}

fn fabricate_thumbnail(path: &Path, cache_dir: &Path) -> ThumbnailOutcome {
    let Some(embedded) = read_tagged_file_from_path(path)
        .ok()
        .and_then(|tagged| find_embedded_picture(&tagged).map(|pic| pic.data().to_vec()))
    else {
        return ThumbnailOutcome::NoPictureEmbedded;
    };

    let body_key = digest_bytes(&embedded);
    let entity = cache_dir.join(thumb_entity_name(&body_key));

    if usable_cache_file(&entity) {
        return ThumbnailOutcome::Ready(entity.to_string_lossy().into_owned());
    }
    if entity.exists() {
        let _ = fs::remove_file(&entity);
    }

    let Ok(decoded) = image::load_from_memory(&embedded) else {
        return ThumbnailOutcome::NoPictureEmbedded;
    };

    let shrunk = decoded.resize(
        THUMB_SIDE_PIXELS,
        THUMB_SIDE_PIXELS,
        image::imageops::FilterType::Triangle,
    );
    match persist_rendered_image(&shrunk, ImageFormat::Jpeg, &entity) {
        Some(persisted) => ThumbnailOutcome::Ready(persisted),
        None => ThumbnailOutcome::NotStorable,
    }
}

/// 读取（或生成）歌曲内嵌封面的缩略图缓存路径。
pub fn get_or_create_thumbnail(path: &Path, cache_dir: &Path) -> Option<String> {
    resolve_or_fabricate_thumbnail(&path.to_string_lossy(), path, path, cache_dir)
}

/// 通用别名封面提取：从 `read_path` 读取音频标签中的内嵌封面，但缓存别名按
/// `source_key`（稳定主键，如 SAF content URI）哈希。
///
/// 这样扫描阶段产出的缩略图，能在播放/列表展示时按同一 source_key 命中，
/// 避免运行时再去读无法直接以文件路径访问的 content URI。
fn get_or_create_thumbnail_aliased(
    source_key: &str,
    read_path: &Path,
    cache_dir: &Path,
) -> Option<String> {
    resolve_or_fabricate_thumbnail(source_key, Path::new(source_key), read_path, cache_dir)
}

/// 扫描 SAF 音频时从已打开的 `content://` fd 提取内嵌封面并写入封面缓存。
///
/// 读文件走 `/proc/self/fd/{fd}`，别名按 `source_path`（content URI）哈希，
/// 与播放/列表时 `get_song_cover_thumbnail` 按同路径查找所命中的别名一致。
pub fn get_or_create_thumbnail_from_fd(
    source_path: &str,
    fd: i32,
    cache_dir: &Path,
) -> Option<String> {
    let fd_path = format!("/proc/self/fd/{fd}");
    get_or_create_thumbnail_aliased(source_path, Path::new(&fd_path), cache_dir)
}

/// 扫描 SAF 音频时从物化到应用内部存储的真实文件路径提取封面。
///
/// 别名仍按 `source_key`（content URI）哈希，与展示路径一致；读取走稳定真实路径，
/// 比 `/proc/self/fd/{fd}` 更可靠，适用于复制成功后统一走路径式解析的场景。
pub fn get_or_create_thumbnail_from_path(
    source_key: &str,
    real_path: &Path,
    cache_dir: &Path,
) -> Option<String> {
    get_or_create_thumbnail_aliased(source_key, real_path, cache_dir)
}

/// 从内嵌封面字节直接生成缩略图实体（云端流缓存元数据探针用）。
/// 不做负缓存、不绑定源文件别名；命中既有实体直接复用。
pub fn save_embedded_thumb_from_bytes(bytes: &[u8], cache_dir: &Path) -> Option<String> {
    let body_key = digest_bytes(bytes);
    let entity = cache_dir.join(thumb_entity_name(&body_key));

    if usable_cache_file(&entity) {
        return Some(entity.to_string_lossy().into_owned());
    }
    if entity.exists() {
        let _ = fs::remove_file(&entity);
    }

    let Ok(decoded) = image::load_from_memory(bytes) else {
        return None;
    };
    let shrunk = decoded.resize(
        THUMB_SIDE_PIXELS,
        THUMB_SIDE_PIXELS,
        image::imageops::FilterType::Triangle,
    );
    persist_rendered_image(&shrunk, ImageFormat::Jpeg, &entity)
}

// ---------- 全尺寸封面构建 ----------

enum FullCoverOutcome {
    Ready(String),
    // 落盘失败：与既有行为一致，不写入负缓存。
    GiveUpQuietly,
    // 无图或图解不开且无可靠扩展名：应记入负缓存。
    NoPictureEmbedded,
}

/// 读取（或生成）歌曲内嵌封面的全尺寸缓存路径。
pub fn get_or_create_full_cover(path: &Path, cache_dir: &Path) -> Option<String> {
    let source_key = fingerprint_source_file(path);
    let alias = full_alias_file(cache_dir, &source_key);

    if let Some(hit) = read_alias_target(cache_dir, &alias) {
        return Some(hit);
    }

    let seen_at = current_unix_seconds();
    let identity = normalize_path(&path.to_string_lossy());
    if miss_recorded_recently(&identity, seen_at) {
        return None;
    }

    let Some((embedded, mime)) = read_tagged_file_from_path(path).ok().and_then(|tagged| {
        find_embedded_picture(&tagged).map(|pic| (pic.data().to_vec(), pic.mime_type().cloned()))
    }) else {
        record_miss(&identity, seen_at);
        return None;
    };

    match fabricate_full_cover(&embedded, mime.as_ref(), cache_dir) {
        FullCoverOutcome::Ready(entity) => {
            let _ = bind_alias(&alias, Path::new(&entity));
            Some(entity)
        }
        FullCoverOutcome::GiveUpQuietly => None,
        FullCoverOutcome::NoPictureEmbedded => {
            record_miss(&identity, seen_at);
            None
        }
    }
}

fn fabricate_full_cover(
    embedded: &[u8],
    mime: Option<&MimeType>,
    cache_dir: &Path,
) -> FullCoverOutcome {
    let body_key = digest_bytes(embedded);
    let stem = full_entity_stem(&body_key);

    if let Some(existing) = hunt_existing_full_entity(cache_dir, &stem) {
        return FullCoverOutcome::Ready(existing);
    }
    prune_unreadable_full_entities(cache_dir, &stem);

    let Ok(decoded) = image::load_from_memory(embedded) else {
        // 图像解不开：原始字节直存，交给前端自行容错。
        return match mime_to_extension(mime) {
            Some(ext) => {
                let entity = cache_dir.join(format!("{stem}.{ext}"));
                match persist_raw_bytes(embedded, &entity) {
                    Some(persisted) => FullCoverOutcome::Ready(persisted),
                    None => FullCoverOutcome::GiveUpQuietly,
                }
            }
            None => FullCoverOutcome::NoPictureEmbedded,
        };
    };

    let oversized = decoded.width() > FULL_SIDE_PIXELS || decoded.height() > FULL_SIDE_PIXELS;

    // 原始尺寸未超限时按原始格式与字节直接缓存，省一次重编码。
    if !oversized {
        if let Some(ext) = mime_to_extension(mime) {
            let entity = cache_dir.join(format!("{stem}.{ext}"));
            return match persist_raw_bytes(embedded, &entity) {
                Some(persisted) => FullCoverOutcome::Ready(persisted),
                None => FullCoverOutcome::GiveUpQuietly,
            };
        }
    }

    // 将显示封面钳制到高质量边长，以免把原始数千像素的大图解码进内存。
    let display = if oversized {
        decoded.resize(
            FULL_SIDE_PIXELS,
            FULL_SIDE_PIXELS,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        decoded
    };

    let entity = cache_dir.join(format!("{stem}.{FULL_FALLBACK_FORMAT}"));
    match persist_rendered_image(&display, ImageFormat::Png, &entity) {
        Some(persisted) => FullCoverOutcome::Ready(persisted),
        None => FullCoverOutcome::GiveUpQuietly,
    }
}

// ---------- 缓存维护 ----------

// 清空目录内的全部文件（不含子目录）。
fn empty_directory(cache_dir: &Path) -> Result<(), String> {
    if !cache_dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(cache_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let victim = entry.path();
        if victim.is_file() {
            fs::remove_file(&victim).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 后台线程按最近访问时间从旧到新逐个删除，直至总量回到配额以内。
pub fn run_cache_cleanup(cache_dir: &Path) {
    let cache_dir = cache_dir.to_path_buf();
    let storage_quota = COVER_CACHE_QUOTA_BYTES;

    std::thread::spawn(move || {
        let Ok(entries) = fs::read_dir(&cache_dir) else {
            return;
        };
        let mut inventory: Vec<(PathBuf, u64, SystemTime)> = entries
            .flatten()
            .filter_map(|entry| {
                let stat = entry.metadata().ok()?;
                let footprint = stat.len();
                let last_used = stat
                    .accessed()
                    .or(stat.modified())
                    .unwrap_or(SystemTime::now());
                Some((entry.path(), footprint, last_used))
            })
            .collect();

        inventory.sort_by_key(|&(_, _, last_used)| last_used);
        let mut total_bytes: u64 = inventory.iter().map(|&(_, footprint, _)| footprint).sum();

        for (victim, footprint, _) in inventory {
            if total_bytes <= storage_quota {
                break;
            }
            if fs::remove_file(&victim).is_ok() {
                total_bytes = total_bytes.saturating_sub(footprint);
            }
        }
    });
}

/// 清空封面缓存（含无图负缓存）。
pub fn clear_cover_cache(cache_dir: &Path) -> Result<(), String> {
    empty_directory(cache_dir)?;
    clear_no_cover_negative_cache();
    Ok(())
}

// ---------- 图片魔数与字节直存 ----------

// 魔数探测：JPEG / PNG / RIFF-WEBP 三种；认不出返回 None。
fn sniff_image_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&JPEG_MAGIC) {
        return Some("jpg");
    }
    if bytes.starts_with(&PNG_MAGIC) {
        return Some("png");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

/// 从内存字节落盘自动识别的歌手头像（供扫描/远程链路复用）。
pub fn save_artist_avatar_auto(bytes: &[u8], covers_dir: &Path) -> Option<String> {
    let kind = sniff_image_kind(bytes)?;

    if !covers_dir.exists() && fs::create_dir_all(covers_dir).is_err() {
        return None;
    }

    let digest = digest_bytes(bytes);
    let target = covers_dir.join(format!("artist-avatar-auto-{digest}.{kind}"));
    let stored = persist_raw_bytes(bytes, &target)?;
    Some(normalize_path(&stored))
}

/// 云盘曲目内嵌封面落盘（网盘头部预探专用，对齐桌面 save_embedded_thumb）：
/// 按图内容指纹共享缓存（同名即同图，跨曲目/跨会话复用），返回绝对路径。
/// 直接存原图字节（内嵌封面一般 ≤1MB，无需缩放，Image.file 自行解码）。
pub fn save_embedded_cover_from_bytes(bytes: &[u8], covers_dir: &Path) -> Option<String> {
    let ext = if bytes.starts_with(&JPEG_MAGIC) {
        "jpg"
    } else if bytes.starts_with(&PNG_MAGIC) {
        "png"
    } else if bytes.starts_with(b"RIFF") && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
        "webp"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else {
        return None;
    };

    let sha256_hex = digest_bytes(bytes);
    let target_path = covers_dir.join(format!("cloud-embed-{}.{}", sha256_hex, ext));

    if !covers_dir.exists() {
        if fs::create_dir_all(covers_dir).is_err() {
            return None;
        }
    }

    persist_raw_bytes(bytes, &target_path).map(|stored| normalize_path(&stored))
}

// ---------- FRB 入口（宿主后台线程调用） ----------

/// 获取歌曲缩略图封面（远程 URI 先保证缓存到本地）。
///
/// 成功后回写 `songs.cover_thumb_path`。
pub async fn get_song_cover_thumbnail(
    cache_root: PathBuf,
    db_conn: Arc<Mutex<rusqlite::Connection>>,
    path: String,
) -> Result<String, String> {
    let _permit = thumbnail_semaphore()
        .acquire()
        .await
        .map_err(|e| e.to_string())?;
    let source_path = if is_remote_uri(&path) {
        ensure_cached_path(&cache_root, db_conn.clone(), &path).await?
    } else {
        path.clone()
    };

    let cache_dir = get_cover_cache_dir(&cache_root);
    let p_buf = PathBuf::from(&source_path);

    let result = std::thread::spawn(move || get_or_create_thumbnail(&p_buf, &cache_dir))
        .join()
        .map_err(|_| "缩略图生成线程异常".to_string())?;

    if let Some(cache_path_str) = result {
        if !cache_path_str.is_empty() {
            if let Ok(conn) = db_conn.lock() {
                let _ = conn.execute(
                    "UPDATE songs SET cover_thumb_path = ?1 WHERE path = ?2",
                    rusqlite::params![&cache_path_str, &path],
                );
            }
        }
        return Ok(cache_path_str);
    }
    Ok(String::new())
}

/// 获取歌曲高清封面（远程 URI 先保证缓存到本地）。
pub async fn get_song_cover(
    cache_root: PathBuf,
    db_conn: Arc<Mutex<rusqlite::Connection>>,
    path: String,
) -> Result<String, String> {
    let _permit = full_cover_semaphore()
        .acquire()
        .await
        .map_err(|e| e.to_string())?;
    let source_path = if is_remote_uri(&path) {
        ensure_cached_path(&cache_root, db_conn, &path).await?
    } else {
        path.clone()
    };

    let cache_dir = get_cover_cache_dir(&cache_root);
    let p_buf = PathBuf::from(&source_path);

    let result = std::thread::spawn(move || get_or_create_full_cover(&p_buf, &cache_dir))
        .join()
        .map_err(|_| "高清封面生成线程异常".to_string())?;

    if let Some(cache_path_str) = result {
        return Ok(cache_path_str);
    }
    Ok(String::new())
}
