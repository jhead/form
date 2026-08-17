//! Attachment records (F3).
//!
//! The blob is content-addressed at `{dataDir}/attachments/{sha256}` and written once; a row
//! per attachment points at it. Two people pasting the same screenshot into two sessions get
//! two records and one file.

use std::path::PathBuf;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{CoreError, Result};
use crate::protocol::{now_ms, Attachment};

use super::{new_id, Store};

/// F3.6 — anything larger is rejected with an inline reason rather than silently truncated.
pub const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;

/// Exact mime types plus the `text/` prefix. Deliberately conservative: the harness executes
/// nothing, but this is the list a real tool layer would inherit.
const ALLOWED_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/heic",
    "image/heif",
    "image/tiff",
    "image/bmp",
    "image/svg+xml",
    "application/pdf",
    "application/json",
    "application/xml",
    "application/toml",
    "application/x-yaml",
    "application/octet-stream",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttachmentSource {
    /// A file on disk, copied into the content store.
    Path(String),
    /// Raw bytes — the paste and drag-and-drop paths (F3.1).
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddAttachment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub source: AttachmentSource,
    pub filename: String,
    pub mime: String,
}

impl Store {
    pub fn add_attachment(&self, req: AddAttachment) -> Result<Attachment> {
        if !mime_allowed(&req.mime) {
            return Err(CoreError::AttachmentRejected {
                reason: format!("unsupported type: {}", req.mime),
            });
        }

        let bytes = match &req.source {
            AttachmentSource::Bytes(b) => {
                reject_if_oversized(b.len() as u64)?;
                b.clone()
            }
            AttachmentSource::Path(path) => {
                let meta = std::fs::metadata(path).map_err(|e| CoreError::AttachmentRejected {
                    reason: format!("cannot read {path}: {e}"),
                })?;
                reject_if_oversized(meta.len())?;
                std::fs::read(path).map_err(|e| CoreError::AttachmentRejected {
                    reason: format!("cannot read {path}: {e}"),
                })?
            }
        };
        if bytes.is_empty() {
            return Err(CoreError::AttachmentRejected {
                reason: "file is empty".to_string(),
            });
        }

        let sha256 = sha256_hex(&bytes);
        let blob: PathBuf = self.data_dir.join("attachments").join(&sha256);
        if !blob.exists() {
            if let Some(parent) = blob.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&blob, &bytes)?;
        }

        let (width, height) = image_dimensions(&bytes).unzip();
        let attachment = Attachment {
            id: new_id("att"),
            session_id: req.session_id,
            sha256,
            filename: req.filename,
            mime: req.mime,
            bytes: bytes.len() as u64,
            width,
            height,
            path: blob.to_string_lossy().into_owned(),
            thumb_path: None,
            created_at: now_ms(),
        };
        self.with_conn(|conn| insert_attachment(conn, &attachment))?;
        Ok(attachment)
    }

    pub fn get_attachment(&self, attachment_id: &str) -> Result<Attachment> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT * FROM attachments WHERE id = ?1",
                params![attachment_id],
                |row| {
                    Ok(Attachment {
                        id: row.get("id")?,
                        session_id: row.get("session_id")?,
                        sha256: row.get("sha256")?,
                        filename: row.get("filename")?,
                        mime: row.get("mime")?,
                        bytes: row.get::<_, i64>("bytes")?.max(0) as u64,
                        width: row.get::<_, Option<i64>>("width")?.map(|v| v as u32),
                        height: row.get::<_, Option<i64>>("height")?.map(|v| v as u32),
                        path: row.get("path")?,
                        thumb_path: row.get("thumb_path")?,
                        created_at: row.get("created_at")?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| CoreError::AttachmentNotFound(attachment_id.to_string()))
        })
    }

    pub fn list_attachments(&self, session_id: &str) -> Result<Vec<Attachment>> {
        self.with_conn(|conn| {
            let ids: Vec<String> = {
                let mut stmt = conn.prepare(
                    "SELECT id FROM attachments WHERE session_id = ?1 ORDER BY created_at ASC",
                )?;
                let rows = stmt
                    .query_map(params![session_id], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            Ok(ids)
        })?
        .into_iter()
        .map(|id| self.get_attachment(&id))
        .collect()
    }

    /// Swift rasterizes thumbnails (F3.3) and records the path here, keyed by content hash
    /// so the cache survives the record being removed and re-added.
    pub fn set_thumb_path(&self, attachment_id: &str, thumb_path: &str) -> Result<()> {
        self.with_conn(|conn| {
            let n = conn.execute(
                "UPDATE attachments SET thumb_path = ?2 WHERE id = ?1",
                params![attachment_id, thumb_path],
            )?;
            if n == 0 {
                return Err(CoreError::AttachmentNotFound(attachment_id.to_string()));
            }
            Ok(())
        })
    }

    /// Removes the record. The blob stays if another record still references the hash —
    /// dedupe means the file is not ours alone to delete.
    pub fn remove_attachment(&self, attachment_id: &str) -> Result<()> {
        let attachment = self.get_attachment(attachment_id)?;
        let remaining: i64 = self.with_conn(|conn| {
            conn.execute(
                "DELETE FROM attachments WHERE id = ?1",
                params![attachment_id],
            )?;
            let n = conn.query_row(
                "SELECT COUNT(*) FROM attachments WHERE sha256 = ?1",
                params![attachment.sha256],
                |r| r.get(0),
            )?;
            Ok(n)
        })?;
        if remaining == 0 {
            let _ = std::fs::remove_file(&attachment.path);
        }
        Ok(())
    }
}

pub(in crate::app) fn insert_attachment(conn: &Connection, a: &Attachment) -> Result<()> {
    conn.execute(
        "INSERT INTO attachments (id, session_id, sha256, filename, mime, bytes, width, height,
                                  path, thumb_path, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            a.id,
            a.session_id,
            a.sha256,
            a.filename,
            a.mime,
            a.bytes as i64,
            a.width.map(|v| v as i64),
            a.height.map(|v| v as i64),
            a.path,
            a.thumb_path,
            a.created_at,
        ],
    )?;
    Ok(())
}

fn reject_if_oversized(bytes: u64) -> Result<()> {
    if bytes > MAX_ATTACHMENT_BYTES {
        return Err(CoreError::AttachmentRejected {
            reason: format!(
                "{:.1} MB exceeds the {} MB limit",
                bytes as f64 / (1024.0 * 1024.0),
                MAX_ATTACHMENT_BYTES / (1024 * 1024)
            ),
        });
    }
    Ok(())
}

fn mime_allowed(mime: &str) -> bool {
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    mime.starts_with("text/") || ALLOWED_MIMES.contains(&mime)
}

pub(in crate::app) fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Enough header parsing to fill the thumbnail chip's aspect ratio without pulling in an
/// image decoder — the actual raster is Swift's job.
pub(in crate::app) fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some((w, h));
    }
    if bytes.starts_with(b"GIF8") && bytes.len() >= 10 {
        let w = u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32;
        let h = u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32;
        return Some((w, h));
    }
    if bytes.starts_with(b"\xff\xd8") {
        return jpeg_dimensions(bytes);
    }
    None
}

/// Walk the JPEG marker chain to the first start-of-frame segment.
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 3 < bytes.len() {
        if bytes[i] != 0xff {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        // Standalone markers carry no length field.
        if (0xd0..=0xd9).contains(&marker) || marker == 0x01 || marker == 0xff {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes(bytes.get(i + 2..i + 4)?.try_into().ok()?) as usize;
        // SOF0..SOF15, excluding the DHT/JPG/DAC markers interleaved in that range.
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let h = u16::from_be_bytes(bytes.get(i + 5..i + 7)?.try_into().ok()?) as u32;
            let w = u16::from_be_bytes(bytes.get(i + 7..i + 9)?.try_into().ok()?) as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_allowlist_covers_text_subtypes_and_rejects_binaries() {
        assert!(mime_allowed("text/markdown"));
        assert!(mime_allowed("image/png"));
        assert!(mime_allowed("text/plain; charset=utf-8"));
        assert!(!mime_allowed("application/x-mach-binary"));
        assert!(!mime_allowed("video/mp4"));
    }

    #[test]
    fn reads_png_and_gif_headers() {
        let png = crate::app::seed::png::encode_gradient(64, 32, 3);
        assert_eq!(image_dimensions(&png), Some((64, 32)));

        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&100u16.to_le_bytes());
        gif.extend_from_slice(&50u16.to_le_bytes());
        assert_eq!(image_dimensions(&gif), Some((100, 50)));

        assert_eq!(image_dimensions(b"not an image at all"), None);
    }
}
