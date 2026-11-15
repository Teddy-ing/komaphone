//! Experimental single-user Komaphone scene contracts. Party documents are separate.
//! Positions use exact fixed-point ticks; floats are bounded region-local or screen values.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde::{Deserialize, Serialize};

pub mod coordinates;
pub mod render;
pub mod round;
pub mod shared;
pub mod world_protocol;

pub const MAX_DEPTH: u8 = 12;
pub const CHILD_SCALE: i128 = 100;
pub const REGION_LOCAL_SIZE: i128 = 1024;
pub const LOCAL_SUBPIXELS: i128 = 1024;
pub const ROOT_TILE_LIMIT: i128 = 1_000_000;
pub const MAX_POINTS: usize = 8192;
pub const MAX_STROKE_LOCAL: f64 = 8192.0;
pub const MAX_SPATIAL_CELLS: usize = 512;
pub const MAX_VISIBLE_STROKES: usize = 4096;
pub const MAX_VIEW_POINTS: usize = 131_072;
pub const MAX_SUMMARY_POINTS: usize = 24;
pub const MAX_CANVASES: usize = 1024;
pub const MAX_BOOKMARKS: usize = 256;
pub const MAX_NAME_BYTES: usize = 96;
pub const MAX_STROKES: u64 = 1_000_000;
pub const MAX_VIEW_SIDE: u32 = 8192;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("Invalid canvas value: {0}")]
    Invalid(&'static str),
    #[error("Canvas limit reached: {0}. Existing saved artwork is retained.")]
    Limit(&'static str),
    #[error("The target belongs to a different canvas")]
    Canvas,
}
pub type Result<T> = std::result::Result<T, Error>;

macro_rules! identity {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);
        impl $name {
            pub fn new(value: &str) -> Result<Self> {
                if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) || value.bytes().all(|b| b == b'0') {
                    return Err(Error::Invalid(stringify!($name)));
                }
                let mut bytes = [0; 32];
                bytes.copy_from_slice(value.as_bytes());
                Ok(Self(bytes))
            }
            pub fn as_str(&self) -> &str {
                std::str::from_utf8(&self.0).unwrap_or_default()
            }
        }
        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
                struct Visitor;
                impl serde::de::Visitor<'_> for Visitor {
                    type Value = $name;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        write!(f, "a nonzero 32-character lowercase hex identity")
                    }
                    fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
                        $name::new(value).map_err(E::custom)
                    }
                }
                deserializer.deserialize_str(Visitor)
            }
        }
    };
}
identity!(CanvasId);
identity!(OwnerId);
identity!(StrokeId);

/// Decimal strings preserve all 128 bits across JSON control clients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Wide(pub i128);
impl Serialize for Wide {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for Wide {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Wide;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "a bounded canonical signed decimal string")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
                if value.len() > 40 {
                    return Err(E::custom("coordinate too long"));
                }
                let n: i128 = value.parse().map_err(E::custom)?;
                if n.to_string() != value {
                    return Err(E::custom("noncanonical coordinate"));
                }
                Ok(Wide(n))
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: Wide,
    pub y: Wide,
}

/// Stable region identity within a canvas; parent and transform are derived exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionKey {
    pub depth: u8,
    pub x: Wide,
    pub y: Wide,
}

/// Exact region-local tick offsets avoid losing small detail while navigating outward.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub canvas: CanvasId,
    pub region: RegionKey,
    pub offset: [Wide; 2],
    /// Screen pixels per region-local unit; normalized by camera operations.
    pub zoom: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Viewport {
    pub camera: Camera,
    pub width: u32,
    pub height: u32,
}

/// Canonical non-premultiplied, floating-point sRGB color. This explicit working space
/// is independent of finite document bit depths/profiles; imports must use CMS.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Srgba {
    pub rgba: [f32; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub id: StrokeId,
    pub canvas: CanvasId,
    pub owner: OwnerId,
    /// Assigned by the store; zero means an uncommitted stroke.
    pub revision: u64,
    pub anchor: RegionKey,
    pub points: Vec<[f64; 2]>,
    /// Region-local diameter frozen when the stroke begins.
    pub width: f64,
    pub color: Srgba,
    pub erase: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectedStroke {
    pub id: StrokeId,
    pub revision: u64,
    pub paths: Vec<Vec<[f32; 2]>>,
    pub fills: Vec<Vec<[f32; 2]>>,
    pub width: f32,
    pub color: Srgba,
    pub erase: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bookmark {
    pub name: String,
    pub camera: Camera,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanvasInfo {
    pub id: CanvasId,
    pub owner: OwnerId,
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
    pub camera: Camera,
    pub bookmarks: Vec<Bookmark>,
    pub stroke_count: u64,
    pub undo_count: u64,
    pub redo_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewStats {
    pub queried_cells: usize,
    pub candidate_rows: usize,
    pub full_strokes: usize,
    pub summary_strokes: usize,
    pub loaded_bytes: usize,
    pub points: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneView {
    pub viewport: Viewport,
    pub revision: u64,
    pub strokes: Vec<Stroke>,
    pub stats: ViewStats,
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
        return Err(Error::Invalid("name (1–96 UTF-8 bytes)"));
    }
    Ok(())
}

impl Stroke {
    pub fn validate(&self) -> Result<()> {
        coordinates::region_span(self.anchor.depth)?;
        coordinates::point(&self.anchor, [0.0, 0.0])?;
        if self.points.is_empty() || self.points.len() > MAX_POINTS {
            return Err(Error::Limit("stroke samples"));
        }
        if self.points.iter().flatten().any(|n| !n.is_finite() || n.abs() > MAX_STROKE_LOCAL) {
            return Err(Error::Invalid("stroke coordinates"));
        }
        if !self.width.is_finite() || self.width <= 0.0 || self.width > 2048.0 {
            return Err(Error::Invalid("stroke width"));
        }
        if self.color.rgba.iter().any(|n| !n.is_finite() || !(0.0..=1.0).contains(n)) {
            return Err(Error::Invalid("sRGB color"));
        }
        if self.color.rgba[3] != 1.0 {
            return Err(Error::Invalid("canvas brush opacity (this prototype supports opaque strokes)"));
        }
        if self.revision > i64::MAX as u64 {
            return Err(Error::Limit("revision"));
        }
        Ok(())
    }
}
