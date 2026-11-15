//! Immutable raster/animation remix references, with independent placement and
//! authorship. Descriptors are not permission: the service must authorize the
//! source and destination and verify each asset before accepting PlaceMedia.
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{DeviceId, Error, MediaId, Result, SourceId};
use crate::{CanvasId, OwnerId, RegionKey, coordinates};

pub const MAX_MEDIA_FRAMES: usize = 240;
pub const MAX_MEDIA_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_FRAME_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_MEDIA_DURATION_MS: u64 = 600_000;
pub const MAX_MEDIA_AUTHORS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AssetHash([u8; 64]);
impl AssetHash {
    pub fn new(value: &str) -> Result<Self> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) || value.bytes().all(|b| b == b'0') {
            return Err(Error::Invalid("media SHA256"));
        }
        let mut bytes = [0; 64];
        bytes.copy_from_slice(value.as_bytes());
        Ok(Self(bytes))
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}
impl Serialize for AssetHash {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}
impl<'de> Deserialize<'de> for AssetHash {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = AssetHash;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "a nonzero lowercase SHA256 hash")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
                AssetHash::new(value).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaFrame {
    /// Canonical lossless sRGB PNG. Encoded bytes, decoded dimensions, and hash
    /// must all be checked by the storage adapter before admission/rendering.
    pub hash: AssetHash,
    pub byte_len: u64,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
}
impl MediaFrame {
    pub fn validate(&self) -> Result<()> {
        if self.byte_len == 0
            || self.byte_len > MAX_FRAME_BYTES
            || self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || u64::from(self.width) * u64::from(self.height) > 16_777_216
            || !(10..=60_000).contains(&self.duration_ms)
        {
            return Err(Error::Limit("media frame size or duration"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaSource {
    Gallery {
        entry: SourceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        room: Option<SourceId>,
        game: Option<SourceId>,
        chain: u8,
        frame: Option<u16>,
    },
    Canvas {
        canvas: CanvasId,
        media: MediaId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credit {
    pub profile: OwnerId,
    pub label: String,
}
impl Credit {
    pub fn validate(&self) -> Result<()> {
        if self.label.len() > crate::MAX_NAME_BYTES || self.label.trim().is_empty() || self.label.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
            return Err(Error::Invalid("media author label"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub source: MediaSource,
    pub source_revision: u64,
    pub artifact_hash: AssetHash,
    pub authors: Vec<Credit>,
}
impl Provenance {
    pub fn validate(&self) -> Result<()> {
        super::valid_revision(self.source_revision)?;
        if self.authors.is_empty() || self.authors.len() > MAX_MEDIA_AUTHORS {
            return Err(Error::Limit("media authors"));
        }
        let mut authors = BTreeSet::new();
        for credit in &self.authors {
            // Frozen party participants historically permit tab/newline labels.
            // Preserve that exact credit; canvas/tour names remain single-line.
            credit.validate()?;
            if !authors.insert(credit.profile) {
                return Err(Error::Invalid("duplicate media author"));
            }
        }
        if let MediaSource::Gallery { chain, frame, .. } = &self.source
            && (*chain >= 3 || frame.is_some_and(|n| usize::from(n) >= MAX_MEDIA_FRAMES))
        {
            return Err(Error::Invalid("gallery source chain or still frame"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaPlacement {
    pub id: MediaId,
    pub canvas: CanvasId,
    pub owner: OwnerId,
    pub anchor: RegionKey,
    /// Upper-left in bounded region-local coordinates, independent of screen zoom.
    pub offset: [f64; 2],
    pub width: f64,
    pub height: f64,
    pub frames: Vec<MediaFrame>,
    pub looping: bool,
    pub provenance: Provenance,
}
impl MediaPlacement {
    pub fn validate(&self) -> Result<()> {
        coordinates::point(&self.anchor, self.offset)?;
        if !self.width.is_finite() || !self.height.is_finite() || self.width <= 0.0 || self.height <= 0.0 || self.width > 8192.0 || self.height > 8192.0 {
            return Err(Error::Invalid("media placement size"));
        }
        coordinates::point(&self.anchor, [self.offset[0] + self.width, self.offset[1] + self.height])?;
        if self.frames.is_empty() || self.frames.len() > MAX_MEDIA_FRAMES {
            return Err(Error::Limit("media frames"));
        }
        self.provenance.validate()?;
        if let MediaSource::Gallery { frame: Some(_), .. } = &self.provenance.source
            && self.frames.len() != 1
        {
            return Err(Error::Invalid("explicit still extraction must contain one frame"));
        }
        let mut bytes = 0_u64;
        let mut duration = 0_u64;
        let mut dimensions = None;
        for frame in &self.frames {
            frame.validate()?;
            if dimensions.is_some_and(|size| size != (frame.width, frame.height)) {
                return Err(Error::Invalid("animation frame dimensions differ"));
            }
            dimensions = Some((frame.width, frame.height));
            bytes = bytes.checked_add(frame.byte_len).ok_or(Error::Limit("media bytes"))?;
            duration = duration.checked_add(u64::from(frame.duration_ms)).ok_or(Error::Limit("media duration"))?;
        }
        if bytes > MAX_MEDIA_BYTES || duration > MAX_MEDIA_DURATION_MS {
            return Err(Error::Limit("media bytes or duration"));
        }
        Ok(())
    }
    pub fn duration_ms(&self) -> Result<u64> {
        self.validate()?;
        Ok(self.frames.iter().map(|frame| u64::from(frame.duration_ms)).sum())
    }
    /// Pure frame selection. A frozen export supplies the same animation origin
    /// to every rendered frame; wall-clock download/acceptance times are irrelevant.
    pub fn frame_at(&self, elapsed_ms: u64) -> Result<(usize, &MediaFrame)> {
        let duration = self.duration_ms()?;
        let mut elapsed = if self.looping { elapsed_ms % duration } else { elapsed_ms.min(duration.saturating_sub(1)) };
        for (index, frame) in self.frames.iter().enumerate() {
            if elapsed < u64::from(frame.duration_ms) {
                return Ok((index, frame));
            }
            elapsed -= u64::from(frame.duration_ms);
        }
        Err(Error::Invalid("media timeline"))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedMedia {
    pub media: MediaPlacement,
    pub device: DeviceId,
    pub visible: bool,
    pub created_revision: u64,
    pub changed_revision: u64,
}
