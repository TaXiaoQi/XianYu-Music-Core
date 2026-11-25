// player/stream_cache/decrypt.rs - 已完成的流缓存文件的 CENC 就地解密
//
// 下载线程把整个缓存文件写完后，若是 CENC 加密格式（汽水音乐等音源），
// 调用本函数读入内存解密并写回，使重启后可直接以普通 mp4a 播放。
// 运行时逐字节解密见 player/cenc 的 CencDecryptReader（流式）。

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};

/// 就地解密一个 CENC 加密的缓存文件；非 CENC 文件不修改直接返回 Ok。
pub fn decrypt_cenc_file(path: &std::path::Path, cek: &str) -> Result<(), String> {
    let mut data = std::fs::read(path).map_err(|e| format!("读取缓存文件失败: {}", e))?;
    let key = crate::player::cenc::cek_to_key(cek).map_err(|e| e.to_string())?;
    let decrypted = crate::player::cenc::decrypt_cenc_in_place(&mut data, &key)
        .map_err(|e| format!("CENC 解密失败: {}", e))?;
    if decrypted {
        let mut f = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| format!("打开缓存文件写回失败: {}", e))?;
        f.seek(SeekFrom::Start(0))
            .map_err(|e| format!("定位缓存文件失败: {}", e))?;
        f.write_all(&data)
            .map_err(|e| format!("写回解密文件失败: {}", e))?;
        f.set_len(data.len() as u64)
            .map_err(|e| format!("设置文件长度失败: {}", e))?;
        f.flush().map_err(|e| format!("刷新文件失败: {}", e))?;
    }
    Ok(())
}
