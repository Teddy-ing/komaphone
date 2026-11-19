//! Single-source-of-truth multi-scale brush converter and viewport-clipped rasterizer
//! for shared canvases (`World`).
//!
//! Both CPU exports/thumbnails (`world_media::WorldRenderer`) and the native GPU
//! compositor (`komaphone_world_gpu::WorldGpu`) rasterize `StrokeBrush` strokes through
//! this module, guaranteeing pixel-identical opacity, flow, overlap, tip shape, dynamics,
//! texture, dual brush, wet edges, smoothing, and deep-zoom behavior across live previews,
//! peer previews, accepted art, reopen, catch-up playback, tours, PNG exports, and thumbnails.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use photocraft_codecs::{ChannelLayout, DecodeOptions, EncodeOptions, Format, Image, Limits as CodecLimits, SampleType};
use photocraft_color::BlendMode;
use photocraft_paint::{
    BrushSettings, ColorDynamics, Control, Dab, DualBrush, Dynamic, GrayTile, MaskMode, Pattern, PatternStyle, Pose, ResponseCurve, Scattering, ShapeDynamics,
    Smoothing, StrokePoint, TipShape, Transfer,
    dynamics::DabGenerator,
    grid_center,
    procedural::{bristle_tip, chalk_tip, charcoal_tip, grass_tip, leaf_tip, pattern as procedural_pattern, rake_tip, spatter_tip, sponge_tip, star_tip},
    render::{mask_combine, texture_combine},
    rng::hash2,
    tile::{Mips, PatternImage, b64_decode, b64_encode},
};
use photocraft_scene::{
    Stroke, Viewport,
    brush::{
        BrushBlendMode, BrushColorDynamics, BrushControl, BrushDual, BrushDynamic, BrushMaskMode, BrushPattern, BrushPatternStyle, BrushPose, BrushScattering,
        BrushShapeDynamics, BrushSmoothing, BrushTexture, BrushTip, BrushTransfer, MAX_BRUSH_SIZE, MAX_PREVIEW_B64_BYTES, MAX_SAMPLED_B64_BYTES,
        MAX_SAMPLED_SIDE, MIN_BRUSH_SIZE, ProceduralTipStyle, STROKE_BRUSH_SCHEMA, StrokeBrush, StrokeSample,
    },
    coordinates,
};

use crate::{EngineError, Result};

/// Edge of one viewport brush tile in physical pixels. `64 * 4 = 256` bytes per row,
/// matching `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT` with zero padding overhead.
pub const WORLD_BRUSH_TILE: u32 = 64;

/// Maximum combined primary + dual dabs rasterized for a single shared-canvas stroke.
pub const MAX_STROKE_DABS: usize = 32_768;

/// Default per-render work budget (physical pixel samples).
pub const MAX_BRUSH_WORK_SAMPLES: u64 = 268_435_456;

const DITHER_SALT: u64 = 0x0000_6469_7468_6572;
const NOISE_SALT: u64 = 0x006E_6F69_7365;
const SMALL_TIP_RADIUS: f32 = 3.0;
const SUBPIXEL: [f64; 4] = [-0.375, -0.125, 0.125, 0.375];
const TIP_EDGE_PX: f32 = 1.2;
const TIP_SHAPE: [(f32, f32); 11] =
    [(0.00, 0.664), (0.14, 0.58), (0.28, 0.50), (0.40, 0.44), (0.52, 0.36), (0.63, 0.29), (0.70, 0.24), (0.78, 0.19), (0.84, 0.15), (0.89, 0.11), (1.00, 0.0)];

fn err(msg: impl Into<String>) -> EngineError {
    EngineError::Other(msg.into())
}

// ---------------------------------------------------------------------------
// 106-bit Double arithmetic (matches photocraft_scene::render::Double)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Double {
    hi: f64,
    lo: f64,
}

impl Double {
    fn scalar(hi: f64) -> Self {
        Self { hi, lo: 0.0 }
    }
    fn integer(n: i128) -> Self {
        let hi = n as f64;
        Self { hi, lo: (n - hi as i128) as f64 }
    }
    fn normalized(hi: f64, lo: f64) -> Self {
        let sum = hi + lo;
        Self { hi: sum, lo: lo - (sum - hi) }
    }
    fn add(self, b: Self) -> Self {
        let sum = self.hi + b.hi;
        let v = sum - self.hi;
        let error = (self.hi - (sum - v)) + (b.hi - v) + self.lo + b.lo;
        Self::normalized(sum, error)
    }
    fn neg(self) -> Self {
        Self { hi: -self.hi, lo: -self.lo }
    }
    fn sub(self, b: Self) -> Self {
        self.add(b.neg())
    }
    fn mul(self, b: Self) -> Self {
        let product = self.hi * b.hi;
        Self::normalized(product, self.hi.mul_add(b.hi, -product) + self.hi * b.lo + self.lo * b.hi + self.lo * b.lo)
    }
    fn div(self, b: Self) -> Self {
        let q = self.hi / b.hi;
        let remainder = self.sub(b.mul(Self::scalar(q)));
        Self::normalized(q, (remainder.hi + remainder.lo) / b.hi)
    }
    fn value(self) -> f64 {
        self.hi + self.lo
    }
}

// ---------------------------------------------------------------------------
// Built-in procedural tip catalog & bidirectional BrushSettings <-> StrokeBrush
// ---------------------------------------------------------------------------

struct ProceduralEntry {
    style: ProceduralTipStyle,
    size: u32,
    seed: u32,
    param: u32,
    tile: GrayTile,
}

fn generate_procedural_tip(style: ProceduralTipStyle, size: u32, seed: u32, param: u32) -> GrayTile {
    let n = size.clamp(8, 256);
    let p = param.min(256);
    match style {
        ProceduralTipStyle::Chalk => chalk_tip(n, seed),
        ProceduralTipStyle::Charcoal => charcoal_tip(n, seed),
        ProceduralTipStyle::Bristle => bristle_tip(n, seed),
        ProceduralTipStyle::Rake => rake_tip(n, seed, p.max(1)),
        ProceduralTipStyle::Sponge => sponge_tip(n, seed),
        ProceduralTipStyle::Spatter => spatter_tip(n, seed, p.max(1)),
        ProceduralTipStyle::Leaf => leaf_tip(n),
        ProceduralTipStyle::Grass => grass_tip(n, seed, p.max(1)),
        ProceduralTipStyle::Star => star_tip(n),
    }
}

fn builtin_procedural_catalog() -> &'static [ProceduralEntry] {
    static CATALOG: OnceLock<Vec<ProceduralEntry>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let specs: &[(ProceduralTipStyle, u32, u32, u32)] = &[
            (ProceduralTipStyle::Chalk, 64, 7, 0),
            (ProceduralTipStyle::Charcoal, 64, 4, 0),
            (ProceduralTipStyle::Chalk, 64, 31, 0),
            (ProceduralTipStyle::Bristle, 64, 5, 0),
            (ProceduralTipStyle::Rake, 64, 3, 7),
            (ProceduralTipStyle::Bristle, 64, 41, 0),
            (ProceduralTipStyle::Sponge, 96, 8, 0),
            (ProceduralTipStyle::Spatter, 64, 11, 14),
            (ProceduralTipStyle::Spatter, 64, 77, 40),
            (ProceduralTipStyle::Leaf, 64, 0, 0),
            (ProceduralTipStyle::Grass, 96, 6, 11),
            (ProceduralTipStyle::Star, 64, 0, 0),
            (ProceduralTipStyle::Sponge, 64, 19, 0),
            (ProceduralTipStyle::Spatter, 48, 23, 20),
        ];
        specs
            .iter()
            .map(|&(style, size, seed, param)| ProceduralEntry { style, size, seed, param, tile: generate_procedural_tip(style, size, seed, param) })
            .collect()
    })
}

fn match_builtin_procedural(tile: &GrayTile) -> Option<(ProceduralTipStyle, u32, u32, u32)> {
    for entry in builtin_procedural_catalog() {
        if tile.width == entry.tile.width && tile.height == entry.tile.height && tile.data == entry.tile.data {
            return Some((entry.style, entry.size, entry.seed, entry.param));
        }
    }
    None
}

fn encode_gray_tile_png(tile: &GrayTile, max_side: u16, max_b64: usize) -> Result<(u16, u16, String)> {
    if !tile.is_valid() {
        return Err(err("Invalid brush bitmap tile"));
    }
    let mut side = u32::from(max_side.min(MAX_SAMPLED_SIDE)).max(8);
    loop {
        let scaled = if tile.width > side || tile.height > side { tile.preview(side) } else { tile.clone() };
        let pixels: Vec<u8> = scaled.data.iter().map(|&v| ((u32::from(v) + 128) / 257) as u8).collect();
        let img = Image::from_u8(scaled.width, scaled.height, ChannelLayout::Gray, pixels).map_err(|e| err(e.to_string()))?;
        let png = photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).map_err(|e| err(e.to_string()))?;
        let b64 = b64_encode(&png);
        if b64.len() <= max_b64 {
            let w = u16::try_from(scaled.width).map_err(|_| err("Brush bitmap width overflow"))?;
            let h = u16::try_from(scaled.height).map_err(|_| err("Brush bitmap height overflow"))?;
            return Ok((w, h, b64));
        }
        if side <= 16 {
            return Err(err("Custom brush bitmap exceeds shared-canvas transfer limit"));
        }
        side = (side / 2).max(16);
    }
}

fn decode_gray_tile_png(width: u16, height: u16, png_b64: &str) -> Result<GrayTile> {
    let bytes = b64_decode(png_b64).ok_or_else(|| err("Invalid brush bitmap base64"))?;
    let opts = DecodeOptions {
        limits: CodecLimits {
            max_width: u32::from(MAX_SAMPLED_SIDE),
            max_height: u32::from(MAX_SAMPLED_SIDE),
            max_pixels: u64::from(MAX_SAMPLED_SIDE) * u64::from(MAX_SAMPLED_SIDE),
            max_alloc: 4 * 1024 * 1024,
        },
        ..Default::default()
    };
    let img = photocraft_codecs::decode_as_with(Format::Png, &bytes, &opts).map_err(|e| err(format!("Invalid brush bitmap PNG: {e}")))?;
    if img.width() != u32::from(width) || img.height() != u32::from(height) || img.layout() != ChannelLayout::Gray || img.sample_type() != SampleType::U8 {
        return Err(err("Brush bitmap PNG dimensions or format do not match declaration"));
    }
    let raw = img.as_u8().ok_or_else(|| err("Brush bitmap is not 8-bit grayscale"))?;
    let data: Vec<u16> = raw.iter().map(|&v| u16::from(v) * 257).collect();
    let tile = GrayTile { width: u32::from(width), height: u32::from(height), data: data.into() };
    if !tile.is_valid() {
        return Err(err("Decoded brush bitmap has invalid dimensions"));
    }
    Ok(tile)
}

pub fn blend_mode_from_paint(mode: BlendMode) -> BrushBlendMode {
    match mode {
        BlendMode::PassThrough | BlendMode::Normal => BrushBlendMode::Normal,
        BlendMode::Dissolve => BrushBlendMode::Dissolve,
        BlendMode::Darken => BrushBlendMode::Darken,
        BlendMode::Multiply => BrushBlendMode::Multiply,
        BlendMode::ColorBurn => BrushBlendMode::ColorBurn,
        BlendMode::LinearBurn => BrushBlendMode::LinearBurn,
        BlendMode::DarkerColor => BrushBlendMode::DarkerColor,
        BlendMode::Lighten => BrushBlendMode::Lighten,
        BlendMode::Screen => BrushBlendMode::Screen,
        BlendMode::ColorDodge => BrushBlendMode::ColorDodge,
        BlendMode::LinearDodge => BrushBlendMode::LinearDodge,
        BlendMode::LighterColor => BrushBlendMode::LighterColor,
        BlendMode::Overlay => BrushBlendMode::Overlay,
        BlendMode::SoftLight => BrushBlendMode::SoftLight,
        BlendMode::HardLight => BrushBlendMode::HardLight,
        BlendMode::VividLight => BrushBlendMode::VividLight,
        BlendMode::LinearLight => BrushBlendMode::LinearLight,
        BlendMode::PinLight => BrushBlendMode::PinLight,
        BlendMode::HardMix => BrushBlendMode::HardMix,
        BlendMode::Difference => BrushBlendMode::Difference,
        BlendMode::Exclusion => BrushBlendMode::Exclusion,
        BlendMode::Subtract => BrushBlendMode::Subtract,
        BlendMode::Divide => BrushBlendMode::Divide,
        BlendMode::Hue => BrushBlendMode::Hue,
        BlendMode::Saturation => BrushBlendMode::Saturation,
        BlendMode::Color => BrushBlendMode::Color,
        BlendMode::Luminosity => BrushBlendMode::Luminosity,
        BlendMode::Reflect => BrushBlendMode::Reflect,
        BlendMode::Glow => BrushBlendMode::Glow,
        BlendMode::Negation => BrushBlendMode::Negation,
        BlendMode::Xor => BrushBlendMode::Xor,
        BlendMode::PaintNetColorBurn => BrushBlendMode::PaintNetColorBurn,
        BlendMode::PaintNetColorDodge => BrushBlendMode::PaintNetColorDodge,
    }
}

pub fn blend_mode_to_paint(mode: BrushBlendMode) -> BlendMode {
    match mode {
        BrushBlendMode::Normal => BlendMode::Normal,
        BrushBlendMode::Dissolve => BlendMode::Dissolve,
        BrushBlendMode::Darken => BlendMode::Darken,
        BrushBlendMode::Multiply => BlendMode::Multiply,
        BrushBlendMode::ColorBurn => BlendMode::ColorBurn,
        BrushBlendMode::LinearBurn => BlendMode::LinearBurn,
        BrushBlendMode::DarkerColor => BlendMode::DarkerColor,
        BrushBlendMode::Lighten => BlendMode::Lighten,
        BrushBlendMode::Screen => BlendMode::Screen,
        BrushBlendMode::ColorDodge => BlendMode::ColorDodge,
        BrushBlendMode::LinearDodge => BlendMode::LinearDodge,
        BrushBlendMode::LighterColor => BlendMode::LighterColor,
        BrushBlendMode::Overlay => BlendMode::Overlay,
        BrushBlendMode::SoftLight => BlendMode::SoftLight,
        BrushBlendMode::HardLight => BlendMode::HardLight,
        BrushBlendMode::VividLight => BlendMode::VividLight,
        BrushBlendMode::LinearLight => BlendMode::LinearLight,
        BrushBlendMode::PinLight => BlendMode::PinLight,
        BrushBlendMode::HardMix => BlendMode::HardMix,
        BrushBlendMode::Difference => BlendMode::Difference,
        BrushBlendMode::Exclusion => BlendMode::Exclusion,
        BrushBlendMode::Subtract => BlendMode::Subtract,
        BrushBlendMode::Divide => BlendMode::Divide,
        BrushBlendMode::Hue => BlendMode::Hue,
        BrushBlendMode::Saturation => BlendMode::Saturation,
        BrushBlendMode::Color => BlendMode::Color,
        BrushBlendMode::Luminosity => BlendMode::Luminosity,
        BrushBlendMode::Reflect => BlendMode::Reflect,
        BrushBlendMode::Glow => BlendMode::Glow,
        BrushBlendMode::Negation => BlendMode::Negation,
        BrushBlendMode::Xor => BlendMode::Xor,
        BrushBlendMode::PaintNetColorBurn => BlendMode::PaintNetColorBurn,
        BrushBlendMode::PaintNetColorDodge => BlendMode::PaintNetColorDodge,
    }
}

fn control_from_paint(c: Control) -> BrushControl {
    match c {
        Control::Off => BrushControl::Off,
        Control::Fade => BrushControl::Fade,
        Control::PenPressure => BrushControl::PenPressure,
        Control::PenTilt => BrushControl::PenTilt,
        Control::StylusWheel => BrushControl::StylusWheel,
        Control::Rotation => BrushControl::Rotation,
        Control::InitialDirection => BrushControl::InitialDirection,
        Control::Direction => BrushControl::Direction,
    }
}

fn control_to_paint(c: BrushControl) -> Control {
    match c {
        BrushControl::Off => Control::Off,
        BrushControl::Fade => Control::Fade,
        BrushControl::PenPressure => Control::PenPressure,
        BrushControl::PenTilt => Control::PenTilt,
        BrushControl::StylusWheel => Control::StylusWheel,
        BrushControl::Rotation => Control::Rotation,
        BrushControl::InitialDirection => Control::InitialDirection,
        BrushControl::Direction => Control::Direction,
    }
}

fn mask_mode_from_paint(m: MaskMode) -> BrushMaskMode {
    match m {
        MaskMode::Multiply => BrushMaskMode::Multiply,
        MaskMode::Subtract => BrushMaskMode::Subtract,
        MaskMode::Darken => BrushMaskMode::Darken,
        MaskMode::Overlay => BrushMaskMode::Overlay,
        MaskMode::ColorDodge => BrushMaskMode::ColorDodge,
        MaskMode::ColorBurn => BrushMaskMode::ColorBurn,
        MaskMode::LinearBurn => BrushMaskMode::LinearBurn,
        MaskMode::HardMix => BrushMaskMode::HardMix,
        MaskMode::LinearHeight => BrushMaskMode::LinearHeight,
        MaskMode::Height => BrushMaskMode::Height,
    }
}

fn mask_mode_to_paint(m: BrushMaskMode) -> MaskMode {
    match m {
        BrushMaskMode::Multiply => MaskMode::Multiply,
        BrushMaskMode::Subtract => MaskMode::Subtract,
        BrushMaskMode::Darken => MaskMode::Darken,
        BrushMaskMode::Overlay => MaskMode::Overlay,
        BrushMaskMode::ColorDodge => MaskMode::ColorDodge,
        BrushMaskMode::ColorBurn => MaskMode::ColorBurn,
        BrushMaskMode::LinearBurn => MaskMode::LinearBurn,
        BrushMaskMode::HardMix => MaskMode::HardMix,
        BrushMaskMode::LinearHeight => MaskMode::LinearHeight,
        BrushMaskMode::Height => MaskMode::Height,
    }
}

fn pattern_style_from_paint(s: PatternStyle) -> BrushPatternStyle {
    match s {
        PatternStyle::Noise => BrushPatternStyle::Noise,
        PatternStyle::Canvas => BrushPatternStyle::Canvas,
        PatternStyle::Paper => BrushPatternStyle::Paper,
        PatternStyle::Dots => BrushPatternStyle::Dots,
    }
}

fn pattern_style_to_paint(s: BrushPatternStyle) -> PatternStyle {
    match s {
        BrushPatternStyle::Noise => PatternStyle::Noise,
        BrushPatternStyle::Canvas => PatternStyle::Canvas,
        BrushPatternStyle::Paper => PatternStyle::Paper,
        BrushPatternStyle::Dots => PatternStyle::Dots,
    }
}

fn dynamic_from_paint(d: &Dynamic, max_jitter: f32) -> BrushDynamic {
    let jitter = if d.jitter.is_finite() { d.jitter.clamp(0.0, max_jitter) } else { 0.0 };
    let minimum = if d.minimum.is_finite() { d.minimum.clamp(0.0, 1.0) } else { 0.0 };
    let fade_steps = d.fade_steps.clamp(1, 100_000);
    let curve = (!d.curve.is_linear()).then(|| d.curve.points());
    BrushDynamic { jitter, control: control_from_paint(d.control), fade_steps, minimum, curve }
}

fn dynamic_to_paint(d: &BrushDynamic) -> Dynamic {
    Dynamic {
        jitter: d.jitter,
        control: control_to_paint(d.control),
        fade_steps: d.fade_steps,
        minimum: d.minimum,
        curve: d.curve.as_deref().map_or(ResponseCurve::LINEAR, ResponseCurve::new),
    }
}

fn tip_from_paint(tip: &TipShape, preview: bool) -> Result<BrushTip> {
    let Some(bitmap) = tip.bitmap() else {
        return Ok(BrushTip::Round);
    };
    if let Some((style, size, seed, param)) = match_builtin_procedural(bitmap) {
        return Ok(BrushTip::Procedural { style, size, seed, param });
    }
    let (max_side, max_b64) = if preview { (48, MAX_PREVIEW_B64_BYTES) } else { (MAX_SAMPLED_SIDE, MAX_SAMPLED_B64_BYTES) };
    let (width, height, png_b64) = encode_gray_tile_png(bitmap, max_side, max_b64)?;
    Ok(BrushTip::Sampled { width, height, png_b64 })
}

fn tip_to_paint(tip: &BrushTip) -> Result<TipShape> {
    match tip {
        BrushTip::Round => Ok(TipShape::Round),
        BrushTip::Procedural { style, size, seed, param } => Ok(TipShape::Sampled(generate_procedural_tip(*style, *size, *seed, *param))),
        BrushTip::Sampled { width, height, png_b64 } => Ok(TipShape::Sampled(decode_gray_tile_png(*width, *height, png_b64)?)),
    }
}

fn pattern_from_paint(pattern: &Pattern, preview: bool) -> Result<BrushPattern> {
    match pattern {
        Pattern::Procedural { style, size, seed } => {
            Ok(BrushPattern::Procedural { style: pattern_style_from_paint(*style), size: (*size).clamp(8, 256), seed: *seed })
        }
        _ => {
            let Some(bitmap) = pattern.bitmap() else {
                return Ok(BrushPattern::default());
            };
            let (max_side, max_b64) = if preview { (48, MAX_PREVIEW_B64_BYTES) } else { (MAX_SAMPLED_SIDE, MAX_SAMPLED_B64_BYTES) };
            let (width, height, png_b64) = encode_gray_tile_png(bitmap, max_side, max_b64)?;
            Ok(BrushPattern::Tile { width, height, png_b64 })
        }
    }
}

fn pattern_to_paint(pattern: &BrushPattern) -> Result<Pattern> {
    match pattern {
        BrushPattern::Procedural { style, size, seed } => {
            Ok(Pattern::Procedural { style: pattern_style_to_paint(*style), size: (*size).clamp(8, 256), seed: *seed })
        }
        BrushPattern::Tile { width, height, png_b64 } => Ok(Pattern::Tile(decode_gray_tile_png(*width, *height, png_b64)?)),
    }
}

/// Convert live editor `BrushSettings` into a validated, compact `StrokeBrush`
/// suitable for durable storage or transient peer previews (`preview = true`).
pub fn to_stroke_brush(brush: &BrushSettings, smoothing_zoom: f32, preview: bool) -> Result<StrokeBrush> {
    let b = brush.bounded_for_render();
    let authored_size = if b.size.is_finite() { b.size.clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE) } else { 20.0 };
    let hardness = if b.hardness.is_finite() { b.hardness.clamp(0.0, 1.0) } else { 1.0 };
    let spacing = if b.spacing.is_finite() { b.spacing.clamp(0.01, 10.0) } else { 0.1 };
    let opacity = if b.opacity.is_finite() { b.opacity.clamp(0.0, 1.0) } else { 1.0 };
    let flow = if b.flow.is_finite() { b.flow.clamp(0.0, 1.0) } else { 1.0 };
    let angle = if b.angle.is_finite() { b.angle.rem_euclid(360.0) } else { 0.0 };
    let angle = if angle > 180.0 { angle - 360.0 } else { angle };
    let roundness = if b.roundness.is_finite() { b.roundness.clamp(0.01, 1.0) } else { 1.0 };
    let background = b.background.map(|c| if c.is_finite() { c.clamp(0.0, 1.0) } else { 1.0 });

    let shape_dynamics = b.shape_dynamics.enabled.then(|| BrushShapeDynamics {
        size: dynamic_from_paint(&b.shape_dynamics.size, 1.0),
        tilt_scale: if b.shape_dynamics.tilt_scale.is_finite() { b.shape_dynamics.tilt_scale.clamp(0.0, 2.0) } else { 0.0 },
        angle: dynamic_from_paint(&b.shape_dynamics.angle, 1.0),
        roundness: dynamic_from_paint(&b.shape_dynamics.roundness, 1.0),
        flip_x_jitter: b.shape_dynamics.flip_x_jitter,
        flip_y_jitter: b.shape_dynamics.flip_y_jitter,
        brush_projection: b.shape_dynamics.brush_projection,
    });

    let scattering = b.scattering.enabled.then(|| BrushScattering {
        scatter: dynamic_from_paint(&b.scattering.scatter, photocraft_scene::brush::MAX_SCATTER),
        both_axes: b.scattering.both_axes,
        count: b.scattering.count.clamp(1, 16),
        count_jitter: dynamic_from_paint(&b.scattering.count_jitter, 1.0),
    });

    let texture = if b.texture.enabled {
        Some(BrushTexture {
            pattern: pattern_from_paint(&b.texture.pattern, preview)?,
            invert: b.texture.invert,
            scale: if b.texture.scale.is_finite() { b.texture.scale.clamp(0.01, 20.0) } else { 1.0 },
            brightness: if b.texture.brightness.is_finite() { b.texture.brightness.clamp(-1.0, 1.0) } else { 0.0 },
            contrast: if b.texture.contrast.is_finite() { b.texture.contrast.clamp(-1.0, 1.0) } else { 0.0 },
            each_tip: b.texture.each_tip,
            mode: mask_mode_from_paint(b.texture.mode),
            depth: if b.texture.depth.is_finite() { b.texture.depth.clamp(0.0, 1.0) } else { 1.0 },
            depth_jitter: dynamic_from_paint(&b.texture.depth_jitter, 1.0),
        })
    } else {
        None
    };

    let dual_brush = if b.dual_brush.enabled {
        let dual_angle = if b.dual_brush.angle.is_finite() { b.dual_brush.angle.rem_euclid(360.0) } else { 0.0 };
        let dual_angle = if dual_angle > 180.0 { dual_angle - 360.0 } else { dual_angle };
        Some(Box::new(BrushDual {
            mode: mask_mode_from_paint(b.dual_brush.mode),
            tip: tip_from_paint(&b.dual_brush.tip, preview)?,
            size: if b.dual_brush.size.is_finite() { b.dual_brush.size.clamp(MIN_BRUSH_SIZE, MAX_BRUSH_SIZE) } else { 20.0 },
            hardness: if b.dual_brush.hardness.is_finite() { b.dual_brush.hardness.clamp(0.0, 1.0) } else { 1.0 },
            roundness: if b.dual_brush.roundness.is_finite() { b.dual_brush.roundness.clamp(0.01, 1.0) } else { 1.0 },
            angle: dual_angle,
            spacing: if b.dual_brush.spacing.is_finite() { b.dual_brush.spacing.clamp(0.01, 10.0) } else { 0.25 },
            scatter: if b.dual_brush.scatter.is_finite() { b.dual_brush.scatter.clamp(0.0, photocraft_scene::brush::MAX_SCATTER) } else { 0.0 },
            both_axes: b.dual_brush.both_axes,
            count: b.dual_brush.count.clamp(1, 16),
            flip: b.dual_brush.flip,
        }))
    } else {
        None
    };

    let color_dynamics = b.color_dynamics.enabled.then(|| BrushColorDynamics {
        per_tip: b.color_dynamics.per_tip,
        fg_bg: dynamic_from_paint(&b.color_dynamics.fg_bg, 1.0),
        hue_jitter: if b.color_dynamics.hue_jitter.is_finite() { b.color_dynamics.hue_jitter.clamp(0.0, 1.0) } else { 0.0 },
        saturation_jitter: if b.color_dynamics.saturation_jitter.is_finite() { b.color_dynamics.saturation_jitter.clamp(0.0, 1.0) } else { 0.0 },
        brightness_jitter: if b.color_dynamics.brightness_jitter.is_finite() { b.color_dynamics.brightness_jitter.clamp(0.0, 1.0) } else { 0.0 },
        purity: if b.color_dynamics.purity.is_finite() { b.color_dynamics.purity.clamp(-1.0, 1.0) } else { 0.0 },
    });

    let transfer =
        b.transfer.enabled.then(|| BrushTransfer { opacity: dynamic_from_paint(&b.transfer.opacity, 1.0), flow: dynamic_from_paint(&b.transfer.flow, 1.0) });

    let pose = b.pose.enabled.then(|| BrushPose {
        tilt_x: if b.pose.tilt_x.is_finite() { b.pose.tilt_x.clamp(-90.0, 90.0) } else { 0.0 },
        tilt_y: if b.pose.tilt_y.is_finite() { b.pose.tilt_y.clamp(-90.0, 90.0) } else { 0.0 },
        rotation: if b.pose.rotation.is_finite() { b.pose.rotation.rem_euclid(360.0) } else { 0.0 },
        pressure: if b.pose.pressure.is_finite() { b.pose.pressure.clamp(0.0, 1.0) } else { 1.0 },
        override_tilt_x: b.pose.override_tilt_x,
        override_tilt_y: b.pose.override_tilt_y,
        override_rotation: b.pose.override_rotation,
        override_pressure: b.pose.override_pressure,
    });

    let smoothing = (b.smoothing.enabled && b.smoothing.amount.is_finite() && b.smoothing.amount > 0.0).then(|| BrushSmoothing {
        amount: b.smoothing.amount.clamp(0.0, 1.0),
        pulled_string: b.smoothing.pulled_string,
        catch_up: b.smoothing.catch_up,
        catch_up_on_end: b.smoothing.catch_up_on_end,
        adjust_for_zoom: b.smoothing.adjust_for_zoom,
        zoom: if smoothing_zoom.is_finite() { smoothing_zoom.clamp(0.01, 1000.0) } else { 1.0 },
    });

    let out = StrokeBrush {
        schema: STROKE_BRUSH_SCHEMA,
        authored_size,
        hardness,
        spacing,
        spacing_enabled: b.spacing_enabled,
        opacity,
        flow,
        pressure_size: b.pressure_size,
        pressure_opacity: b.pressure_opacity,
        background: if color_dynamics.is_some() { background } else { [1.0, 1.0, 1.0, 1.0] },
        behind: b.behind && !b.erase,
        mode: if b.erase { BrushBlendMode::Normal } else { blend_mode_from_paint(b.mode) },
        tip: tip_from_paint(&b.tip, preview)?,
        angle,
        roundness,
        flip_x: b.flip_x,
        flip_y: b.flip_y,
        aliased: b.aliased,
        square: b.square,
        shape_dynamics,
        scattering,
        texture,
        dual_brush,
        color_dynamics,
        transfer,
        pose,
        noise: b.noise,
        wet_edges: b.wet_edges,
        build_up: b.build_up,
        build_up_rate: if b.build_up && b.build_up_rate.is_finite() { b.build_up_rate.clamp(0.1, 1000.0) } else { 20.0 },
        smoothing,
        seed: b.seed,
    };
    if preview {
        out.validate_preview().map_err(|e| err(e.to_string()))?;
    } else {
        out.validate().map_err(|e| err(e.to_string()))?;
    }
    Ok(out)
}

/// Validate any embedded custom PNG tip or texture tiles inside a `StrokeBrush`.
pub fn validate_stroke_brush_assets(brush: &StrokeBrush) -> Result<()> {
    brush.validate().map_err(|e| err(e.to_string()))?;
    if let BrushTip::Sampled { width, height, png_b64 } = &brush.tip {
        let _ = decode_gray_tile_png(*width, *height, png_b64)?;
    }
    if let Some(tx) = &brush.texture
        && let BrushPattern::Tile { width, height, png_b64 } = &tx.pattern
    {
        let _ = decode_gray_tile_png(*width, *height, png_b64)?;
    }
    if let Some(dual) = &brush.dual_brush
        && let BrushTip::Sampled { width, height, png_b64 } = &dual.tip
    {
        let _ = decode_gray_tile_png(*width, *height, png_b64)?;
    }
    Ok(())
}

/// Convert a shared-canvas `Stroke` + `StrokeBrush` into `photocraft_paint::BrushSettings`
/// operating in canonical brush-space (`size == brush.authored_size`).
pub fn to_paint_settings(stroke: &Stroke, brush: &StrokeBrush) -> Result<BrushSettings> {
    let mut settings = BrushSettings {
        size: brush.authored_size,
        hardness: brush.hardness,
        spacing: brush.spacing,
        spacing_enabled: brush.spacing_enabled,
        opacity: brush.opacity,
        flow: brush.flow,
        pressure_size: brush.pressure_size,
        pressure_opacity: brush.pressure_opacity,
        // Opacity lives exclusively on `brush.opacity`; `stroke.color.rgba[3]` may carry a
        // summary opacity value for metadata/thumbnails and must never multiply opacity twice.
        color: [stroke.color.rgba[0], stroke.color.rgba[1], stroke.color.rgba[2], 1.0],
        color_cmyk: None,
        background: brush.background,
        erase: stroke.erase,
        behind: brush.behind && !stroke.erase,
        mode: blend_mode_to_paint(brush.mode),
        tip: tip_to_paint(&brush.tip)?,
        angle: brush.angle,
        roundness: brush.roundness,
        flip_x: brush.flip_x,
        flip_y: brush.flip_y,
        aliased: brush.aliased,
        square: brush.square,
        noise: brush.noise,
        wet_edges: brush.wet_edges,
        build_up: brush.build_up,
        build_up_rate: brush.build_up_rate,
        seed: brush.seed,
        ..Default::default()
    };
    if let Some(sd) = &brush.shape_dynamics {
        settings.shape_dynamics = ShapeDynamics {
            enabled: true,
            size: dynamic_to_paint(&sd.size),
            tilt_scale: sd.tilt_scale,
            angle: dynamic_to_paint(&sd.angle),
            roundness: dynamic_to_paint(&sd.roundness),
            flip_x_jitter: sd.flip_x_jitter,
            flip_y_jitter: sd.flip_y_jitter,
            brush_projection: sd.brush_projection,
        };
    }
    if let Some(sc) = &brush.scattering {
        settings.scattering = Scattering {
            enabled: true,
            scatter: dynamic_to_paint(&sc.scatter),
            both_axes: sc.both_axes,
            count: sc.count,
            count_jitter: dynamic_to_paint(&sc.count_jitter),
        };
    }
    if let Some(tx) = &brush.texture {
        settings.texture = photocraft_paint::Texture {
            enabled: true,
            pattern: pattern_to_paint(&tx.pattern)?,
            invert: tx.invert,
            scale: tx.scale,
            brightness: tx.brightness,
            contrast: tx.contrast,
            each_tip: tx.each_tip,
            mode: mask_mode_to_paint(tx.mode),
            depth: tx.depth,
            depth_jitter: dynamic_to_paint(&tx.depth_jitter),
        };
    }
    if let Some(dual) = &brush.dual_brush {
        settings.dual_brush = DualBrush {
            enabled: true,
            mode: mask_mode_to_paint(dual.mode),
            tip: tip_to_paint(&dual.tip)?,
            size: dual.size,
            hardness: dual.hardness,
            roundness: dual.roundness,
            angle: dual.angle,
            spacing: dual.spacing,
            scatter: dual.scatter,
            both_axes: dual.both_axes,
            count: dual.count,
            flip: dual.flip,
        };
    }
    if let Some(cd) = &brush.color_dynamics {
        settings.color_dynamics = ColorDynamics {
            enabled: true,
            per_tip: cd.per_tip,
            fg_bg: dynamic_to_paint(&cd.fg_bg),
            hue_jitter: cd.hue_jitter,
            saturation_jitter: cd.saturation_jitter,
            brightness_jitter: cd.brightness_jitter,
            purity: cd.purity,
        };
    }
    if let Some(tr) = &brush.transfer {
        settings.transfer = Transfer {
            enabled: true,
            opacity: dynamic_to_paint(&tr.opacity),
            flow: dynamic_to_paint(&tr.flow),
            wetness: Dynamic::default(),
            mix: Dynamic::default(),
        };
    }
    if let Some(pose) = &brush.pose {
        settings.pose = Pose {
            enabled: true,
            tilt_x: pose.tilt_x,
            tilt_y: pose.tilt_y,
            rotation: pose.rotation,
            pressure: pose.pressure,
            override_tilt_x: pose.override_tilt_x,
            override_tilt_y: pose.override_tilt_y,
            override_rotation: pose.override_rotation,
            override_pressure: pose.override_pressure,
        };
    }
    if let Some(sm) = &brush.smoothing {
        settings.smoothing = Smoothing {
            enabled: true,
            amount: sm.amount,
            pulled_string: sm.pulled_string,
            catch_up: sm.catch_up,
            catch_up_on_end: sm.catch_up_on_end,
            adjust_for_zoom: sm.adjust_for_zoom,
        };
    } else {
        settings.smoothing = Smoothing { enabled: false, amount: 0.0, ..Default::default() };
    }
    Ok(settings)
}

// ---------------------------------------------------------------------------
// Cached prepared tips & textures
// ---------------------------------------------------------------------------

struct PreparedAssets {
    tip: Option<Arc<Mips>>,
    dual_tip: Option<Arc<Mips>>,
    texture: Option<Arc<PatternImage>>,
}

fn asset_cache_key(brush: &StrokeBrush) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    if let Ok(bytes) = serde_json::to_vec(&brush.tip) {
        h.update(&bytes);
    }
    h.update(&[0xff]);
    if let Some(dual) = &brush.dual_brush
        && let Ok(bytes) = serde_json::to_vec(&dual.tip)
    {
        h.update(&bytes);
    }
    h.update(&[0xfe]);
    if let Some(tx) = &brush.texture {
        if let Ok(bytes) = serde_json::to_vec(&tx.pattern) {
            h.update(&bytes);
        }
        h.update(&[u8::from(tx.invert)]);
        h.update(&tx.brightness.to_bits().to_le_bytes());
        h.update(&tx.contrast.to_bits().to_le_bytes());
    }
    *h.finalize().as_bytes()
}

type AssetCacheEntry = ([u8; 32], Arc<PreparedAssets>);

fn prepare_assets(brush: &StrokeBrush) -> Result<Arc<PreparedAssets>> {
    static CACHE: OnceLock<Mutex<Vec<AssetCacheEntry>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    let key = asset_cache_key(brush);
    {
        let mut guard = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(pos) = guard.iter().position(|(k, _)| *k == key) {
            let entry = guard.remove(pos);
            let value = Arc::clone(&entry.1);
            guard.push(entry);
            return Ok(value);
        }
    }
    let tip = match &brush.tip {
        BrushTip::Round => None,
        BrushTip::Procedural { style, size, seed, param } => Some(Arc::new(Mips::new(&generate_procedural_tip(*style, *size, *seed, *param)))),
        BrushTip::Sampled { width, height, png_b64 } => Some(Arc::new(Mips::new(&decode_gray_tile_png(*width, *height, png_b64)?))),
    };
    let dual_tip = match brush.dual_brush.as_deref().map(|d| &d.tip) {
        None | Some(BrushTip::Round) => None,
        Some(BrushTip::Procedural { style, size, seed, param }) => Some(Arc::new(Mips::new(&generate_procedural_tip(*style, *size, *seed, *param)))),
        Some(BrushTip::Sampled { width, height, png_b64 }) => Some(Arc::new(Mips::new(&decode_gray_tile_png(*width, *height, png_b64)?))),
    };
    let texture = match &brush.texture {
        None => None,
        Some(tx) => {
            let mut img = match &tx.pattern {
                BrushPattern::Procedural { style, size, seed } => procedural_pattern(pattern_style_to_paint(*style), *size, *seed),
                BrushPattern::Tile { width, height, png_b64 } => {
                    let g = decode_gray_tile_png(*width, *height, png_b64)?;
                    PatternImage { width: g.width as usize, height: g.height as usize, data: g.to_f32() }
                }
            };
            let paint_tx =
                photocraft_paint::Texture { enabled: true, invert: tx.invert, brightness: tx.brightness, contrast: tx.contrast, ..Default::default() };
            for v in &mut img.data {
                *v = paint_tx.level(*v);
            }
            Some(Arc::new(img))
        }
    };
    let prepared = Arc::new(PreparedAssets { tip, dual_tip, texture });
    {
        let mut guard = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.len() >= 64 {
            guard.remove(0);
        }
        guard.push((key, Arc::clone(&prepared)));
    }
    Ok(prepared)
}

// ---------------------------------------------------------------------------
// Multi-scale tip math & texture sampling
// ---------------------------------------------------------------------------

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn wet_factor(rn: f32) -> f32 {
    0.5 + 0.5 * smoothstep(0.5, 1.0, rn)
}

#[inline]
fn soft_exp(u: f32) -> f32 {
    let mut g = (1.0 - u / 64.0).clamp(0.0, 1.0);
    for _ in 0..6 {
        g *= g;
    }
    g
}

#[derive(Clone, Copy)]
struct ScaledTipProfile {
    core: f32,
    shoulder: f32,
    reach: f32,
}

impl ScaledTipProfile {
    #[inline]
    fn new(radius: f32, hardness: f32, phys_per_brush: f32) -> Self {
        let h = if hardness.is_nan() { 0.0 } else { hardness.clamp(0.0, 1.0) } * 10.0;
        let i = (h.floor() as usize).min(9);
        let t = h - i as f32;
        let a = TIP_SHAPE.get(i).copied().unwrap_or((0.0, 0.664));
        let b = TIP_SHAPE.get(i + 1).copied().unwrap_or((1.0, 0.0));
        let (r0, s) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        let radius = if radius.is_finite() { radius.max(0.0) } else { 0.0 };
        // At 1:1 scale (phys_per_brush == 1.0), identical to `photocraft_paint::tip_shape`.
        // When zoomed in (phys_per_brush > 1.0), keep the hard AA edge crisp (~1.2 physical px)
        // while preserving soft Gaussian shoulders (`s * radius`) in brush-space.
        let zoom_in = phys_per_brush.max(1.0);
        let edge = (TIP_EDGE_PX / zoom_in).min(0.3 * radius).max(0.25 / zoom_in);
        let core = (r0 * radius).min(radius - edge).max(0.0);
        let shoulder = (s * radius).max(edge).max(1e-4 / zoom_in);
        Self { core, shoulder, reach: core + 2.6 * shoulder }
    }

    #[inline]
    fn falloff(self, d: f32) -> f32 {
        if d <= self.core {
            return 1.0;
        }
        let x = (d - self.core) / self.shoulder;
        soft_exp(x * x)
    }

    #[inline]
    fn coverage(self, d: f32, slack_brush: f32) -> f32 {
        if d.is_nan() || d >= self.reach + slack_brush {
            return 0.0;
        }
        self.falloff(d)
    }
}

#[inline]
fn sample_texture(pattern: &PatternImage, scale: f32, bx: f64, by: f64) -> f32 {
    let s = f64::from(scale.max(0.01));
    let fx = bx / s - 0.5;
    let fy = by / s - 0.5;
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
    let w = pattern.width as i64;
    let h = pattern.height as i64;
    let get = |xi: i64, yi: i64| {
        let ux = xi.rem_euclid(w) as usize;
        let uy = yi.rem_euclid(h) as usize;
        pattern.data.get(uy * pattern.width + ux).copied().unwrap_or(1.0)
    };
    let (xi, yi) = (x0 as i64, y0 as i64);
    let a = get(xi, yi) + (get(xi + 1, yi) - get(xi, yi)) * tx;
    let b = get(xi, yi + 1) + (get(xi + 1, yi + 1) - get(xi, yi + 1)) * tx;
    a + (b - a) * ty
}

// ---------------------------------------------------------------------------
// Exact viewport <-> canonical brush-space transform
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct BrushTransform {
    phys_w: u32,
    phys_h: u32,
    /// Camera center expressed in canonical brush-space (106-bit precision).
    cam_bx: Double,
    cam_by: Double,
    /// Physical pixels per 1 brush-space unit along X and Y.
    phys_per_brush_x: f64,
    phys_per_brush_y: f64,
    /// Brush-space units per 1 physical pixel along X and Y.
    brush_per_phys_x: f64,
    brush_per_phys_y: f64,
    /// Effective isotropic physical pixels per brush unit (for mip level & AA edge).
    phys_per_brush: f32,
    /// Seamless brush-space texture/noise origin offset (`p0 * brush_per_local`).
    tex_origin_x: f64,
    tex_origin_y: f64,
}

impl BrushTransform {
    fn new(stroke: &Stroke, brush: &StrokeBrush, view: &Viewport, density: f32) -> Result<Self> {
        let p0 = *stroke.points.first().ok_or_else(|| err("Brush stroke has no points"))?;
        let phys_w = (f64::from(view.width) * f64::from(density)).ceil() as u32;
        let phys_h = (f64::from(view.height) * f64::from(density)).ceil() as u32;
        if phys_w == 0 || phys_h == 0 {
            return Err(err("Empty viewport"));
        }
        let p0_world = coordinates::point(&stroke.anchor, p0).map_err(|e| err(e.to_string()))?;
        let cam_world = coordinates::center(&view.camera).map_err(|e| err(e.to_string()))?;
        let anchor_unit = coordinates::local_unit(stroke.anchor.depth).map_err(|e| err(e.to_string()))?;
        let ratio = coordinates::scale_ratio(stroke.anchor.depth, &view.camera).map_err(|e| err(e.to_string()))?;
        let brush_per_local = f64::from(brush.authored_size) / stroke.width;
        let local_per_brush = stroke.width / f64::from(brush.authored_size);
        let scale_x = f64::from(phys_w) / f64::from(view.width);
        let scale_y = f64::from(phys_h) / f64::from(view.height);
        let phys_per_brush_x = ratio * local_per_brush * scale_x;
        let phys_per_brush_y = ratio * local_per_brush * scale_y;
        if !phys_per_brush_x.is_finite() || !phys_per_brush_y.is_finite() || phys_per_brush_x <= 0.0 || phys_per_brush_y <= 0.0 {
            return Err(err("Non-finite brush view scale"));
        }
        let inv_unit = Double::scalar(brush_per_local).div(Double::integer(anchor_unit));
        let cam_bx = Double::integer(cam_world.x.0 - p0_world.x.0).mul(inv_unit);
        let cam_by = Double::integer(cam_world.y.0 - p0_world.y.0).mul(inv_unit);
        // Keep texture coordinates aligned across adjacent strokes in the same region,
        // wrapped to a large power-of-two period so f64 precision stays crisp everywhere.
        let period = 65536.0;
        let tex_origin_x = (p0[0] * brush_per_local).rem_euclid(period);
        let tex_origin_y = (p0[1] * brush_per_local).rem_euclid(period);
        Ok(Self {
            phys_w,
            phys_h,
            cam_bx,
            cam_by,
            phys_per_brush_x,
            phys_per_brush_y,
            brush_per_phys_x: 1.0 / phys_per_brush_x,
            brush_per_phys_y: 1.0 / phys_per_brush_y,
            phys_per_brush: phys_per_brush_x.min(phys_per_brush_y) as f32,
            tex_origin_x,
            tex_origin_y,
        })
    }

    /// Project a canonical brush-space box `[cx - reach, cx + reach] × [cy - reach, cy + reach]`
    /// to clamped physical viewport pixel bounds `[x0, y0, x1, y1)`.
    fn clip_dab_rect(&self, cx: f64, cy: f64, reach_brush: f64) -> Option<(u32, u32, u32, u32)> {
        let half_w = f64::from(self.phys_w) * 0.5;
        let half_h = f64::from(self.phys_h) * 0.5;
        let rel_x = Double::scalar(cx).sub(self.cam_bx);
        let rel_y = Double::scalar(cy).sub(self.cam_by);
        let sx_min = rel_x.sub(Double::scalar(reach_brush)).mul(Double::scalar(self.phys_per_brush_x)).value() + half_w;
        let sx_max = rel_x.add(Double::scalar(reach_brush)).mul(Double::scalar(self.phys_per_brush_x)).value() + half_w;
        let sy_min = rel_y.sub(Double::scalar(reach_brush)).mul(Double::scalar(self.phys_per_brush_y)).value() + half_h;
        let sy_max = rel_y.add(Double::scalar(reach_brush)).mul(Double::scalar(self.phys_per_brush_y)).value() + half_h;
        if sx_max <= 0.0 || sy_max <= 0.0 || sx_min >= f64::from(self.phys_w) || sy_min >= f64::from(self.phys_h) {
            return None;
        }
        let x0 = (sx_min.floor() - 1.0).clamp(0.0, f64::from(self.phys_w)) as u32;
        let x1 = (sx_max.ceil() + 1.0).clamp(0.0, f64::from(self.phys_w)) as u32;
        let y0 = (sy_min.floor() - 1.0).clamp(0.0, f64::from(self.phys_h)) as u32;
        let y1 = (sy_max.ceil() + 1.0).clamp(0.0, f64::from(self.phys_h)) as u32;
        (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
    }

    /// Map a canonical brush-space point to logical viewport screen coordinates (for smoothing string).
    fn brush_to_logical(&self, bx: f64, by: f64, view: &Viewport) -> [f64; 2] {
        let half_w = f64::from(self.phys_w) * 0.5;
        let half_h = f64::from(self.phys_h) * 0.5;
        let px = Double::scalar(bx).sub(self.cam_bx).mul(Double::scalar(self.phys_per_brush_x)).value() + half_w;
        let py = Double::scalar(by).sub(self.cam_by).mul(Double::scalar(self.phys_per_brush_y)).value() + half_h;
        [px * f64::from(view.width) / f64::from(self.phys_w), py * f64::from(view.height) / f64::from(self.phys_h)]
    }
}

// ---------------------------------------------------------------------------
// Sparse-tiled viewport stroke accumulation
// ---------------------------------------------------------------------------

const TILE_PIXELS: usize = (WORLD_BRUSH_TILE * WORLD_BRUSH_TILE) as usize;

#[derive(Clone)]
struct AccumTile {
    cov: Vec<f32>,
    depth: Vec<f32>,
    col: Vec<[f32; 4]>,
    dual: Vec<f32>,
}

impl AccumTile {
    fn new() -> Self {
        Self { cov: vec![0.0; TILE_PIXELS], depth: Vec::new(), col: Vec::new(), dual: Vec::new() }
    }
}

/// One finalized 64×64 viewport tile of a rasterized brush stroke.
#[derive(Clone, Debug)]
pub struct BrushTile {
    pub x0: u32,
    pub y0: u32,
    pub width: u32,
    pub height: u32,
    /// Always `64 * 64` entries in row-major order (only `0..height` × `0..width` are populated).
    /// When `direct_premul`: premultiplied RGBA `[r*a, g*a, b*a, a]` (or `[0, 0, 0, k]` for erase).
    /// Otherwise (non-Normal blend modes): straight RGBA `[r, g, b, src_a]` paired with `mask`.
    pub pixels: Vec<[f32; 4]>,
    pub mask: Vec<f32>,
}

impl BrushTile {
    /// Convert this direct-premultiplied tile into `64 * 64 * 4` RGBA8 bytes (256-byte rows,
    /// ready for `wgpu` `copy_buffer_to_texture` with zero padding overhead!).
    pub fn to_premul_rgba8_padded(&self, aliased: bool) -> Vec<u8> {
        let mut out = vec![0u8; TILE_PIXELS * 4];
        let stride = WORLD_BRUSH_TILE as usize;
        for ly in 0..self.height as usize {
            let py = (self.y0 as usize + ly) as i32;
            for lx in 0..self.width as usize {
                let i = ly * stride + lx;
                let Some(px_val) = self.pixels.get(i) else { continue };
                if px_val[3] <= 0.0 {
                    continue;
                }
                let mut v = *px_val;
                if !aliased {
                    let px = (self.x0 as usize + lx) as i32;
                    let d = (hash2(px, py, DITHER_SALT) - 0.5) / 255.0;
                    for ch in &mut v {
                        *ch += d;
                    }
                }
                let a = (v[3].clamp(0.0, 1.0) * 255.0).round() as u8;
                let r = ((v[0].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                let g = ((v[1].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                let b = ((v[2].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                let o = i * 4;
                if let Some(slot) = out.get_mut(o..o + 4) {
                    slot[0] = r;
                    slot[1] = g;
                    slot[2] = b;
                    slot[3] = a;
                }
            }
        }
        out
    }
}

/// Finalized sparse-tiled output of rasterizing one shared-canvas brush stroke.
#[derive(Clone, Debug)]
pub struct RasterizedBrushStroke {
    pub mode: BlendMode,
    pub behind: bool,
    pub erase: bool,
    pub aliased: bool,
    pub phys_w: u32,
    pub phys_h: u32,
    pub tiles: Vec<BrushTile>,
}

impl RasterizedBrushStroke {
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
    /// True when the stroke can be composited directly onto the premultiplied device
    /// scratch surface via hardware blend states (`SrcOver`, `DstOver`, or `Erase`).
    pub fn is_direct_premul(&self) -> bool {
        self.erase || self.behind || matches!(self.mode, BlendMode::Normal | BlendMode::Dissolve)
    }
}

struct Accumulator {
    brush: StrokeBrush,
    color: [f32; 4],
    erase: bool,
    assets: Arc<PreparedAssets>,
    xf: BrushTransform,
    per_dab_color: bool,
    tiles: BTreeMap<(u32, u32), AccumTile>,
    dabs_done: usize,
}

struct DabSampler<'a> {
    cx: f64,
    cy: f64,
    dbx: f64,
    dby: f64,
    aliased: bool,
    square: bool,
    aliased_supersample: bool,
    small: bool,
    r: f32,
    ro: f32,
    rm: f32,
    subpixel_att: f32,
    slack_brush: f32,
    reach2: f32,
    small_reach2: f32,
    profile: ScaledTipProfile,
    sn: f32,
    cs: f32,
    fx: f32,
    fy: f32,
    proj: Option<(f32, f32, f32)>,
    mips: Option<&'a Mips>,
    level: usize,
    inv_scale: f32,
    tw: f32,
    th: f32,
}

impl DabSampler<'_> {
    #[inline]
    fn to_tip(&self, dx: f32, dy: f32) -> (f32, f32) {
        let (mut ux, mut uy) = (dx, -dy);
        if let Some((ax, ay, k)) = self.proj {
            let t = (ux * ax + uy * ay) * k;
            ux += t * ax;
            uy += t * ay;
        }
        ((ux * self.cs + uy * self.sn) * self.fx, (-ux * self.sn + uy * self.cs) * self.fy)
    }

    fn sample_square(&self, dx_base: f64, dy_base: f64) -> Option<(f32, f32)> {
        if self.aliased_supersample {
            let mut hits = 0u32;
            for sy in SUBPIXEL {
                for sx in SUBPIXEL {
                    let bx = (self.cx + dx_base + sx * self.dbx).floor() + 0.5;
                    let by = (self.cy + dy_base + sy * self.dby).floor() + 0.5;
                    if ((bx - self.cx).abs() as f32) < self.r && ((by - self.cy).abs() as f32) < self.r {
                        hits += 1;
                    }
                }
            }
            Some((hits as f32 / 16.0, 0.0))
        } else {
            let bx = (self.cx + dx_base).floor() + 0.5;
            let by = (self.cy + dy_base).floor() + 0.5;
            (((bx - self.cx).abs() as f32) < self.r && ((by - self.cy).abs() as f32) < self.r).then_some((1.0, 0.0))
        }
    }

    fn sample_aliased_round(&self, dx_base: f64, dy_base: f64) -> Option<(f32, f32)> {
        if self.aliased_supersample {
            let mut hits = 0u32;
            for sy in SUBPIXEL {
                for sx in SUBPIXEL {
                    let bx = (self.cx + dx_base + sx * self.dbx).floor() + 0.5;
                    let by = (self.cy + dy_base + sy * self.dby).floor() + 0.5;
                    let (u, v) = self.to_tip((bx - self.cx) as f32, (by - self.cy) as f32);
                    if ((u * self.ro).powi(2) + v * v).sqrt() <= self.rm {
                        hits += 1;
                    }
                }
            }
            Some((hits as f32 / 16.0, 0.0))
        } else {
            let bx = (self.cx + dx_base).floor() + 0.5;
            let by = (self.cy + dy_base).floor() + 0.5;
            let (u, v) = self.to_tip((bx - self.cx) as f32, (by - self.cy) as f32);
            let d2 = (u * self.ro).powi(2) + v * v;
            if d2 >= self.reach2 {
                return None;
            }
            let dd = d2.sqrt();
            Some((if dd <= self.rm { 1.0 } else { 0.0 }, dd / self.rm))
        }
    }

    fn sample(&self, dx_base: f64, dy_base: f64) -> Option<(f32, f32)> {
        if self.aliased && self.square {
            return self.sample_square(dx_base, dy_base);
        }
        match self.mips {
            None if self.small => {
                let (u0, v0) = self.to_tip(dx_base as f32, dy_base as f32);
                let d2 = (u0 * self.ro).powi(2) + v0 * v0;
                if d2 >= self.small_reach2 {
                    return None;
                }
                let mut sum = 0.0f32;
                for sy in SUBPIXEL {
                    for sx in SUBPIXEL {
                        let (su, sv) = self.to_tip((dx_base + sx * self.dbx) as f32, (dy_base + sy * self.dby) as f32);
                        let sd = ((su * self.ro).powi(2) + sv * sv).sqrt();
                        sum += self.profile.falloff(sd);
                    }
                }
                Some((sum / 16.0, d2.sqrt() / self.rm))
            }
            None if self.aliased => self.sample_aliased_round(dx_base, dy_base),
            None => {
                let (u, v) = self.to_tip(dx_base as f32, dy_base as f32);
                let d2 = (u * self.ro).powi(2) + v * v;
                if d2 >= self.reach2 {
                    return None;
                }
                let dd = d2.sqrt();
                Some((self.profile.coverage(dd, self.slack_brush) * self.subpixel_att, dd / self.rm))
            }
            Some(m) => {
                let (dx_s, dy_s) = if self.aliased {
                    let bx = (self.cx + dx_base).floor() + 0.5;
                    let by = (self.cy + dy_base).floor() + 0.5;
                    ((bx - self.cx) as f32, (by - self.cy) as f32)
                } else {
                    (dx_base as f32, dy_base as f32)
                };
                let (u, v) = self.to_tip(dx_s, dy_s);
                let (txp, typ) = (u * self.inv_scale + self.tw * 0.5, -(v / self.ro) * self.inv_scale + self.th * 0.5);
                if txp < -1.0 || typ < -1.0 || txp > self.tw + 1.0 || typ > self.th + 1.0 {
                    return None;
                }
                let s = m.sample_clamped(self.level, txp / self.tw, typ / self.th);
                let val = if self.aliased { if s >= 0.5 { 1.0 } else { 0.0 } } else { s };
                Some((val, 1.0 - s))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn deposit_dab_pixel(tile: &mut AccumTile, ti: usize, d: &Dab, dual: bool, wet: bool, per_dab_color: bool, v: f32, rn: f32) {
    if dual {
        if tile.dual.is_empty() {
            tile.dual = vec![0.0; TILE_PIXELS];
        }
        if let Some(slot) = tile.dual.get_mut(ti) {
            let c = *slot;
            if 1.0 > c {
                *slot = c + v * (1.0 - c);
            }
        }
        return;
    }
    if wet {
        if tile.depth.is_empty() {
            tile.depth = vec![0.0; TILE_PIXELS];
        }
        if let Some(slot) = tile.depth.get_mut(ti) {
            *slot = slot.max((1.0 - rn).clamp(0.0, 1.0));
        }
    }
    if let Some(cov_slot) = tile.cov.get_mut(ti) {
        let c = *cov_slot;
        let nv = if d.opacity > c { c + v * (d.opacity - c) } else { c };
        if nv > c {
            if per_dab_color {
                if tile.col.is_empty() {
                    tile.col = vec![[0.0; 4]; TILE_PIXELS];
                }
                let k = if c < 1.0 { (nv - c) / (1.0 - c) } else { 0.0 };
                if let Some(col_slot) = tile.col.get_mut(ti) {
                    for (dst, &dc) in col_slot.iter_mut().zip(&d.color) {
                        *dst = dc * k + *dst * (1.0 - k);
                    }
                }
            }
            *cov_slot = nv;
        }
    }
}

impl Accumulator {
    fn new(stroke: &Stroke, brush: &StrokeBrush, assets: Arc<PreparedAssets>, xf: BrushTransform) -> Self {
        let per_dab_color = brush.color_dynamics.is_some() && !stroke.erase;
        Self {
            brush: brush.clone(),
            // Opacity lives exclusively on `brush.opacity`; `stroke.color.rgba[3]` may carry a
            // summary opacity value for metadata/thumbnails and must never multiply opacity twice.
            color: [stroke.color.rgba[0], stroke.color.rgba[1], stroke.color.rgba[2], 1.0],
            erase: stroke.erase,
            assets,
            xf,
            per_dab_color,
            tiles: BTreeMap::new(),
            dabs_done: 0,
        }
    }

    fn dab_reach_brush(&self, d: &Dab, dual: bool) -> f64 {
        let sampled = if dual { self.assets.dual_tip.is_some() } else { self.assets.tip.is_some() };
        let hardness = if dual { self.brush.dual_brush.as_deref().map_or(1.0, |db| db.hardness) } else { self.brush.hardness };
        let aliased = self.brush.aliased && !dual;
        let reach = if sampled || (aliased && self.brush.square) {
            f64::from(d.radius) * std::f64::consts::SQRT_2
        } else if aliased {
            f64::from(d.radius)
        } else {
            let profile = ScaledTipProfile::new(d.radius, hardness, self.xf.phys_per_brush);
            f64::from(profile.reach.max(d.radius))
        };
        reach + self.xf.brush_per_phys_x.max(self.xf.brush_per_phys_y)
    }

    fn accumulate_dab(&mut self, d: &Dab, dual: bool, work: &mut u64, max_work: u64) -> Result<()> {
        if self.dabs_done >= MAX_STROKE_DABS {
            return Ok(());
        }
        self.dabs_done += 1;
        let b = &self.brush;
        let aliased = b.aliased && !dual;
        let (cx, cy) = if aliased {
            let (gx, gy) = grid_center(d.center.x, d.center.y, 2.0 * d.radius);
            (f64::from(gx), f64::from(gy))
        } else {
            (d.center.x, d.center.y)
        };
        let reach_b = self.dab_reach_brush(d, dual);
        let Some((x0, y0, x1, y1)) = self.xf.clip_dab_rect(cx, cy, reach_b) else {
            return Ok(());
        };
        let samples = u64::from(x1 - x0).saturating_mul(u64::from(y1 - y0));
        *work = work.saturating_add(samples);
        if *work > max_work {
            return Err(err("World brush render budget reached; choose a closer view or smaller output"));
        }

        let (hardness, mips) = if dual {
            (b.dual_brush.as_deref().map_or(1.0, |db| db.hardness), self.assets.dual_tip.as_deref())
        } else {
            (b.hardness, self.assets.tip.as_deref())
        };
        let wet = b.wet_edges && !dual;
        let noise = b.noise && !dual;
        let tex_tip = !dual && b.texture.as_ref().is_some_and(|tx| tx.each_tip) && self.assets.texture.is_some();
        let tex_scale = b.texture.as_ref().map_or(1.0, |tx| tx.scale);
        let tex_mode = b.texture.as_ref().map_or(MaskMode::Multiply, |tx| mask_mode_to_paint(tx.mode));

        let (sn, cs) = d.angle.sin_cos();
        let (fx, fy) = (if d.flip_x { -1.0f32 } else { 1.0f32 }, if d.flip_y { -1.0f32 } else { 1.0f32 });
        let proj = (!dual && d.proj_scale < 0.999).then(|| {
            let (ps, pc) = d.proj_angle.sin_cos();
            (pc, ps, 1.0 / d.proj_scale.max(0.05) - 1.0)
        });
        let r = d.radius;
        let ro = d.roundness.max(0.5 / r).min(1.0);
        let rm_true = (r * ro).max(0.5);
        let rm_phys = rm_true * self.xf.phys_per_brush;
        // Sub-pixel attenuation when zoomed far out: keep hairlines visible smoothly.
        let (rm, subpixel_att) = if !aliased && mips.is_none() && rm_phys < 0.5 && self.xf.phys_per_brush > 0.0 {
            ((0.5 / self.xf.phys_per_brush).max(rm_true), (rm_phys / 0.5).clamp(0.0, 1.0))
        } else {
            (rm_true, 1.0)
        };
        let profile = ScaledTipProfile::new(rm, hardness, self.xf.phys_per_brush);
        let slack_brush = (0.5 / self.xf.phys_per_brush.max(1e-6)).min(rm.max(0.5));
        let reach = if aliased || mips.is_some() { rm } else { profile.reach.max(rm) };
        let reach2 = (reach + slack_brush) * (reach + slack_brush);
        let small = mips.is_none() && !aliased && rm_phys < SMALL_TIP_RADIUS && subpixel_att >= 1.0;
        let small_reach2 = (reach + 0.75 * self.xf.brush_per_phys_x.max(self.xf.brush_per_phys_y) as f32).powi(2);
        let aliased_supersample = aliased && self.xf.phys_per_brush < 0.75;

        let (level, inv_scale, tw, th) = match mips {
            Some(m) => {
                let (w0, h0, _) = m.levels.first().map_or((1, 1, &[][..]), |(w, h, d)| (*w, *h, d.as_slice()));
                let big = w0.max(h0) as f32;
                let phys_diam = (2.0 * r * ro.max(0.25) * self.xf.phys_per_brush).max(1.0);
                (m.level_for(phys_diam), big / (2.0 * r), w0 as f32, h0 as f32)
            }
            None => (0, 1.0, 1.0, 1.0),
        };

        let sampler = DabSampler {
            cx,
            cy,
            dbx: self.xf.brush_per_phys_x,
            dby: self.xf.brush_per_phys_y,
            aliased,
            square: b.square,
            aliased_supersample,
            small,
            r,
            ro,
            rm,
            subpixel_att,
            slack_brush,
            reach2,
            small_reach2,
            profile,
            sn,
            cs,
            fx,
            fy,
            proj,
            mips,
            level,
            inv_scale,
            tw,
            th,
        };

        // Precompute exact 106-bit camera-center offset relative to this dab center once per dab!
        let dab_off_x = self.xf.cam_bx.sub(Double::scalar(cx)).value();
        let dab_off_y = self.xf.cam_by.sub(Double::scalar(cy)).value();
        let half_w = f64::from(self.xf.phys_w) * 0.5;
        let half_h = f64::from(self.xf.phys_h) * 0.5;
        let dbx = self.xf.brush_per_phys_x;
        let dby = self.xf.brush_per_phys_y;
        let stride = WORLD_BRUSH_TILE as usize;

        let t0x = x0 / WORLD_BRUSH_TILE;
        let t1x = (x1 - 1) / WORLD_BRUSH_TILE;
        let t0y = y0 / WORLD_BRUSH_TILE;
        let t1y = (y1 - 1) / WORLD_BRUSH_TILE;

        for ty in t0y..=t1y {
            let py0 = (ty * WORLD_BRUSH_TILE).max(y0);
            let py1 = ((ty + 1) * WORLD_BRUSH_TILE).min(y1);
            for tx in t0x..=t1x {
                let px0 = (tx * WORLD_BRUSH_TILE).max(x0);
                let px1 = ((tx + 1) * WORLD_BRUSH_TILE).min(x1);
                let mut tile_opt: Option<AccumTile> = self.tiles.remove(&(tx, ty));

                for py in py0..py1 {
                    let dy_base = dab_off_y + (f64::from(py) + 0.5 - half_h) * dby;
                    let ly = (py - ty * WORLD_BRUSH_TILE) as usize;
                    for px in px0..px1 {
                        let dx_base = dab_off_x + (f64::from(px) + 0.5 - half_w) * dbx;
                        let Some((mut val, rn)) = sampler.sample(dx_base, dy_base) else {
                            continue;
                        };
                        if val <= 0.0 {
                            continue;
                        }
                        let world_bx = cx + dx_base + self.xf.tex_origin_x;
                        let world_by = cy + dy_base + self.xf.tex_origin_y;
                        if noise && val < 1.0 {
                            let n = hash2(world_bx.floor() as i32, world_by.floor() as i32, NOISE_SALT);
                            let hard = if n < val { 1.0 } else { 0.0 };
                            val += (hard - val) * 0.7;
                        }
                        if tex_tip && let Some(pattern) = self.assets.texture.as_deref() {
                            val = texture_combine(tex_mode, val, sample_texture(pattern, tex_scale, world_bx, world_by), d.depth);
                        }
                        let v = val * d.alpha;
                        if v <= 0.0 {
                            continue;
                        }
                        let tile = tile_opt.get_or_insert_with(AccumTile::new);
                        let lx = (px - tx * WORLD_BRUSH_TILE) as usize;
                        deposit_dab_pixel(tile, ly * stride + lx, d, dual, wet, self.per_dab_color, v, rn);
                    }
                }
                if let Some(tile) = tile_opt {
                    self.tiles.insert((tx, ty), tile);
                }
            }
        }
        Ok(())
    }

    fn finalize(&self) -> RasterizedBrushStroke {
        let mode = blend_mode_to_paint(self.brush.mode);
        let behind = self.brush.behind && !self.erase;
        let direct_premul = self.erase || behind || matches!(mode, BlendMode::Normal | BlendMode::Dissolve);
        let opacity = self.brush.opacity.clamp(0.0, 1.0);
        let dual_mode = self.brush.dual_brush.as_deref().map(|d| mask_mode_to_paint(d.mode));
        let stroke_tex = self
            .brush
            .texture
            .as_ref()
            .filter(|tx| !tx.each_tip)
            .and_then(|tx| self.assets.texture.as_deref().map(|pat| (pat, tx.scale, mask_mode_to_paint(tx.mode), tx.depth.clamp(0.0, 1.0))));
        let half_w = f64::from(self.xf.phys_w) * 0.5;
        let half_h = f64::from(self.xf.phys_h) * 0.5;
        let cam_bx = self.xf.cam_bx.value();
        let cam_by = self.xf.cam_by.value();
        let dbx = self.xf.brush_per_phys_x;
        let dby = self.xf.brush_per_phys_y;
        let stride = WORLD_BRUSH_TILE as usize;

        let mut out_tiles = Vec::with_capacity(self.tiles.len());
        for (&(tx, ty), tile) in &self.tiles {
            let x0 = tx * WORLD_BRUSH_TILE;
            let y0 = ty * WORLD_BRUSH_TILE;
            let width = self.xf.phys_w.saturating_sub(x0).min(WORLD_BRUSH_TILE);
            let height = self.xf.phys_h.saturating_sub(y0).min(WORLD_BRUSH_TILE);
            if width == 0 || height == 0 {
                continue;
            }
            let mut pixels = vec![[0.0f32; 4]; TILE_PIXELS];
            let mut mask = if direct_premul { Vec::new() } else { vec![0.0f32; TILE_PIXELS] };
            let mut any_nonzero = false;

            for ly in 0..height as usize {
                let py = y0 + ly as u32;
                let world_by = cam_by + (f64::from(py) + 0.5 - half_h) * dby + self.xf.tex_origin_y;
                for lx in 0..width as usize {
                    let i = ly * stride + lx;
                    let c = tile.cov.get(i).copied().unwrap_or(0.0);
                    if c <= 0.0 {
                        continue;
                    }
                    let px = x0 + lx as u32;
                    let world_bx = cam_bx + (f64::from(px) + 0.5 - half_w) * dbx + self.xf.tex_origin_x;
                    let mut m = if self.brush.wet_edges {
                        let dp = tile.depth.get(i).copied().unwrap_or(0.0);
                        c * wet_factor(1.0 - dp)
                    } else {
                        c
                    };
                    if let Some(dm) = dual_mode {
                        let dv = tile.dual.get(i).copied().unwrap_or(0.0);
                        m = mask_combine(dm, m, dv, 1.0);
                    }
                    if let Some((pat, scale, tmode, depth)) = stroke_tex {
                        m = texture_combine(tmode, m, sample_texture(pat, scale, world_bx, world_by), depth);
                    }
                    let mut k = (m * opacity).clamp(0.0, 1.0);
                    if k <= 0.0 {
                        continue;
                    }
                    if mode == BlendMode::Dissolve {
                        k = if hash2(world_bx.floor() as i32, world_by.floor() as i32, self.brush.seed) < k { 1.0 } else { 0.0 };
                        if k <= 0.0 {
                            continue;
                        }
                    }
                    any_nonzero = true;
                    if self.erase {
                        if let Some(slot) = pixels.get_mut(i) {
                            *slot = [0.0, 0.0, 0.0, k];
                        }
                    } else {
                        let src = if self.per_dab_color && !tile.col.is_empty() {
                            let p = tile.col.get(i).copied().unwrap_or([0.0; 4]);
                            [(p[0] / c).clamp(0.0, 1.0), (p[1] / c).clamp(0.0, 1.0), (p[2] / c).clamp(0.0, 1.0), (p[3] / c).clamp(0.0, 1.0)]
                        } else {
                            self.color
                        };
                        if direct_premul {
                            let sa = (src[3] * k).clamp(0.0, 1.0);
                            if let Some(slot) = pixels.get_mut(i) {
                                *slot = [src[0] * sa, src[1] * sa, src[2] * sa, sa];
                            }
                        } else {
                            if let Some(slot) = pixels.get_mut(i) {
                                *slot = src;
                            }
                            if let Some(mslot) = mask.get_mut(i) {
                                *mslot = k;
                            }
                        }
                    }
                }
            }
            if any_nonzero {
                out_tiles.push(BrushTile { x0, y0, width, height, pixels, mask });
            }
        }
        RasterizedBrushStroke { mode, behind, erase: self.erase, aliased: self.brush.aliased, phys_w: self.xf.phys_w, phys_h: self.xf.phys_h, tiles: out_tiles }
    }
}

/// Build canonical brush-space `StrokePoint`s from a shared-canvas `Stroke`.
pub fn stroke_points_in_brush_space(stroke: &Stroke, brush: &StrokeBrush) -> Vec<StrokePoint> {
    let Some(&p0) = stroke.points.first() else {
        return Vec::new();
    };
    let brush_per_local = f64::from(brush.authored_size) / stroke.width;
    let mut out = Vec::with_capacity(stroke.points.len());
    for (idx, p) in stroke.points.iter().enumerate() {
        let sample = stroke.samples.as_ref().and_then(|s| s.get(idx)).copied().unwrap_or_default();
        out.push(StrokePoint {
            x: (p[0] - p0[0]) * brush_per_local,
            y: (p[1] - p0[1]) * brush_per_local,
            pressure: sample.pressure,
            tilt_x: sample.tilt_x,
            tilt_y: sample.tilt_y,
            rotation: sample.rotation,
            wheel: sample.wheel,
            time: sample.time_ms,
        });
    }
    out
}

/// Rasterize one shared-canvas brush stroke clipped to `view` at `density`.
/// Returns `Ok(None)` when `stroke.brush` is `None` or the stroke is completely offscreen.
pub fn rasterize_world_stroke(stroke: &Stroke, view: &Viewport, density: f32, work: &mut u64, max_work: u64) -> Result<Option<RasterizedBrushStroke>> {
    let Some(brush) = stroke.brush.as_deref() else {
        return Ok(None);
    };
    let xf = BrushTransform::new(stroke, brush, view, density)?;
    let assets = prepare_assets(brush)?;
    let settings = to_paint_settings(stroke, brush)?;
    let smoothing_zoom = brush.smoothing.as_ref().map_or(1.0, |sm| sm.zoom);
    let mut generator = DabGenerator::new(&settings, smoothing_zoom);
    let points = stroke_points_in_brush_space(stroke, brush);
    let mut dabs = Vec::new();
    let mut duals = Vec::new();
    generator.push(&points, &mut dabs, &mut duals);
    generator.finish(&mut dabs, &mut duals);

    let mut accum = Accumulator::new(stroke, brush, assets, xf);
    for d in &dabs {
        accum.accumulate_dab(d, false, work, max_work)?;
    }
    for d in &duals {
        accum.accumulate_dab(d, true, work, max_work)?;
    }
    let result = accum.finalize();
    Ok((!result.is_empty()).then_some(result))
}

/// Composite one finalized `RasterizedBrushStroke` onto a premultiplied `[f32; 4]`
/// device layer buffer (`width × height` in physical pixels).
pub fn composite_brush_onto_layer(raster: &RasterizedBrushStroke, layer: &mut [[f32; 4]], width: u32, mut touch: impl FnMut(usize)) {
    let stride = WORLD_BRUSH_TILE as usize;
    let w = width as usize;
    let direct_premul = raster.is_direct_premul();
    for tile in &raster.tiles {
        for ly in 0..tile.height as usize {
            let py = tile.y0 as usize + ly;
            let row_base = py.saturating_mul(w);
            for lx in 0..tile.width as usize {
                let ti = ly * stride + lx;
                let idx = row_base + tile.x0 as usize + lx;
                if direct_premul {
                    let Some(&src) = tile.pixels.get(ti) else { continue };
                    if src[3] <= 0.0 {
                        continue;
                    }
                    touch(idx);
                    let Some(dst) = layer.get_mut(idx) else { continue };
                    if raster.erase {
                        let keep = (1.0 - src[3]).clamp(0.0, 1.0);
                        for ch in dst.iter_mut() {
                            *ch *= keep;
                        }
                    } else if raster.behind {
                        let keep = (1.0 - dst[3]).clamp(0.0, 1.0);
                        for ch in 0..4 {
                            dst[ch] = (dst[ch] + src[ch] * keep).clamp(0.0, 1.0);
                        }
                    } else {
                        let keep = (1.0 - src[3]).clamp(0.0, 1.0);
                        for ch in 0..4 {
                            dst[ch] = (src[ch] + dst[ch] * keep).clamp(0.0, 1.0);
                        }
                    }
                } else {
                    let k = tile.mask.get(ti).copied().unwrap_or(0.0);
                    if k <= 0.0 {
                        continue;
                    }
                    let Some(&src_straight) = tile.pixels.get(ti) else { continue };
                    touch(idx);
                    let Some(dst) = layer.get_mut(idx) else { continue };
                    let dst_straight = if dst[3] > 1e-6 {
                        [(dst[0] / dst[3]).clamp(0.0, 1.0), (dst[1] / dst[3]).clamp(0.0, 1.0), (dst[2] / dst[3]).clamp(0.0, 1.0), dst[3].clamp(0.0, 1.0)]
                    } else {
                        [0.0; 4]
                    };
                    let out = photocraft_color::blend::composite(raster.mode, dst_straight, src_straight, k);
                    let a = out[3].clamp(0.0, 1.0);
                    *dst = [(out[0] * a).clamp(0.0, 1.0), (out[1] * a).clamp(0.0, 1.0), (out[2] * a).clamp(0.0, 1.0), a];
                }
            }
        }
    }
}

/// Incremental live brush renderer for the active local shared-canvas draft stroke.
/// Feeds only newly appended pointer samples or stationary hold ticks on each frame,
/// avoiding full-stroke re-rasterization during interactive drawing.
pub struct LiveWorldBrush {
    view: Viewport,
    density: f32,
    p0: [f64; 2],
    brush_per_local: f64,
    generator: DabGenerator,
    accum: Accumulator,
    fed_points: usize,
    work: u64,
}

impl LiveWorldBrush {
    pub fn new(stroke: &Stroke, view: &Viewport, density: f32) -> Result<Self> {
        let brush = stroke.brush.as_deref().ok_or_else(|| err("LiveWorldBrush requires StrokeBrush"))?;
        let p0 = *stroke.points.first().ok_or_else(|| err("LiveWorldBrush requires at least one point"))?;
        let xf = BrushTransform::new(stroke, brush, view, density)?;
        let assets = prepare_assets(brush)?;
        let settings = to_paint_settings(stroke, brush)?;
        let smoothing_zoom = brush.smoothing.as_ref().map_or(1.0, |sm| sm.zoom);
        let generator = DabGenerator::new(&settings, smoothing_zoom);
        let accum = Accumulator::new(stroke, brush, assets, xf);
        let mut live =
            Self { view: view.clone(), density, p0, brush_per_local: f64::from(brush.authored_size) / stroke.width, generator, accum, fed_points: 0, work: 0 };
        live.sync_points(&stroke.points, stroke.samples.as_deref())?;
        Ok(live)
    }

    pub fn matches_view(&self, view: &Viewport, density: f32) -> bool {
        self.view == *view && self.density.to_bits() == density.to_bits()
    }

    pub fn wants_time(&self) -> bool {
        self.generator.wants_time()
    }

    /// Logical screen coordinates `(brush_pos, pointer_pos)` of the smoothing string while lagging.
    pub fn smoothing_string(&self) -> Option<([f64; 2], [f64; 2])> {
        let (a, b) = self.generator.smoothing_string()?;
        Some((self.accum.xf.brush_to_logical(a[0], a[1], &self.view), self.accum.xf.brush_to_logical(b[0], b[1], &self.view)))
    }

    pub fn sync_points(&mut self, points: &[[f64; 2]], samples: Option<&[StrokeSample]>) -> Result<()> {
        if points.len() <= self.fed_points {
            return Ok(());
        }
        let mut new_pts = Vec::with_capacity(points.len() - self.fed_points);
        for idx in self.fed_points..points.len() {
            let Some(&p) = points.get(idx) else { break };
            let sample = samples.and_then(|s| s.get(idx)).copied().unwrap_or_default();
            new_pts.push(StrokePoint {
                x: (p[0] - self.p0[0]) * self.brush_per_local,
                y: (p[1] - self.p0[1]) * self.brush_per_local,
                pressure: sample.pressure,
                tilt_x: sample.tilt_x,
                tilt_y: sample.tilt_y,
                rotation: sample.rotation,
                wheel: sample.wheel,
                time: sample.time_ms,
            });
        }
        self.fed_points = points.len();
        let mut dabs = Vec::new();
        let mut duals = Vec::new();
        self.generator.push(&new_pts, &mut dabs, &mut duals);
        for d in &dabs {
            self.accum.accumulate_dab(d, false, &mut self.work, MAX_BRUSH_WORK_SAMPLES)?;
        }
        for d in &duals {
            self.accum.accumulate_dab(d, true, &mut self.work, MAX_BRUSH_WORK_SAMPLES)?;
        }
        Ok(())
    }

    /// Produce the finalized live preview, including any pending first dab or smoothing catch-up tail,
    /// without mutating the live generator state.
    pub fn preview_raster(&self) -> Result<Option<RasterizedBrushStroke>> {
        let mut tail_gen = self.generator.clone();
        let mut tail_dabs = Vec::new();
        let mut tail_duals = Vec::new();
        tail_gen.finish(&mut tail_dabs, &mut tail_duals);
        let result = if tail_dabs.is_empty() && tail_duals.is_empty() {
            self.accum.finalize()
        } else {
            let mut copy = Accumulator {
                brush: self.accum.brush.clone(),
                color: self.accum.color,
                erase: self.accum.erase,
                assets: Arc::clone(&self.accum.assets),
                xf: self.accum.xf,
                per_dab_color: self.accum.per_dab_color,
                tiles: self.accum.tiles.clone(),
                dabs_done: self.accum.dabs_done,
            };
            let mut work = self.work;
            for d in &tail_dabs {
                copy.accumulate_dab(d, false, &mut work, MAX_BRUSH_WORK_SAMPLES)?;
            }
            for d in &tail_duals {
                copy.accumulate_dab(d, true, &mut work, MAX_BRUSH_WORK_SAMPLES)?;
            }
            copy.finalize()
        };
        Ok((!result.is_empty()).then_some(result))
    }
}
