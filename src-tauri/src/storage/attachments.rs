//! attachments —— 通用附件通道（上传 / 存储 / 图像物化）
//!
//! 职责（不做业务判断）：
//!   1. 把用户选中的文件复制进**会话内稳定落点**，返回 [`Attachment`] 引用；
//!   2. 请求前把图片附件物化为 [`ImagePart`]（base64），供多模态模型消费；
//!      超限的图片先缩边再重编码 JPEG，收敛到目标体积。
//!
//! 存储落点：`data_dir/session-files/<会话id>/`（有会话）；
//!           无会话 → `data_dir/uploads/`。文件一律**快照复制**（`原名_时间戳.ext`），
//!           保证原始文件被移动/删除后附件仍可用。
//!
//! 设计边界：本模块只管「文件 → 引用 → 像素」的搬运与规范化，不解析文件内容、
//! 不做识别。图是否真的送给模型由 orchestrator 按模型视觉能力决定（见下文
//! `materialize_images` 的调用方）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::llm::ImagePart;

/// 单个附件在会话内的稳定引用（随事件 / 请求历史流转；不含字节）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// 会话内稳定路径（复制后的落点；前端用 asset URL 读它做缩略图）。
    pub path: String,
    /// 展示名（用户原始文件名）。
    pub name: String,
    /// MIME 类型（由扩展名推断；未知为 application/octet-stream）。
    #[serde(default)]
    pub mime_type: String,
    /// 字节数。
    #[serde(default)]
    pub size: u64,
}

impl Attachment {
    /// 是否图片（据 MIME 判定）。
    pub fn is_image(&self) -> bool {
        self.mime_type.starts_with("image/")
    }
}

/// 允许的图片 MIME 白名单（仅用于「是否走图像物化」的判断，非上传白名单）。
const ALLOWED_IMAGE_MIME: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/jpg",
    "image/webp",
    "image/gif",
    "image/bmp",
];

/// 上传文件数上限（单次调用）。
pub const MAX_FILES: usize = 9;
/// 单文件体积上限（原始字节）。超限拒绝（大图由物化阶段压缩）。
pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// 物化目标：base64 字符数上限（≈ Anthropic 5MB 上限的安全余量，
/// 同时压住 payload 体积与 token 成本）。
const TARGET_BASE64_CHARS: usize = 850 * 1024;
/// 缩边上限（Anthropic 建议的 1568 长边）。
const MAX_EDGE: u32 = 1568;

/// 会话附件落点目录（不建目录，调用方按需 create_dir_all）。
///
/// `conversation_id` 会拼进路径：非空时必须只含字母数字与 `- _`，
/// 防 `..` / 分隔符 / 盘符把落点带出数据目录。空串仍表示无会话的 uploads。
pub fn session_files_dir(conversation_id: &str) -> Result<PathBuf, String> {
    if conversation_id.trim().is_empty() {
        return Ok(crate::config::data_dir().join("uploads"));
    }
    if !conversation_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "conversationId 非法（仅允许字母数字与 - _）：{conversation_id:?}"
        ));
    }
    Ok(crate::config::data_dir()
        .join("session-files")
        .join(conversation_id))
}

/// 由扩展名推断 MIME（未知 → application/octet-stream）。
pub fn mime_for_path(path: &Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("pdf") => "application/pdf",
        Some("txt") | Some("md") => "text/plain",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// 是否白名单图片 MIME。
pub fn is_allowed_image_mime(mime: &str) -> bool {
    ALLOWED_IMAGE_MIME.contains(&mime.to_ascii_lowercase().as_str())
}

/// 文件名净化：只保留跨平台 basename，去掉文件系统保留字符与控制字符，
/// 去掉 Windows 尾随的点/空格。返回空串表示不可用（调用方回退默认名）。
pub fn sanitize_file_name(name: &str) -> String {
    let base = name.replace('\\', "/");
    let base = base.rsplit('/').next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| {
            let code = *c as u32;
            !(code <= 0x1f || code == 0x7f)
                && !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    trimmed.to_string()
}

/// 生成带时间戳的唯一落盘名（保留扩展名）。
pub fn unique_name(base: &str, ext: &str) -> String {
    let suffix = format!(
        "_{}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        std::process::id()
    );
    let stem = if base.is_empty() { "file" } else { base };
    if ext.is_empty() {
        format!("{stem}{suffix}")
    } else {
        format!("{stem}{suffix}.{ext}")
    }
}

/// 把一个已存在的文件复制进 `dir`，返回附件引用。
///
/// 不做存在性之外的安全校验（绝对路径 / 敏感路径由命令层把关）。
pub fn copy_into(src: &Path, dir: &Path) -> Result<Attachment, String> {
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .map(sanitize_file_name)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "file".into());
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let base = Path::new(&name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let dest = dir.join(unique_name(base, ext));

    let size = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    std::fs::copy(src, &dest).map_err(|e| format!("复制失败: {e}"))?;

    Ok(Attachment {
        path: dest.to_string_lossy().into_owned(),
        name,
        mime_type: mime_for_path(src),
        size,
    })
}

/// 引用一个**已存在**的文件作为附件（不复制）。
///
/// 用于工具产物（如 MAA 截图）——文件本就落在 PRTS 数据目录里，
/// 附件只作引用流转。调用方须保证该路径此后不再被覆写。
pub fn reference_existing(src: &Path) -> Attachment {
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .map(sanitize_file_name)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "file".into());
    Attachment {
        path: src.to_string_lossy().into_owned(),
        name,
        mime_type: mime_for_path(src),
        size: std::fs::metadata(src).map(|m| m.len()).unwrap_or(0),
    }
}

/// 把内存中的 base64 blob 落盘（粘贴 / 录音等无路径来源）。
pub fn write_blob_into(
    dir: &Path,
    name: &str,
    base64_data: &str,
    mime_type: &str,
) -> Result<Attachment, String> {
    let bytes = base64_decode::decode(base64_data).map_err(|e| format!("base64 解码失败: {e}"))?;
    if bytes.is_empty() {
        return Err("空数据".into());
    }
    let clean = sanitize_file_name(name);
    let ext = Path::new(&clean)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let base = Path::new(&clean)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("blob");
    let dest = dir.join(unique_name(base, ext));
    std::fs::write(&dest, &bytes).map_err(|e| format!("写入失败: {e}"))?;
    Ok(Attachment {
        path: dest.to_string_lossy().into_owned(),
        name: if clean.is_empty() { "blob".into() } else { clean },
        mime_type: mime_type.to_string(),
        size: bytes.len() as u64,
    })
}

/// 把图片路径列表物化为 [`ImagePart`] 列表（best-effort：失败项跳过）。
///
/// 调用方（orchestrator）负责按模型视觉能力决定是否调用；本函数只做编码。
pub fn materialize_images(paths: &[String]) -> Vec<ImagePart> {
    paths
        .iter()
        .filter_map(|p| encode_image(Path::new(p)).ok())
        .collect()
}

/// 读取并规范化单张图片：小图原样 base64；超限图缩边 + 逐档 JPEG 质量。
fn encode_image(path: &Path) -> Result<ImagePart, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读取失败: {e}"))?;
    let mime = mime_for_path(path);
    let b64 = base64_encode(&bytes);
    if b64.len() <= TARGET_BASE64_CHARS {
        return Ok(ImagePart { data: b64, mime_type: mime });
    }
    if !is_allowed_image_mime(&mime) {
        return Err("非可压缩图片且超过体积上限".into());
    }

    let img = image::open(path).map_err(|e| format!("图像解码失败: {e}"))?;
    // 逐轮：先按长边缩到 MAX_EDGE，质量档递增下降；仍超限则尺寸再降一档。
    let mut scale = 1.0f32;
    for _ in 0..4 {
        let edge = ((MAX_EDGE as f32) * scale).round().max(64.0) as u32;
        let resized = fit_within(&img, edge);
        for quality in [82u8, 74, 66, 58] {
            if let Ok(buf) = encode_jpeg(&resized, quality) {
                let b64 = base64_encode(&buf);
                if b64.len() <= TARGET_BASE64_CHARS {
                    return Ok(ImagePart { data: b64, mime_type: "image/jpeg".into() });
                }
            }
        }
        scale *= 0.85;
    }
    Err("图像压缩后仍超过体积上限".into())
}

/// 等比缩放到长边不超过 `edge`（已足够小则原样返回引用）。
fn fit_within(img: &image::DynamicImage, edge: u32) -> image::DynamicImage {
    let (w, h) = (img.width(), img.height());
    if w <= edge && h <= edge {
        return img.clone();
    }
    let ratio = (edge as f32) / (w.max(h) as f32);
    let nw = ((w as f32) * ratio).round().max(1.0) as u32;
    let nh = ((h as f32) * ratio).round().max(1.0) as u32;
    img.resize_exact(nw, nh, image::imageops::FilterType::Lanczos3)
}

/// 以指定质量编码为 JPEG 字节。
fn encode_jpeg(img: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode_image(img)
        .map_err(|e| format!("JPEG 编码失败: {e}"))?;
    Ok(buf)
}

/// 标准 base64 编码（自实现，避免引入仅此一处所需的依赖）。
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// 标准 base64 解码（自实现，容忍尾部填充与空白）。
mod base64_decode {
    pub fn decode(input: &str) -> Result<Vec<u8>, String> {
        fn val(c: u8) -> Option<u8> {
            match c {
                b'A'..=b'Z' => Some(c - b'A'),
                b'a'..=b'z' => Some(c - b'a' + 26),
                b'0'..=b'9' => Some(c - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        }
        // 去掉 data URL 前缀与空白
        let raw = input.trim();
        let raw = raw.split(',').last().unwrap_or(raw);
        let filtered: Vec<u8> = raw.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        let mut out = Vec::with_capacity(filtered.len() / 4 * 3);
        let mut buf = 0u32;
        let mut bits = 0u32;
        for &c in &filtered {
            if c == b'=' {
                break;
            }
            let v = val(c).ok_or_else(|| format!("非法 base64 字符: {}", c as char))?;
            buf = (buf << 6) | (v as u32);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(((buf >> bits) & 0xff) as u8);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_files_dir_rejects_path_components() {
        for bad in ["..", "../x", "a/b", "a\\b", "C:\\x", "a b"] {
            assert!(
                session_files_dir(bad).is_err(),
                "conversationId 应拒绝路径成分：{bad:?}"
            );
        }
        assert!(session_files_dir("").is_ok(), "空串 = uploads 仍可用");
        assert!(session_files_dir("conv-01_a").is_ok());
    }

    /// 性能标定（纯 CPU，不碰游戏）：对数据目录里最新一张 MAA 截图量
    /// 「路径 → 像素」的耗时。跑法：
    /// `cargo test --lib materialize_timing -- --ignored --nocapture`
    #[test]
    #[ignore = "性能标定：需数据目录里已有截图"]
    fn materialize_timing() {
        let dir = crate::config::data_dir().join("maa").join("screenshots");
        let mut shots: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().starts_with("screenshot_"))
                            .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        shots.sort();
        let Some(p) = shots.pop() else {
            println!("目录里没有截图：{}", dir.display());
            return;
        };
        let src_bytes = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        let path = p.to_string_lossy().into_owned();
        let t0 = std::time::Instant::now();
        let parts = materialize_images(&[path.clone()]);
        let dt = t0.elapsed();
        println!("文件 {path}（{src_bytes} 字节）");
        println!(
            "物化耗时 {dt:?}｜份数 {}｜base64 字符数 {}",
            parts.len(),
            parts.first().map(|x| x.data.len()).unwrap_or(0)
        );
    }

    #[test]
    fn sanitize_strips_path_and_reserved() {
        assert_eq!(sanitize_file_name("C:\\a\\b/截图.png"), "截图.png");
        assert_eq!(sanitize_file_name("a<b>c?.txt"), "abc.txt");
        assert_eq!(sanitize_file_name("  ..name.  "), "name");
        assert_eq!(sanitize_file_name(""), "");
    }

    #[test]
    fn mime_by_extension() {
        assert_eq!(mime_for_path(Path::new("x.PNG")), "image/png");
        assert_eq!(mime_for_path(Path::new("x.jpeg")), "image/jpeg");
        assert_eq!(mime_for_path(Path::new("x.bin")), "application/octet-stream");
    }

    #[test]
    fn base64_round_trip() {
        for sample in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            let enc = base64_encode(sample);
            let dec = base64_decode::decode(&enc).unwrap();
            assert_eq!(dec, sample, "sample={sample:?} enc={enc}");
        }
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_decode::decode("Zm9vYmFy").unwrap(), b"foobar");
    }

    #[test]
    fn unique_name_keeps_ext() {
        let n = unique_name("shot", "png");
        assert!(n.starts_with("shot_") && n.ends_with(".png"), "got {n}");
    }

    #[test]
    fn attachment_is_image_by_mime() {
        let a = Attachment { path: "p".into(), name: "n".into(), mime_type: "image/png".into(), size: 1 };
        assert!(a.is_image());
        let b = Attachment { path: "p".into(), name: "n".into(), mime_type: "application/pdf".into(), size: 1 };
        assert!(!b.is_image());
    }
}
