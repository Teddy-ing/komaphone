//! Single-source-of-truth multi-scale brush converter and viewport-clipped rasterizer
//! for shared canvases (`World`).
//!
//! Both CPU exports/thumbnails (`world_media::WorldRenderer`) and the native GPU
//! compositor (`komaphone_world_gpu::WorldGpu`) rasterize `StrokeBrush` strokes through
//! this module, guaranteeing pixel-identical opacity, flow, overlap, tip shape, dynamics,
//! texture, dual brush, wet edges, smoothing, and deep-zoom behavior across live previews,
//! peer previews, accepted art, reopen, catch-up playback, tours, PNG exports, and thumbnails.

#[cfg(not(target_arch = "wasm32"))]
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

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
    /// Exact 106-bit Euclidean remainder modulo a positive `period`, preserving
    /// arbitrary pattern tiling periods across all 12 deep-zoom depths.
    fn rem_euclid_period(self, period: Self) -> f64 {
        let p = period.value();
        if !p.is_finite() || p <= 0.0 {
            return 0.0;
        }
        let q_d = self.div(period);
        let q_hi = q_d.hi.floor();
        let rem1 = self.sub(period.mul(Self::scalar(q_hi)));
        let q_lo = rem1.div(period).value().floor();
        let mut r = rem1.sub(period.mul(Self::scalar(q_lo))).value();
        if r < 0.0 {
            r += p;
        }
        if r >= p {
            r -= p;
        }
        if r.is_finite() { r.clamp(0.0, p) } else { 0.0 }
    }
    /// Split into an `i32`-wrapped integer grid cell and fractional offset in `[0, 1)`.
    /// Bit-identical to `(x.floor() as i32, x - x.floor())` across the entire `i32` range,
    /// while preventing `as i32` saturation collapse at deep ancestor zoom.
    fn wrap_i32_cell(self) -> (i32, f64) {
        const TWO_POW_32: f64 = 4_294_967_296.0;
        let r = self.rem_euclid_period(Self::scalar(TWO_POW_32));
        let fl = r.floor();
        let cell = (fl as u32) as i32;
        (cell, r - fl)
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
    let mut settings = to_generator_settings(stroke, brush);
    settings.tip = tip_to_paint(&brush.tip)?;
    if let Some(tx) = &brush.texture {
        settings.texture.pattern = pattern_to_paint(&tx.pattern)?;
    }
    if let Some(dual) = &brush.dual_brush {
        settings.dual_brush.tip = tip_to_paint(&dual.tip)?;
    }
    Ok(settings)
}

/// Lightweight `BrushSettings` converter for `DabGenerator`, which never reads `tip`,
/// `dual_brush.tip`, or `texture.pattern` (those are cached in `PreparedAssets`).
/// Avoids regenerating procedural tips or decoding PNGs on every stroke rasterization.
fn to_generator_settings(stroke: &Stroke, brush: &StrokeBrush) -> BrushSettings {
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
        tip: TipShape::Round,
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
            pattern: Pattern::default(),
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
            tip: TipShape::Round,
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
    settings
}

// ---------------------------------------------------------------------------
// Cached prepared tips & textures
// ---------------------------------------------------------------------------

struct PreparedAssets {
    tip: Option<Arc<Mips>>,
    dual_tip: Option<Arc<Mips>>,
    texture: Option<Arc<PatternImage>>,
}

fn hash_tip(h: &mut blake3::Hasher, tip: &BrushTip) {
    match tip {
        BrushTip::Round => {
            h.update(&[0]);
        }
        BrushTip::Procedural { style, size, seed, param } => {
            h.update(&[1, *style as u8]);
            h.update(&size.to_le_bytes());
            h.update(&seed.to_le_bytes());
            h.update(&param.to_le_bytes());
        }
        BrushTip::Sampled { width, height, png_b64 } => {
            h.update(&[2]);
            h.update(&width.to_le_bytes());
            h.update(&height.to_le_bytes());
            h.update(&(png_b64.len() as u64).to_le_bytes());
            h.update(png_b64.as_bytes());
        }
    }
}

fn hash_pattern(h: &mut blake3::Hasher, pattern: &BrushPattern) {
    match pattern {
        BrushPattern::Procedural { style, size, seed } => {
            h.update(&[1, *style as u8]);
            h.update(&size.to_le_bytes());
            h.update(&seed.to_le_bytes());
        }
        BrushPattern::Tile { width, height, png_b64 } => {
            h.update(&[2]);
            h.update(&width.to_le_bytes());
            h.update(&height.to_le_bytes());
            h.update(&(png_b64.len() as u64).to_le_bytes());
            h.update(png_b64.as_bytes());
        }
    }
}

fn asset_cache_key(brush: &StrokeBrush) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    hash_tip(&mut h, &brush.tip);
    h.update(&[0xff]);
    if let Some(dual) = &brush.dual_brush {
        hash_tip(&mut h, &dual.tip);
    }
    h.update(&[0xfe]);
    if let Some(tx) = &brush.texture {
        hash_pattern(&mut h, &tx.pattern);
        h.update(&[u8::from(tx.invert)]);
        h.update(&tx.brightness.to_bits().to_le_bytes());
        h.update(&tx.contrast.to_bits().to_le_bytes());
    }
    *h.finalize().as_bytes()
}

type AssetCacheEntry = ([u8; 32], Arc<PreparedAssets>);

fn prepare_assets(brush: &StrokeBrush) -> Result<Arc<PreparedAssets>> {
    if matches!(brush.tip, BrushTip::Round) && brush.dual_brush.is_none() && brush.texture.is_none() {
        static EMPTY: OnceLock<Arc<PreparedAssets>> = OnceLock::new();
        return Ok(Arc::clone(EMPTY.get_or_init(|| Arc::new(PreparedAssets { tip: None, dual_tip: None, texture: None }))));
    }
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
    let wf = pattern.width as f64;
    let hf = pattern.height as f64;
    if pattern.width == 0 || pattern.height == 0 {
        return 1.0;
    }
    let fx = (bx / s - 0.5).rem_euclid(wf);
    let fy = (by / s - 0.5).rem_euclid(hf);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
    let w = pattern.width;
    let h = pattern.height;
    let ux0 = (x0 as usize) % w;
    let uy0 = (y0 as usize) % h;
    let ux1 = if ux0 + 1 == w { 0 } else { ux0 + 1 };
    let uy1 = if uy0 + 1 == h { 0 } else { uy0 + 1 };
    let row0 = uy0 * w;
    let row1 = uy1 * w;
    let p00 = pattern.data.get(row0 + ux0).copied().unwrap_or(1.0);
    let p10 = pattern.data.get(row0 + ux1).copied().unwrap_or(1.0);
    let p01 = pattern.data.get(row1 + ux0).copied().unwrap_or(1.0);
    let p11 = pattern.data.get(row1 + ux1).copied().unwrap_or(1.0);
    let a = p00 + (p10 - p00) * tx;
    let b = p01 + (p11 - p01) * tx;
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
    /// Exact 106-bit region-aligned brush-space coordinate at the viewport center
    /// (`cam_bx + p0 * brush_per_local`), preserving each pattern's exact native period
    /// and noise/dissolve grid alignment across all 12 depths without arbitrary wrapping.
    tex_base_bx: Double,
    tex_base_by: Double,
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
        let tex_base_bx = cam_bx.add(Double::scalar(p0[0]).mul(Double::scalar(brush_per_local)));
        let tex_base_by = cam_by.add(Double::scalar(p0[1]).mul(Double::scalar(brush_per_local)));
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
            tex_base_bx,
            tex_base_by,
        })
    }

    /// Exact 106-bit transform mapping canonical brush-space onto a `260×260`
    /// (`TILE_PHYSICAL`) world-space LOD tile (`[-2, 258) × [-2, 258)`).
    fn for_world_tile(stroke: &Stroke, brush: &StrokeBrush, tile: photocraft_scene::spatial::TileKey) -> Result<Option<Self>> {
        use photocraft_scene::spatial::{TILE_PHYSICAL, pixel_ticks};
        let p0 = *stroke.points.first().ok_or_else(|| err("Brush stroke has no points"))?;
        let p0_world = coordinates::point(&stroke.anchor, p0).map_err(|e| err(e.to_string()))?;
        let anchor_unit = coordinates::local_unit(stroke.anchor.depth).map_err(|e| err(e.to_string()))?;
        let pt = pixel_ticks(tile.lod);
        let center_ticks = tile.center_ticks();
        let brush_per_local = f64::from(brush.authored_size) / stroke.width;
        let local_per_brush = stroke.width / f64::from(brush.authored_size);
        let phys_per_brush_d = Double::integer(anchor_unit).div(Double::integer(pt)).mul(Double::scalar(local_per_brush));
        let phys_per_brush = phys_per_brush_d.value();
        if !phys_per_brush.is_finite() || phys_per_brush <= 1e-12 {
            return Ok(None);
        }
        let brush_per_phys = 1.0 / phys_per_brush;
        if !brush_per_phys.is_finite() {
            return Ok(None);
        }
        let inv_unit = Double::scalar(brush_per_local).div(Double::integer(anchor_unit));
        let cam_bx = Double::integer(center_ticks[0] - p0_world.x.0).mul(inv_unit);
        let cam_by = Double::integer(center_ticks[1] - p0_world.y.0).mul(inv_unit);
        let tex_base_bx = cam_bx.add(Double::scalar(p0[0]).mul(Double::scalar(brush_per_local)));
        let tex_base_by = cam_by.add(Double::scalar(p0[1]).mul(Double::scalar(brush_per_local)));
        Ok(Some(Self {
            phys_w: TILE_PHYSICAL,
            phys_h: TILE_PHYSICAL,
            cam_bx,
            cam_by,
            phys_per_brush_x: phys_per_brush,
            phys_per_brush_y: phys_per_brush,
            brush_per_phys_x: brush_per_phys,
            brush_per_phys_y: brush_per_phys,
            phys_per_brush: phys_per_brush as f32,
            tex_base_bx,
            tex_base_by,
        }))
    }

    /// Exact pattern-period-wrapped brush coordinates `(bx0, by0)` at tile pixel `(x0, y0)`.
    fn tile_tex_origin(&self, x0: u32, y0: u32, pat: &PatternImage, scale: f32) -> (f64, f64) {
        let half_w = f64::from(self.phys_w) * 0.5;
        let half_h = f64::from(self.phys_h) * 0.5;
        let s = Double::scalar(f64::from(scale.max(0.01)));
        let period_x = Double::scalar(pat.width as f64).mul(s);
        let period_y = Double::scalar(pat.height as f64).mul(s);
        let bx = self.tex_base_bx.add(Double::scalar((f64::from(x0) + 0.5 - half_w) * self.brush_per_phys_x));
        let by = self.tex_base_by.add(Double::scalar((f64::from(y0) + 0.5 - half_h) * self.brush_per_phys_y));
        (bx.rem_euclid_period(period_x), by.rem_euclid_period(period_y))
    }

    /// Exact `i32`-wrapped hash cell origin `(hx0, fx0, hy0, fy0)` at tile pixel `(x0, y0)`.
    fn tile_hash_origin(&self, x0: u32, y0: u32) -> (i32, f64, i32, f64) {
        let half_w = f64::from(self.phys_w) * 0.5;
        let half_h = f64::from(self.phys_h) * 0.5;
        let bx = self.tex_base_bx.add(Double::scalar((f64::from(x0) + 0.5 - half_w) * self.brush_per_phys_x));
        let by = self.tex_base_by.add(Double::scalar((f64::from(y0) + 0.5 - half_h) * self.brush_per_phys_y));
        let (hx0, fx0) = bx.wrap_i32_cell();
        let (hy0, fy0) = by.wrap_i32_cell();
        (hx0, fx0, hy0, fy0)
    }

    #[inline]
    fn hash_cell_x(&self, base_cell: i32, base_frac: f64, step: u32, abs_px: u32) -> i32 {
        let off = base_frac + f64::from(step) * self.brush_per_phys_x;
        if off.abs() < 2_147_483_648.0 {
            base_cell.wrapping_add(off.floor() as i64 as i32)
        } else {
            let half_w = f64::from(self.phys_w) * 0.5;
            self.tex_base_bx.add(Double::scalar((f64::from(abs_px) + 0.5 - half_w) * self.brush_per_phys_x)).wrap_i32_cell().0
        }
    }

    #[inline]
    fn hash_cell_y(&self, base_cell: i32, base_frac: f64, step: u32, abs_py: u32) -> i32 {
        let off = base_frac + f64::from(step) * self.brush_per_phys_y;
        if off.abs() < 2_147_483_648.0 {
            base_cell.wrapping_add(off.floor() as i64 as i32)
        } else {
            let half_h = f64::from(self.phys_h) * 0.5;
            self.tex_base_by.add(Double::scalar((f64::from(abs_py) + 0.5 - half_h) * self.brush_per_phys_y)).wrap_i32_cell().0
        }
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
#[cfg(not(target_arch = "wasm32"))]
const PARALLEL_SAMPLE_THRESHOLD: u64 = 131_072;
#[cfg(not(target_arch = "wasm32"))]
const PARALLEL_TILE_THRESHOLD: usize = 4;

#[derive(Clone)]
struct AccumTile {
    cov: Vec<f32>,
    depth: Vec<f32>,
    col: Vec<[f32; 4]>,
    dual: Vec<f32>,
    /// True while every pixel in this 64×64 sub-tile shares identical `(u_cov, u_depth, u_dual, u_col)`.
    /// Eliminates 4,096× per-pixel loops and heap allocations for giant deep-zoom ancestor dabs.
    uniform: bool,
    u_cov: f32,
    u_depth: f32,
    u_dual: f32,
    u_col: [f32; 4],
    /// Exact lower bound on `cov` across all active pixels in this tile.
    /// Used exclusively when `!wet && !dual && !per_dab_color` (strict 0/255 equivalence).
    min_cov: f32,
    touched: bool,
}

impl AccumTile {
    fn new(_wet: bool, _per_dab_color: bool, _has_dual: bool) -> Self {
        Self {
            cov: Vec::new(),
            depth: Vec::new(),
            col: Vec::new(),
            dual: Vec::new(),
            uniform: true,
            u_cov: 0.0,
            u_depth: 0.0,
            u_dual: 0.0,
            u_col: [0.0; 4],
            min_cov: 0.0,
            touched: false,
        }
    }

    #[cfg(test)]
    fn new_dense(wet: bool, per_dab_color: bool, has_dual: bool) -> Self {
        let mut t = Self::new(wet, per_dab_color, has_dual);
        t.materialize_dense(wet, per_dab_color, has_dual);
        t
    }

    fn materialize_dense(&mut self, wet: bool, per_dab_color: bool, has_dual: bool) {
        if !self.uniform {
            return;
        }
        self.cov = vec![self.u_cov; TILE_PIXELS];
        self.depth = if wet { vec![self.u_depth; TILE_PIXELS] } else { Vec::new() };
        self.col = if per_dab_color { vec![self.u_col; TILE_PIXELS] } else { Vec::new() };
        self.dual = if has_dual { vec![self.u_dual; TILE_PIXELS] } else { Vec::new() };
        self.uniform = false;
    }
}

/// One finalized 64×64 viewport tile of a rasterized brush stroke.
#[derive(Clone, Debug)]
pub struct BrushTile {
    pub x0: u32,
    pub y0: u32,
    pub width: u32,
    pub height: u32,
    /// When `Some((px, mask))`, every pixel in `0..height × 0..width` has constant `px` and `mask`,
    /// and `pixels` / `mask` vectors remain empty (zero heap allocation for uniform deep-zoom tiles).
    pub uniform_pixel: Option<([f32; 4], f32)>,
    /// Always `64 * 64` entries in row-major order when `uniform_pixel` is `None`.
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
        let w = (self.width as usize).min(stride);
        let h = (self.height as usize).min(stride);
        if let Some((px_val, _)) = self.uniform_pixel {
            if px_val[3] <= 0.0 {
                return out;
            }
            for (ly, dst_row) in out.chunks_exact_mut(stride * 4).take(h).enumerate() {
                let py = (self.y0 as usize + ly) as i32;
                let dst_chunks = dst_row.chunks_exact_mut(4).take(w);
                if aliased {
                    let a = (px_val[3].clamp(0.0, 1.0) * 255.0).round() as u8;
                    let r = ((px_val[0].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                    let g = ((px_val[1].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                    let b = ((px_val[2].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                    for slot in dst_chunks {
                        slot.copy_from_slice(&[r, g, b, a]);
                    }
                } else {
                    for (lx, slot) in dst_chunks.enumerate() {
                        let px = (self.x0 as usize + lx) as i32;
                        let d = (hash2(px, py, DITHER_SALT) - 0.5) / 255.0;
                        let a = ((px_val[3] + d).clamp(0.0, 1.0) * 255.0).round() as u8;
                        slot[0] = (((px_val[0] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                        slot[1] = (((px_val[1] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                        slot[2] = (((px_val[2] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                        slot[3] = a;
                    }
                }
            }
            return out;
        }
        for (ly, (src_row, dst_row)) in self.pixels.chunks_exact(stride).zip(out.chunks_exact_mut(stride * 4)).take(h).enumerate() {
            let py = (self.y0 as usize + ly) as i32;
            let src_slice = src_row.get(..w).unwrap_or_default();
            let dst_chunks = dst_row.chunks_exact_mut(4).take(w);
            if aliased {
                for (px_val, slot) in src_slice.iter().zip(dst_chunks) {
                    if px_val[3] <= 0.0 {
                        continue;
                    }
                    let a = (px_val[3].clamp(0.0, 1.0) * 255.0).round() as u8;
                    slot[0] = ((px_val[0].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                    slot[1] = ((px_val[1].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                    slot[2] = ((px_val[2].clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                    slot[3] = a;
                }
            } else {
                for (lx, (px_val, slot)) in src_slice.iter().zip(dst_chunks).enumerate() {
                    if px_val[3] <= 0.0 {
                        continue;
                    }
                    let px = (self.x0 as usize + lx) as i32;
                    let d = (hash2(px, py, DITHER_SALT) - 0.5) / 255.0;
                    let a = ((px_val[3] + d).clamp(0.0, 1.0) * 255.0).round() as u8;
                    slot[0] = (((px_val[0] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                    slot[1] = (((px_val[1] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
                    slot[2] = (((px_val[2] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
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

struct StrokeRenderCtx {
    brush: StrokeBrush,
    color: [f32; 4],
    erase: bool,
    assets: Arc<PreparedAssets>,
    xf: BrushTransform,
    per_dab_color: bool,
    wet: bool,
    has_dual: bool,
}

impl StrokeRenderCtx {
    fn new(stroke: &Stroke, brush: &StrokeBrush, assets: Arc<PreparedAssets>, xf: BrushTransform) -> Self {
        Self {
            brush: brush.clone(),
            // Opacity lives exclusively on `brush.opacity`; `stroke.color.rgba[3]` may carry a
            // summary opacity value for metadata/thumbnails and must never multiply opacity twice.
            color: [stroke.color.rgba[0], stroke.color.rgba[1], stroke.color.rgba[2], 1.0],
            erase: stroke.erase,
            assets,
            xf,
            per_dab_color: brush.color_dynamics.is_some() && !stroke.erase,
            wet: brush.wet_edges,
            has_dual: brush.dual_brush.is_some(),
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

    fn prepare_dab(&self, d: &Dab, dual: bool) -> Option<PreparedDab> {
        let b = &self.brush;
        let aliased = b.aliased && !dual;
        let (cx, cy) = if aliased {
            let (gx, gy) = grid_center(d.center.x, d.center.y, 2.0 * d.radius);
            (f64::from(gx), f64::from(gy))
        } else {
            (d.center.x, d.center.y)
        };
        let reach_b = self.dab_reach_brush(d, dual);
        let (x0, y0, x1, y1) = self.xf.clip_dab_rect(cx, cy, reach_b)?;
        let (hardness, has_mips) = if dual {
            (b.dual_brush.as_deref().map_or(1.0, |db| db.hardness), self.assets.dual_tip.is_some())
        } else {
            (b.hardness, self.assets.tip.is_some())
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
        let (rm, subpixel_att) = if !aliased && !has_mips && rm_phys < 0.5 && self.xf.phys_per_brush > 0.0 {
            ((0.5 / self.xf.phys_per_brush).max(rm_true), (rm_phys / 0.5).clamp(0.0, 1.0))
        } else {
            (rm_true, 1.0)
        };
        let profile = ScaledTipProfile::new(rm, hardness, self.xf.phys_per_brush);
        let slack_brush = (0.5 / self.xf.phys_per_brush.max(1e-6)).min(rm.max(0.5));
        let reach = if aliased || has_mips { rm } else { profile.reach.max(rm) };
        let reach2 = (reach + slack_brush) * (reach + slack_brush);
        let small = !has_mips && !aliased && rm_phys < SMALL_TIP_RADIUS && subpixel_att >= 1.0;
        let small_reach2 = (reach + 0.75 * self.xf.brush_per_phys_x.max(self.xf.brush_per_phys_y) as f32).powi(2);
        let aliased_supersample = aliased && self.xf.phys_per_brush < 0.75;

        let mips_ref = if dual { self.assets.dual_tip.as_deref() } else { self.assets.tip.as_deref() };
        let (level, inv_scale, tw, th) = match mips_ref {
            Some(m) => {
                let (w0, h0, _) = m.levels.first().map_or((1, 1, &[][..]), |(w, h, data)| (*w, *h, data.as_slice()));
                let big = w0.max(h0) as f32;
                let phys_diam = (2.0 * r * ro.max(0.25) * self.xf.phys_per_brush).max(1.0);
                (m.level_for(phys_diam), big / (2.0 * r), w0 as f32, h0 as f32)
            }
            None => (0, 1.0, 1.0, 1.0),
        };
        let dab_off_x = self.xf.cam_bx.sub(Double::scalar(cx)).value();
        let dab_off_y = self.xf.cam_by.sub(Double::scalar(cy)).value();
        let fast_round = !dual && !has_mips && !aliased && !small && proj.is_none() && !noise && !tex_tip;
        let a_ux = cs * fx * ro;
        let a_uy = -sn * fx * ro;
        let a_vx = -sn * fy;
        let a_vy = -cs * fy;
        // Conservative squared core radius so float rounding never misclassifies an edge pixel.
        let safe_core = (profile.core - 1e-4 * rm.max(1.0)).max(0.0);
        let core2 = safe_core * safe_core;
        Some(PreparedDab {
            x0,
            y0,
            x1,
            y1,
            dual,
            cx,
            cy,
            dab_off_x,
            dab_off_y,
            alpha: d.alpha,
            opacity: d.opacity,
            color: d.color,
            depth: d.depth,
            wet,
            noise,
            tex_tip,
            tex_scale,
            tex_mode,
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
            level,
            inv_scale,
            tw,
            th,
            fast_round,
            a_ux,
            a_uy,
            a_vx,
            a_vy,
            core2,
        })
    }
}

struct PreparedDab {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    dual: bool,
    cx: f64,
    cy: f64,
    dab_off_x: f64,
    dab_off_y: f64,
    alpha: f32,
    opacity: f32,
    color: [f32; 4],
    depth: f32,
    wet: bool,
    noise: bool,
    tex_tip: bool,
    tex_scale: f32,
    tex_mode: MaskMode,
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
    level: usize,
    inv_scale: f32,
    tw: f32,
    th: f32,
    fast_round: bool,
    a_ux: f32,
    a_uy: f32,
    a_vx: f32,
    a_vy: f32,
    core2: f32,
}

impl PreparedDab {
    fn samples(&self) -> u64 {
        u64::from(self.x1 - self.x0).saturating_mul(u64::from(self.y1 - self.y0))
    }
}

struct DabSampler<'a> {
    pd: &'a PreparedDab,
    dbx: f64,
    dby: f64,
    mips: Option<&'a Mips>,
}

impl DabSampler<'_> {
    #[inline]
    fn to_tip(&self, dx: f32, dy: f32) -> (f32, f32) {
        let (mut ux, mut uy) = (dx, -dy);
        if let Some((ax, ay, k)) = self.pd.proj {
            let t = (ux * ax + uy * ay) * k;
            ux += t * ax;
            uy += t * ay;
        }
        ((ux * self.pd.cs + uy * self.pd.sn) * self.pd.fx, (-ux * self.pd.sn + uy * self.pd.cs) * self.pd.fy)
    }

    fn sample_square(&self, dx_base: f64, dy_base: f64) -> Option<(f32, f32)> {
        if self.pd.aliased_supersample {
            let mut hits = 0u32;
            for sy in SUBPIXEL {
                for sx in SUBPIXEL {
                    let bx = (self.pd.cx + dx_base + sx * self.dbx).floor() + 0.5;
                    let by = (self.pd.cy + dy_base + sy * self.dby).floor() + 0.5;
                    if ((bx - self.pd.cx).abs() as f32) < self.pd.r && ((by - self.pd.cy).abs() as f32) < self.pd.r {
                        hits += 1;
                    }
                }
            }
            Some((hits as f32 / 16.0, 0.0))
        } else {
            let bx = (self.pd.cx + dx_base).floor() + 0.5;
            let by = (self.pd.cy + dy_base).floor() + 0.5;
            (((bx - self.pd.cx).abs() as f32) < self.pd.r && ((by - self.pd.cy).abs() as f32) < self.pd.r).then_some((1.0, 0.0))
        }
    }

    fn sample_aliased_round(&self, dx_base: f64, dy_base: f64) -> Option<(f32, f32)> {
        if self.pd.aliased_supersample {
            let mut hits = 0u32;
            for sy in SUBPIXEL {
                for sx in SUBPIXEL {
                    let bx = (self.pd.cx + dx_base + sx * self.dbx).floor() + 0.5;
                    let by = (self.pd.cy + dy_base + sy * self.dby).floor() + 0.5;
                    let (u, v) = self.to_tip((bx - self.pd.cx) as f32, (by - self.pd.cy) as f32);
                    if ((u * self.pd.ro).powi(2) + v * v).sqrt() <= self.pd.rm {
                        hits += 1;
                    }
                }
            }
            Some((hits as f32 / 16.0, 0.0))
        } else {
            let bx = (self.pd.cx + dx_base).floor() + 0.5;
            let by = (self.pd.cy + dy_base).floor() + 0.5;
            let (u, v) = self.to_tip((bx - self.pd.cx) as f32, (by - self.pd.cy) as f32);
            let d2 = (u * self.pd.ro).powi(2) + v * v;
            if d2 >= self.pd.reach2 {
                return None;
            }
            let dd = d2.sqrt();
            Some((if dd <= self.pd.rm { 1.0 } else { 0.0 }, dd / self.pd.rm))
        }
    }

    fn sample(&self, dx_base: f64, dy_base: f64) -> Option<(f32, f32)> {
        if self.pd.aliased && self.pd.square {
            return self.sample_square(dx_base, dy_base);
        }
        match self.mips {
            None if self.pd.small => {
                let (u0, v0) = self.to_tip(dx_base as f32, dy_base as f32);
                let d2 = (u0 * self.pd.ro).powi(2) + v0 * v0;
                if d2 >= self.pd.small_reach2 {
                    return None;
                }
                let mut sum = 0.0f32;
                for sy in SUBPIXEL {
                    for sx in SUBPIXEL {
                        let (su, sv) = self.to_tip((dx_base + sx * self.dbx) as f32, (dy_base + sy * self.dby) as f32);
                        let sd = ((su * self.pd.ro).powi(2) + sv * sv).sqrt();
                        sum += self.pd.profile.falloff(sd);
                    }
                }
                Some((sum / 16.0, d2.sqrt() / self.pd.rm))
            }
            None if self.pd.aliased => self.sample_aliased_round(dx_base, dy_base),
            None => {
                let (u, v) = self.to_tip(dx_base as f32, dy_base as f32);
                let d2 = (u * self.pd.ro).powi(2) + v * v;
                if d2 >= self.pd.reach2 {
                    return None;
                }
                let dd = d2.sqrt();
                Some((self.pd.profile.coverage(dd, self.pd.slack_brush) * self.pd.subpixel_att, dd / self.pd.rm))
            }
            Some(m) => {
                let (dx_s, dy_s) = if self.pd.aliased {
                    let bx = (self.pd.cx + dx_base).floor() + 0.5;
                    let by = (self.pd.cy + dy_base).floor() + 0.5;
                    ((bx - self.pd.cx) as f32, (by - self.pd.cy) as f32)
                } else {
                    (dx_base as f32, dy_base as f32)
                };
                let (u, v) = self.to_tip(dx_s, dy_s);
                let (txp, typ) = (u * self.pd.inv_scale + self.pd.tw * 0.5, -(v / self.pd.ro) * self.pd.inv_scale + self.pd.th * 0.5);
                if txp < -1.0 || typ < -1.0 || txp > self.pd.tw + 1.0 || typ > self.pd.th + 1.0 {
                    return None;
                }
                let s = m.sample_clamped(self.pd.level, txp / self.pd.tw, typ / self.pd.th);
                let val = if self.pd.aliased { if s >= 0.5 { 1.0 } else { 0.0 } } else { s };
                Some((val, 1.0 - s))
            }
        }
    }
}

#[inline]
fn deposit_dab_pixel(tile: &mut AccumTile, ti: usize, pd: &PreparedDab, per_dab_color: bool, v: f32, rn: f32) {
    if pd.dual {
        if let Some(slot) = tile.dual.get_mut(ti) {
            let c = *slot;
            if 1.0 > c {
                *slot = c + v * (1.0 - c);
                tile.touched = true;
            }
        }
        return;
    }
    if pd.wet
        && let Some(slot) = tile.depth.get_mut(ti)
    {
        *slot = slot.max((1.0 - rn).clamp(0.0, 1.0));
    }
    if let Some(cov_slot) = tile.cov.get_mut(ti) {
        let c = *cov_slot;
        let nv = if pd.opacity > c { c + v * (pd.opacity - c) } else { c };
        if nv > c {
            if per_dab_color {
                let k = if c < 1.0 { (nv - c) / (1.0 - c) } else { 0.0 };
                if let Some(col_slot) = tile.col.get_mut(ti) {
                    for (dst, &dc) in col_slot.iter_mut().zip(&pd.color) {
                        *dst = dc * k + *dst * (1.0 - k);
                    }
                }
            }
            *cov_slot = nv;
            tile.touched = true;
        }
    }
}

fn tile_active_min_cov(tile: &AccumTile, tile_w: usize, tile_h: usize) -> f32 {
    let stride = WORLD_BRUSH_TILE as usize;
    let mut m = f32::INFINITY;
    for row in tile.cov.chunks_exact(stride).take(tile_h) {
        for &c in row.get(..tile_w).unwrap_or_default() {
            if c < m {
                m = c;
            }
        }
    }
    if m.is_finite() { m } else { 0.0 }
}

struct TileSpan {
    tile_y0: u32,
    lx0: usize,
    lx1: usize,
    ly0: usize,
    ly1: usize,
    dx0: f64,
    full_tile: bool,
}

fn fill_core_tile(tile: &mut AccumTile, span: &TileSpan, opacity: f32, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let stride = WORLD_BRUSH_TILE as usize;
    let mut wrote = false;
    if alpha >= 1.0 {
        for ly in span.ly0..span.ly1 {
            let row = tile.cov.get_mut(ly * stride + span.lx0..ly * stride + span.lx1).unwrap_or_default();
            for c in row {
                if opacity > *c {
                    *c = opacity;
                    wrote = true;
                }
            }
        }
        if wrote {
            tile.touched = true;
        }
        if span.full_tile && opacity > tile.min_cov {
            tile.min_cov = opacity;
        }
        return;
    }
    let mut local_min = f32::INFINITY;
    for ly in span.ly0..span.ly1 {
        let row = tile.cov.get_mut(ly * stride + span.lx0..ly * stride + span.lx1).unwrap_or_default();
        for c in row {
            if opacity > *c {
                *c += alpha * (opacity - *c);
                wrote = true;
            }
            if *c < local_min {
                local_min = *c;
            }
        }
    }
    if wrote {
        tile.touched = true;
    }
    if span.full_tile && local_min.is_finite() && local_min > tile.min_cov {
        tile.min_cov = local_min;
    }
}

fn accum_round_early_out(tile: &mut AccumTile, ctx: &StrokeRenderCtx, span: &TileSpan, pd: &PreparedDab) {
    let half_h = f64::from(ctx.xf.phys_h) * 0.5;
    let dby = ctx.xf.brush_per_phys_y;
    let du_dx = ctx.xf.brush_per_phys_x as f32 * pd.a_ux;
    let dv_dx = ctx.xf.brush_per_phys_x as f32 * pd.a_vx;
    let stride = WORLD_BRUSH_TILE as usize;
    let dx0_f = span.dx0 as f32;

    // Pure soft-round (`core == 0.0`, e.g. Airbrush Soft / Soft Round):
    // process 4-wide chunks with ZERO inner-loop branches and ZERO float reductions
    // so LLVM emits packed SSE2/AVX2 (`sqrtps`/`divps`/`mulps`) while preserving
    // exact IEEE-754 operation order identical to `ScaledTipProfile::coverage`.
    if pd.profile.core == 0.0 {
        if pd.alpha <= 0.0 || pd.subpixel_att <= 0.0 || pd.opacity <= 0.0 {
            return;
        }
        let shoulder = pd.profile.shoulder;
        let reach_limit = pd.profile.reach + pd.slack_brush;
        let sub_att = pd.subpixel_att;
        let alpha = pd.alpha;
        let opacity = pd.opacity;
        for ly in span.ly0..span.ly1 {
            let py = span.tile_y0 + ly as u32;
            let dy_f = (pd.dab_off_y + (f64::from(py) + 0.5 - half_h) * dby) as f32;
            let u_row = dx0_f * pd.a_ux + dy_f * pd.a_uy;
            let v_row = dx0_f * pd.a_vx + dy_f * pd.a_vy;
            let row = tile.cov.get_mut(ly * stride + span.lx0..ly * stride + span.lx1).unwrap_or_default();
            let mut chunks = row.chunks_exact_mut(4);
            let mut base_i = 0_u32;
            for chunk in &mut chunks {
                let mut us = [0.0_f32; 4];
                let mut vs = [0.0_f32; 4];
                for lane in 0..4 {
                    let lf = (base_i + lane as u32) as f32;
                    us[lane] = u_row + lf * du_dx;
                    vs[lane] = v_row + lf * dv_dx;
                }
                for lane in 0..4 {
                    let dd = (us[lane] * us[lane] + vs[lane] * vs[lane]).sqrt();
                    let mask = if dd < reach_limit { 1.0_f32 } else { 0.0_f32 };
                    let x = dd / shoulder;
                    let vv = ((soft_exp(x * x) * sub_att) * alpha) * mask;
                    let c = chunk[lane];
                    let gap = (opacity - c).max(0.0);
                    chunk[lane] = c + vv * gap;
                }
                base_i += 4;
            }
            for (k, cov_slot) in chunks.into_remainder().iter_mut().enumerate() {
                let lf = (base_i + k as u32) as f32;
                let u = u_row + lf * du_dx;
                let v = v_row + lf * dv_dx;
                let dd = (u * u + v * v).sqrt();
                let mask = if dd < reach_limit { 1.0_f32 } else { 0.0_f32 };
                let x = dd / shoulder;
                let vv = ((soft_exp(x * x) * sub_att) * alpha) * mask;
                let c = *cov_slot;
                let gap = (opacity - c).max(0.0);
                *cov_slot = c + vv * gap;
            }
        }
        tile.touched = true;
        return;
    }

    let mut wrote = false;
    let mut local_min = f32::INFINITY;
    for ly in span.ly0..span.ly1 {
        let py = span.tile_y0 + ly as u32;
        let dy_f = (pd.dab_off_y + (f64::from(py) + 0.5 - half_h) * dby) as f32;
        let u_row = dx0_f * pd.a_ux + dy_f * pd.a_uy;
        let v_row = dx0_f * pd.a_vx + dy_f * pd.a_vy;
        let row = tile.cov.get_mut(ly * stride + span.lx0..ly * stride + span.lx1).unwrap_or_default();
        for (i, cov_slot) in row.iter_mut().enumerate() {
            let c = *cov_slot;
            if c >= pd.opacity {
                if span.full_tile && c < local_min {
                    local_min = c;
                }
                continue;
            }
            let lf = i as f32;
            let u = u_row + lf * du_dx;
            let v = v_row + lf * dv_dx;
            let d2 = u * u + v * v;
            if d2 < pd.reach2 {
                let vv = pd.profile.coverage(d2.sqrt(), pd.slack_brush) * pd.subpixel_att * pd.alpha;
                if vv > 0.0 {
                    let nv = c + vv * (pd.opacity - c);
                    *cov_slot = nv;
                    wrote = true;
                    if span.full_tile && nv < local_min {
                        local_min = nv;
                    }
                    continue;
                }
            }
            if span.full_tile && c < local_min {
                local_min = c;
            }
        }
    }
    if wrote {
        tile.touched = true;
    }
    if span.full_tile && local_min.is_finite() && local_min > tile.min_cov {
        tile.min_cov = local_min;
    }
}

fn accum_round_wet_or_color(tile: &mut AccumTile, ctx: &StrokeRenderCtx, span: &TileSpan, pd: &PreparedDab) {
    let half_h = f64::from(ctx.xf.phys_h) * 0.5;
    let dby = ctx.xf.brush_per_phys_y;
    let du_dx = ctx.xf.brush_per_phys_x as f32 * pd.a_ux;
    let dv_dx = ctx.xf.brush_per_phys_x as f32 * pd.a_vx;
    let stride = WORLD_BRUSH_TILE as usize;
    let inv_rm = 1.0 / pd.rm;
    let dx0_f = span.dx0 as f32;
    if pd.wet && !ctx.per_dab_color {
        let mut wrote = false;
        for ly in span.ly0..span.ly1 {
            let py = span.tile_y0 + ly as u32;
            let dy_f = (pd.dab_off_y + (f64::from(py) + 0.5 - half_h) * dby) as f32;
            let u_row = dx0_f * pd.a_ux + dy_f * pd.a_uy;
            let v_row = dx0_f * pd.a_vx + dy_f * pd.a_vy;
            let cov_row = tile.cov.get_mut(ly * stride + span.lx0..ly * stride + span.lx1).unwrap_or_default();
            let depth_row = tile.depth.get_mut(ly * stride + span.lx0..ly * stride + span.lx1).unwrap_or_default();
            for (i, (cov_slot, depth_slot)) in cov_row.iter_mut().zip(depth_row.iter_mut()).enumerate() {
                let lf = i as f32;
                let u = u_row + lf * du_dx;
                let v = v_row + lf * dv_dx;
                let d2 = u * u + v * v;
                if d2 >= pd.reach2 {
                    continue;
                }
                let dd = d2.sqrt();
                let vv = pd.profile.coverage(dd, pd.slack_brush) * pd.subpixel_att * pd.alpha;
                if vv <= 0.0 {
                    continue;
                }
                *depth_slot = depth_slot.max((1.0 - dd * inv_rm).clamp(0.0, 1.0));
                let c = *cov_slot;
                if pd.opacity > c {
                    *cov_slot = c + vv * (pd.opacity - c);
                    wrote = true;
                }
            }
        }
        if wrote {
            tile.touched = true;
        }
        return;
    }
    for ly in span.ly0..span.ly1 {
        let py = span.tile_y0 + ly as u32;
        let dy_f = (pd.dab_off_y + (f64::from(py) + 0.5 - half_h) * dby) as f32;
        let u_row = dx0_f * pd.a_ux + dy_f * pd.a_uy;
        let v_row = dx0_f * pd.a_vx + dy_f * pd.a_vy;
        let row_base = ly * stride + span.lx0;
        for i in 0..(span.lx1 - span.lx0) {
            let lf = i as f32;
            let u = u_row + lf * du_dx;
            let v = v_row + lf * dv_dx;
            let d2 = u * u + v * v;
            if d2 >= pd.reach2 {
                continue;
            }
            let dd = d2.sqrt();
            let vv = pd.profile.coverage(dd, pd.slack_brush) * pd.subpixel_att * pd.alpha;
            if vv <= 0.0 {
                continue;
            }
            deposit_dab_pixel(tile, row_base + i, pd, ctx.per_dab_color, vv, dd * inv_rm);
        }
    }
}

/// Equivalence-preserving fast Round accumulation on a single 64×64 tile:
/// uses isotropic circle-AABB cull, exact 4-corner convex interior `core` fill,
/// and strict `0/255`-equivalent saturation early-out (`!wet && !per_dab_color`).
fn accumulate_fast_round_tile(tile: &mut AccumTile, ctx: &StrokeRenderCtx, tx: u32, ty: u32, pd: &PreparedDab) {
    let tile_x0 = tx * WORLD_BRUSH_TILE;
    let tile_y0 = ty * WORLD_BRUSH_TILE;
    let tile_w = ctx.xf.phys_w.saturating_sub(tile_x0).min(WORLD_BRUSH_TILE);
    let tile_h = ctx.xf.phys_h.saturating_sub(tile_y0).min(WORLD_BRUSH_TILE);
    let px0 = tile_x0.max(pd.x0);
    let px1 = (tile_x0 + tile_w).min(pd.x1);
    let py0 = tile_y0.max(pd.y0);
    let py1 = (tile_y0 + tile_h).min(pd.y1);
    if px0 >= px1 || py0 >= py1 {
        return;
    }
    let can_early_out = !pd.wet && !ctx.per_dab_color;
    if can_early_out && tile.min_cov >= pd.opacity {
        return;
    }

    let half_w = f64::from(ctx.xf.phys_w) * 0.5;
    let half_h = f64::from(ctx.xf.phys_h) * 0.5;
    let dx0 = pd.dab_off_x + (f64::from(px0) + 0.5 - half_w) * ctx.xf.brush_per_phys_x;
    let dx1 = pd.dab_off_x + (f64::from(px1 - 1) + 0.5 - half_w) * ctx.xf.brush_per_phys_x;
    let dy0 = pd.dab_off_y + (f64::from(py0) + 0.5 - half_h) * ctx.xf.brush_per_phys_y;
    let dy1 = pd.dab_off_y + (f64::from(py1 - 1) + 0.5 - half_h) * ctx.xf.brush_per_phys_y;

    if pd.ro >= 1.0 - 1e-6 {
        let near_x = if dx0 > 0.0 {
            dx0
        } else if dx1 < 0.0 {
            dx1
        } else {
            0.0
        };
        let near_y = if dy0 > 0.0 {
            dy0
        } else if dy1 < 0.0 {
            dy1
        } else {
            0.0
        };
        if (near_x * near_x + near_y * near_y) as f32 >= pd.reach2 + 1e-3 * pd.reach2.max(1.0) {
            return;
        }
    }

    let span = TileSpan {
        tile_y0,
        lx0: (px0 - tile_x0) as usize,
        lx1: (px1 - tile_x0) as usize,
        ly0: (py0 - tile_y0) as usize,
        ly1: (py1 - tile_y0) as usize,
        dx0,
        full_tile: px0 == tile_x0 && px1 == tile_x0 + tile_w && py0 == tile_y0 && py1 == tile_y0 + tile_h,
    };

    if can_early_out && pd.subpixel_att >= 1.0 && pd.core2 > 0.0 {
        let corner_d2 = |dx: f64, dy: f64| -> f32 {
            let u = dx as f32 * pd.a_ux + dy as f32 * pd.a_uy;
            let v = dx as f32 * pd.a_vx + dy as f32 * pd.a_vy;
            u * u + v * v
        };
        let max_d2 = corner_d2(dx0, dy0).max(corner_d2(dx1, dy0)).max(corner_d2(dx0, dy1)).max(corner_d2(dx1, dy1));
        if max_d2 <= pd.core2 {
            fill_core_tile(tile, &span, pd.opacity, pd.alpha);
            return;
        }
    }

    if can_early_out {
        accum_round_early_out(tile, ctx, &span, pd);
    } else {
        accum_round_wet_or_color(tile, ctx, &span, pd);
    }
}

/// Evaluate whether `pd` is mathematically constant across the entire `tile_w × tile_h` sub-tile
/// and, if so, apply it directly to `tile`'s scalar uniform state in O(1) (eliminating 4,096×
/// per-pixel work for giant deep-zoom ancestor strokes while keeping hard AA rims bit-exact).
fn try_accumulate_uniform_dab(tile: &mut AccumTile, ctx: &StrokeRenderCtx, tx: u32, ty: u32, pd: &PreparedDab) -> bool {
    if !tile.uniform || pd.small || pd.aliased_supersample {
        return false;
    }
    let tile_x0 = tx * WORLD_BRUSH_TILE;
    let tile_y0 = ty * WORLD_BRUSH_TILE;
    let tile_w = ctx.xf.phys_w.saturating_sub(tile_x0).min(WORLD_BRUSH_TILE);
    let tile_h = ctx.xf.phys_h.saturating_sub(tile_y0).min(WORLD_BRUSH_TILE);
    if tile_w == 0 || tile_h == 0 {
        return true;
    }
    // Must cover the entire active sub-tile (or miss it completely).
    let px0 = tile_x0.max(pd.x0);
    let px1 = (tile_x0 + tile_w).min(pd.x1);
    let py0 = tile_y0.max(pd.y0);
    let py1 = (tile_y0 + tile_h).min(pd.y1);
    if px0 >= px1 || py0 >= py1 {
        return true;
    }
    if px0 != tile_x0 || px1 != tile_x0 + tile_w || py0 != tile_y0 || py1 != tile_y0 + tile_h {
        return false;
    }
    let can_early_out = !pd.dual && !pd.wet && !ctx.per_dab_color;
    if can_early_out && tile.min_cov >= pd.opacity {
        return true;
    }

    let dbx = ctx.xf.brush_per_phys_x;
    let dby = ctx.xf.brush_per_phys_y;
    let span_b = (f64::from(tile_w) * dbx).max(f64::from(tile_h) * dby);
    let rm = f64::from(pd.rm.max(0.5));
    if span_b > rm * 1e-5 {
        return false;
    }
    if pd.tex_tip && span_b > f64::from(pd.tex_scale.max(0.01)) * 1e-5 {
        return false;
    }

    let hash_origin = (pd.noise || pd.aliased).then(|| ctx.xf.tile_hash_origin(tile_x0, tile_y0));
    if let Some((hx0, fx0, hy0, fy0)) = hash_origin {
        let hx1 = ctx.xf.hash_cell_x(hx0, fx0, tile_w - 1, tile_x0 + tile_w - 1);
        let hy1 = ctx.xf.hash_cell_y(hy0, fy0, tile_h - 1, tile_y0 + tile_h - 1);
        if hx0 != hx1 || hy0 != hy1 {
            return false;
        }
    }

    let half_w = f64::from(ctx.xf.phys_w) * 0.5;
    let half_h = f64::from(ctx.xf.phys_h) * 0.5;
    let dx0 = pd.dab_off_x + (f64::from(tile_x0) + 0.5 - half_w) * dbx;
    let dx1 = pd.dab_off_x + (f64::from(tile_x0 + tile_w - 1) + 0.5 - half_w) * dbx;
    let dy0 = pd.dab_off_y + (f64::from(tile_y0) + 0.5 - half_h) * dby;
    let dy1 = pd.dab_off_y + (f64::from(tile_y0 + tile_h - 1) + 0.5 - half_h) * dby;

    let mips = if pd.dual { ctx.assets.dual_tip.as_deref() } else { ctx.assets.tip.as_deref() };
    let sampler = DabSampler { pd, dbx, dby, mips };
    if mips.is_some() {
        // Sampled tip: ensure entire sub-tile spans < 1e-4 of a single tip texel.
        if span_b * f64::from(pd.inv_scale) > 1e-4 {
            return false;
        }
    } else if !pd.aliased {
        // Round tip: guarantee we are NOT straddling a razor-sharp hard AA rim (`shoulder` tiny).
        // Distance to origin in tip-ellipse space is convex, so max distance is at one of the 4 corners.
        let corner_d2 = |dx: f64, dy: f64| -> f32 {
            let (u, v) = sampler.to_tip(dx as f32, dy as f32);
            (u * pd.ro).powi(2) + v * v
        };
        let max_d2 = corner_d2(dx0, dy0).max(corner_d2(dx1, dy0)).max(corner_d2(dx0, dy1)).max(corner_d2(dx1, dy1));
        let in_solid_core = pd.profile.core > 0.0 && max_d2 <= pd.profile.core * pd.profile.core;
        let smooth_shoulder = pd.profile.shoulder > 0.0 && span_b <= f64::from(pd.profile.shoulder) * 1e-4;
        if !in_solid_core && !smooth_shoulder {
            return false;
        }
    }

    // Evaluate once at sub-tile center and apply exact scalar deposit formula.
    let cx = tile_x0 + tile_w / 2;
    let cy = tile_y0 + tile_h / 2;
    let dx_mid = pd.dab_off_x + (f64::from(cx) + 0.5 - half_w) * dbx;
    let dy_mid = pd.dab_off_y + (f64::from(cy) + 0.5 - half_h) * dby;
    let Some((mut val, rn)) = sampler.sample(dx_mid, dy_mid) else {
        return true;
    };
    if val <= 0.0 {
        return true;
    }
    if pd.noise
        && val < 1.0
        && let Some((hx0, _, hy0, _)) = hash_origin
    {
        let n = hash2(hx0, hy0, NOISE_SALT);
        let hard = if n < val { 1.0 } else { 0.0 };
        val += (hard - val) * 0.7;
    }
    if pd.tex_tip
        && let Some(pat) = ctx.assets.texture.as_deref()
    {
        let (tx0, ty0) = ctx.xf.tile_tex_origin(cx, cy, pat, pd.tex_scale);
        val = texture_combine(pd.tex_mode, val, sample_texture(pat, pd.tex_scale, tx0, ty0), pd.depth);
    }
    let v = val * pd.alpha;
    if v <= 0.0 {
        return true;
    }
    if pd.dual {
        let c = tile.u_dual;
        if 1.0 > c {
            tile.u_dual = c + v * (1.0 - c);
            tile.touched = true;
        }
        return true;
    }
    if pd.wet {
        tile.u_depth = tile.u_depth.max((1.0 - rn).clamp(0.0, 1.0));
    }
    let c = tile.u_cov;
    let nv = if pd.opacity > c { c + v * (pd.opacity - c) } else { c };
    if nv > c {
        if ctx.per_dab_color {
            let k = if c < 1.0 { (nv - c) / (1.0 - c) } else { 0.0 };
            for (dst, &dc) in tile.u_col.iter_mut().zip(&pd.color) {
                *dst = dc * k + *dst * (1.0 - k);
            }
        }
        tile.u_cov = nv;
        if can_early_out {
            tile.min_cov = nv;
        }
        tile.touched = true;
    }
    true
}

fn accumulate_dab_on_tile(tile: &mut AccumTile, ctx: &StrokeRenderCtx, tx: u32, ty: u32, pd: &PreparedDab) {
    if tile.uniform && try_accumulate_uniform_dab(tile, ctx, tx, ty, pd) {
        return;
    }
    if tile.uniform {
        tile.materialize_dense(ctx.wet, ctx.per_dab_color, ctx.has_dual);
    }
    if pd.fast_round {
        accumulate_fast_round_tile(tile, ctx, tx, ty, pd);
        return;
    }
    let tile_x0 = tx * WORLD_BRUSH_TILE;
    let tile_y0 = ty * WORLD_BRUSH_TILE;
    let tile_w = ctx.xf.phys_w.saturating_sub(tile_x0).min(WORLD_BRUSH_TILE);
    let tile_h = ctx.xf.phys_h.saturating_sub(tile_y0).min(WORLD_BRUSH_TILE);
    let px0 = tile_x0.max(pd.x0);
    let px1 = (tile_x0 + tile_w).min(pd.x1);
    let py0 = tile_y0.max(pd.y0);
    let py1 = (tile_y0 + tile_h).min(pd.y1);
    if px0 >= px1 || py0 >= py1 {
        return;
    }
    let can_early_out = !pd.dual && !pd.wet && !ctx.per_dab_color;
    if can_early_out && tile.min_cov >= pd.opacity {
        return;
    }

    let mips = if pd.dual { ctx.assets.dual_tip.as_deref() } else { ctx.assets.tip.as_deref() };
    let sampler = DabSampler { pd, dbx: ctx.xf.brush_per_phys_x, dby: ctx.xf.brush_per_phys_y, mips };
    let half_w = f64::from(ctx.xf.phys_w) * 0.5;
    let half_h = f64::from(ctx.xf.phys_h) * 0.5;
    let dbx = ctx.xf.brush_per_phys_x;
    let dby = ctx.xf.brush_per_phys_y;
    let stride = WORLD_BRUSH_TILE as usize;
    let tex_origin = pd.tex_tip.then(|| ctx.assets.texture.as_deref().map(|pat| (pat, ctx.xf.tile_tex_origin(px0, py0, pat, pd.tex_scale)))).flatten();
    let hash_origin = pd.noise.then(|| ctx.xf.tile_hash_origin(px0, py0));

    for py in py0..py1 {
        let dy_base = pd.dab_off_y + (f64::from(py) + 0.5 - half_h) * dby;
        let ly = (py - tile_y0) as usize;
        let sy_step = py - py0;
        let hy = hash_origin.map(|(_, _, hy0, fy0)| ctx.xf.hash_cell_y(hy0, fy0, sy_step, py));
        let row_tex_y = tex_origin.map(|(_, (_, ty0))| ty0 + f64::from(sy_step) * dby);
        for px in px0..px1 {
            let lx = (px - tile_x0) as usize;
            let ti = ly * stride + lx;
            if can_early_out && tile.cov.get(ti).copied().unwrap_or(0.0) >= pd.opacity {
                continue;
            }
            let dx_base = pd.dab_off_x + (f64::from(px) + 0.5 - half_w) * dbx;
            let Some((mut val, rn)) = sampler.sample(dx_base, dy_base) else {
                continue;
            };
            if val <= 0.0 {
                continue;
            }
            let sx_step = px - px0;
            if let (Some((hx0, fx0, _, _)), Some(hy_val)) = (hash_origin, hy)
                && val < 1.0
            {
                let hx_val = ctx.xf.hash_cell_x(hx0, fx0, sx_step, px);
                let n = hash2(hx_val, hy_val, NOISE_SALT);
                let hard = if n < val { 1.0 } else { 0.0 };
                val += (hard - val) * 0.7;
            }
            if let (Some((pat, (tx0, _))), Some(wy)) = (tex_origin, row_tex_y) {
                let wx = tx0 + f64::from(sx_step) * dbx;
                val = texture_combine(pd.tex_mode, val, sample_texture(pat, pd.tex_scale, wx, wy), pd.depth);
            }
            let v = val * pd.alpha;
            if v <= 0.0 {
                continue;
            }
            deposit_dab_pixel(tile, ti, pd, ctx.per_dab_color, v, rn);
        }
    }
    if can_early_out && px0 == tile_x0 && px1 == tile_x0 + tile_w && py0 == tile_y0 && py1 == tile_y0 + tile_h && pd.opacity > tile.min_cov {
        tile.min_cov = tile.min_cov.max(tile_active_min_cov(tile, tile_w as usize, tile_h as usize));
    }
}

#[allow(clippy::too_many_arguments)]
fn try_finalize_uniform_tile(
    ctx: &StrokeRenderCtx,
    tile: &AccumTile,
    x0: u32,
    y0: u32,
    width: u32,
    height: u32,
    direct_premul: bool,
    opacity: f32,
    dual_mode: Option<MaskMode>,
    stroke_tex: Option<(&PatternImage, f32, MaskMode, f32)>,
    tex_origin: Option<(f64, f64)>,
    hash_origin: Option<(i32, f64, i32, f64)>,
) -> Option<Option<BrushTile>> {
    if !tile.uniform {
        return None;
    }
    let dbx = ctx.xf.brush_per_phys_x;
    let dby = ctx.xf.brush_per_phys_y;
    let span_b = (f64::from(width) * dbx).max(f64::from(height) * dby);
    let tex_const = stroke_tex.is_none_or(|(_, scale, _, _)| span_b <= f64::from(scale.max(0.01)) * 1e-5);
    let dissolve_const = hash_origin.is_none_or(|(hx0, fx0, hy0, fy0)| {
        ctx.xf.hash_cell_x(hx0, fx0, width - 1, x0 + width - 1) == hx0 && ctx.xf.hash_cell_y(hy0, fy0, height - 1, y0 + height - 1) == hy0
    });
    if !tex_const || !dissolve_const {
        return None;
    }
    let c = tile.u_cov;
    if c <= 0.0 {
        return Some(None);
    }
    let mut m = if ctx.brush.wet_edges { c * wet_factor(1.0 - tile.u_depth) } else { c };
    if let Some(dm) = dual_mode {
        m = mask_combine(dm, m, tile.u_dual, 1.0);
    }
    if let (Some((pat, scale, tmode, depth)), Some((tx0, ty0))) = (stroke_tex, tex_origin) {
        let wx = tx0 + f64::from(width / 2) * dbx;
        let wy = ty0 + f64::from(height / 2) * dby;
        m = texture_combine(tmode, m, sample_texture(pat, scale, wx, wy), depth);
    }
    let mut k = (m * opacity).clamp(0.0, 1.0);
    if k <= 0.0 {
        return Some(None);
    }
    if let Some((hx0, _, hy0, _)) = hash_origin {
        k = if hash2(hx0, hy0, ctx.brush.seed) < k { 1.0 } else { 0.0 };
        if k <= 0.0 {
            return Some(None);
        }
    }
    let (px, mask_val) = if ctx.erase {
        ([0.0, 0.0, 0.0, k], 0.0)
    } else {
        let src = if ctx.per_dab_color {
            let p = tile.u_col;
            [(p[0] / c).clamp(0.0, 1.0), (p[1] / c).clamp(0.0, 1.0), (p[2] / c).clamp(0.0, 1.0), (p[3] / c).clamp(0.0, 1.0)]
        } else {
            ctx.color
        };
        if direct_premul {
            let sa = (src[3] * k).clamp(0.0, 1.0);
            ([src[0] * sa, src[1] * sa, src[2] * sa, sa], 0.0)
        } else {
            (src, k)
        }
    };
    Some(Some(BrushTile { x0, y0, width, height, uniform_pixel: Some((px, mask_val)), pixels: Vec::new(), mask: Vec::new() }))
}

fn finalize_single_tile(ctx: &StrokeRenderCtx, tx: u32, ty: u32, tile: &AccumTile) -> Option<BrushTile> {
    if !tile.touched {
        return None;
    }
    let x0 = tx * WORLD_BRUSH_TILE;
    let y0 = ty * WORLD_BRUSH_TILE;
    let width = ctx.xf.phys_w.saturating_sub(x0).min(WORLD_BRUSH_TILE);
    let height = ctx.xf.phys_h.saturating_sub(y0).min(WORLD_BRUSH_TILE);
    if width == 0 || height == 0 {
        return None;
    }
    let mode = blend_mode_to_paint(ctx.brush.mode);
    let behind = ctx.brush.behind && !ctx.erase;
    let direct_premul = ctx.erase || behind || matches!(mode, BlendMode::Normal | BlendMode::Dissolve);
    let opacity = ctx.brush.opacity.clamp(0.0, 1.0);
    let dual_mode = ctx.brush.dual_brush.as_deref().map(|d| mask_mode_to_paint(d.mode));
    let stroke_tex = ctx
        .brush
        .texture
        .as_ref()
        .filter(|tx_cfg| !tx_cfg.each_tip)
        .and_then(|tx_cfg| ctx.assets.texture.as_deref().map(|pat| (pat, tx_cfg.scale, mask_mode_to_paint(tx_cfg.mode), tx_cfg.depth.clamp(0.0, 1.0))));
    let tex_origin = stroke_tex.map(|(pat, scale, _, _)| ctx.xf.tile_tex_origin(x0, y0, pat, scale));
    let hash_origin = (mode == BlendMode::Dissolve).then(|| ctx.xf.tile_hash_origin(x0, y0));
    let dbx = ctx.xf.brush_per_phys_x;
    let dby = ctx.xf.brush_per_phys_y;
    let stride = WORLD_BRUSH_TILE as usize;

    // Fast O(1) uniform finalization when the accumulated sub-tile, stroke texture, and Dissolve
    // hash cell are all invariant across the sub-tile (zero 64 KiB heap allocation!).
    if let Some(res) = try_finalize_uniform_tile(ctx, tile, x0, y0, width, height, direct_premul, opacity, dual_mode, stroke_tex, tex_origin, hash_origin) {
        return res;
    }

    let mut pixels = vec![[0.0f32; 4]; TILE_PIXELS];
    let mut mask = if direct_premul { Vec::new() } else { vec![0.0f32; TILE_PIXELS] };
    let mut any_nonzero = false;

    for ly in 0..height as usize {
        let py = y0 + ly as u32;
        let row_tex_y = tex_origin.map(|(_, ty0)| ty0 + f64::from(ly as u32) * dby);
        let hy = hash_origin.map(|(_, _, hy0, fy0)| ctx.xf.hash_cell_y(hy0, fy0, ly as u32, py));
        let row_base = ly * stride;
        for lx in 0..width as usize {
            let i = row_base + lx;
            let c = if tile.uniform { tile.u_cov } else { tile.cov.get(i).copied().unwrap_or(0.0) };
            if c <= 0.0 {
                continue;
            }
            let mut m = if ctx.brush.wet_edges {
                let dp = if tile.uniform { tile.u_depth } else { tile.depth.get(i).copied().unwrap_or(0.0) };
                c * wet_factor(1.0 - dp)
            } else {
                c
            };
            if let Some(dm) = dual_mode {
                let dv = if tile.uniform { tile.u_dual } else { tile.dual.get(i).copied().unwrap_or(0.0) };
                m = mask_combine(dm, m, dv, 1.0);
            }
            if let (Some((pat, scale, tmode, depth)), Some((tx0, _)), Some(wy)) = (stroke_tex, tex_origin, row_tex_y) {
                let wx = tx0 + f64::from(lx as u32) * dbx;
                m = texture_combine(tmode, m, sample_texture(pat, scale, wx, wy), depth);
            }
            let mut k = (m * opacity).clamp(0.0, 1.0);
            if k <= 0.0 {
                continue;
            }
            if let (Some((hx0, fx0, _, _)), Some(hy_val)) = (hash_origin, hy) {
                let px = x0 + lx as u32;
                let hx_val = ctx.xf.hash_cell_x(hx0, fx0, lx as u32, px);
                k = if hash2(hx_val, hy_val, ctx.brush.seed) < k { 1.0 } else { 0.0 };
                if k <= 0.0 {
                    continue;
                }
            }
            any_nonzero = true;
            if ctx.erase {
                if let Some(slot) = pixels.get_mut(i) {
                    *slot = [0.0, 0.0, 0.0, k];
                }
            } else {
                let src = if ctx.per_dab_color && (tile.uniform || !tile.col.is_empty()) {
                    let p = if tile.uniform { tile.u_col } else { tile.col.get(i).copied().unwrap_or([0.0; 4]) };
                    [(p[0] / c).clamp(0.0, 1.0), (p[1] / c).clamp(0.0, 1.0), (p[2] / c).clamp(0.0, 1.0), (p[3] / c).clamp(0.0, 1.0)]
                } else {
                    ctx.color
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
    any_nonzero.then_some(BrushTile { x0, y0, width, height, uniform_pixel: None, pixels, mask })
}

struct Accumulator {
    ctx: StrokeRenderCtx,
    tiles: BTreeMap<(u32, u32), AccumTile>,
    dabs_done: usize,
}

impl Accumulator {
    fn new(stroke: &Stroke, brush: &StrokeBrush, assets: Arc<PreparedAssets>, xf: BrushTransform) -> Self {
        Self { ctx: StrokeRenderCtx::new(stroke, brush, assets, xf), tiles: BTreeMap::new(), dabs_done: 0 }
    }

    fn accumulate_dab(&mut self, d: &Dab, dual: bool, work: &mut u64, max_work: u64, dirty: Option<&mut BTreeSet<(u32, u32)>>) -> Result<()> {
        if self.dabs_done >= MAX_STROKE_DABS {
            return Ok(());
        }
        self.dabs_done += 1;
        let Some(pd) = self.ctx.prepare_dab(d, dual) else {
            return Ok(());
        };
        *work = work.saturating_add(pd.samples());
        if *work > max_work {
            return Err(err("World brush render budget reached; choose a closer view or smaller output"));
        }
        let t0x = pd.x0 / WORLD_BRUSH_TILE;
        let t1x = (pd.x1 - 1) / WORLD_BRUSH_TILE;
        let t0y = pd.y0 / WORLD_BRUSH_TILE;
        let t1y = (pd.y1 - 1) / WORLD_BRUSH_TILE;
        let wet = self.ctx.wet;
        let per_dab_color = self.ctx.per_dab_color;
        let has_dual = self.ctx.has_dual;
        let mut dirty = dirty;
        for ty in t0y..=t1y {
            for tx in t0x..=t1x {
                let tile = self.tiles.entry((tx, ty)).or_insert_with(|| AccumTile::new(wet, per_dab_color, has_dual));
                accumulate_dab_on_tile(tile, &self.ctx, tx, ty, &pd);
                if let Some(set) = dirty.as_deref_mut() {
                    set.insert((tx, ty));
                }
            }
        }
        Ok(())
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
    rasterize_with_transform(stroke, brush, xf, work, max_work, true, None)
}

/// Directly rasterize one brush stroke across the full `260×260` (`TILE_PHYSICAL`)
/// grid of a world-space LOD tile, supporting cooperative cancellation.
pub fn rasterize_world_stroke_in_tile(
    stroke: &Stroke,
    tile: photocraft_scene::spatial::TileKey,
    work: &mut u64,
    max_work: u64,
    allow_parallel: bool,
    cancel: Option<&AtomicBool>,
) -> Result<Option<RasterizedBrushStroke>> {
    let Some(brush) = stroke.brush.as_deref() else {
        return Ok(None);
    };
    let Some(xf) = BrushTransform::for_world_tile(stroke, brush, tile)? else {
        return Ok(None);
    };
    rasterize_with_transform(stroke, brush, xf, work, max_work, allow_parallel, cancel)
}

/// Deterministic world-pixel hash in `[0, 1)` anchored to exact `i128` world-tile
/// pixel coordinates so adjacent tiles share identical dithering at their seams.
#[inline]
fn world_dither_noise(wx: i128, wy: i128, lod: u8) -> f32 {
    let mut h = (wx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= (wy as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= ((wx >> 64) as u64).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= ((wy >> 64) as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    h ^= u64::from(lod).wrapping_mul(0x517C_C1B7_2722_0A95) ^ DITHER_SALT;
    h = (h ^ (h >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h = (h ^ (h >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    ((h >> 40) as f32) * (1.0 / 16_777_216.0)
}

/// Pack a `260×260` premultiplied `[f32; 4]` world tile into `260 * 260 * 4`
/// premultiplied `Rgba8Unorm` bytes with world-anchored seam-free dithering.
pub fn pack_world_tile_rgba8(tile: photocraft_scene::spatial::TileKey, pixels: &[[f32; 4]], aliased: bool) -> Vec<u8> {
    use photocraft_scene::spatial::{TILE_APRON, TILE_LOGICAL, TILE_PHYSICAL};
    let side = TILE_PHYSICAL as usize;
    let mut out = vec![0u8; side * side * 4];
    let base_wx = tile.tx.wrapping_mul(i128::from(TILE_LOGICAL)).wrapping_sub(i128::from(TILE_APRON));
    let base_wy = tile.ty.wrapping_mul(i128::from(TILE_LOGICAL)).wrapping_sub(i128::from(TILE_APRON));
    for (ly, (src_row, dst_row)) in pixels.chunks_exact(side).zip(out.chunks_exact_mut(side * 4)).take(side).enumerate() {
        let wy = base_wy.wrapping_add(ly as i128);
        for (lx, (px_val, slot)) in src_row.iter().zip(dst_row.chunks_exact_mut(4)).enumerate() {
            if px_val[3] <= 0.0 {
                continue;
            }
            let d = if aliased {
                0.0
            } else {
                let wx = base_wx.wrapping_add(lx as i128);
                (world_dither_noise(wx, wy, tile.lod) - 0.5) / 255.0
            };
            let a = ((px_val[3] + d).clamp(0.0, 1.0) * 255.0).round() as u8;
            slot[0] = (((px_val[0] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
            slot[1] = (((px_val[1] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
            slot[2] = (((px_val[2] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
            slot[3] = a;
        }
    }
    out
}

fn rasterize_with_transform(
    stroke: &Stroke,
    brush: &StrokeBrush,
    xf: BrushTransform,
    work: &mut u64,
    max_work: u64,
    allow_parallel: bool,
    cancel: Option<&AtomicBool>,
) -> Result<Option<RasterizedBrushStroke>> {
    if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Ok(None);
    }
    let assets = prepare_assets(brush)?;
    let settings = to_generator_settings(stroke, brush);
    let smoothing_zoom = brush.smoothing.as_ref().map_or(1.0, |sm| sm.zoom);
    let mut generator = DabGenerator::new(&settings, smoothing_zoom);
    let points = stroke_points_in_brush_space(stroke, brush);
    let mut dabs = Vec::new();
    let mut duals = Vec::new();
    generator.push(&points, &mut dabs, &mut duals);
    generator.finish(&mut dabs, &mut duals);

    let ctx = StrokeRenderCtx::new(stroke, brush, assets, xf);
    let mut prepared = Vec::with_capacity(dabs.len().saturating_add(duals.len()).min(MAX_STROKE_DABS));
    let mut stroke_samples = 0u64;
    let mut min_tx = u32::MAX;
    let mut max_tx = 0u32;
    let mut min_ty = u32::MAX;
    let mut max_ty = 0u32;

    for (idx, (d, dual)) in dabs.iter().map(|d| (d, false)).chain(duals.iter().map(|d| (d, true))).take(MAX_STROKE_DABS).enumerate() {
        if (idx & 15) == 0 && cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Ok(None);
        }
        let Some(pd) = ctx.prepare_dab(d, dual) else {
            continue;
        };
        let samples = pd.samples();
        *work = work.saturating_add(samples);
        if *work > max_work {
            return Err(err("World brush render budget reached; choose a closer view or smaller output"));
        }
        stroke_samples = stroke_samples.saturating_add(samples);
        min_tx = min_tx.min(pd.x0 / WORLD_BRUSH_TILE);
        max_tx = max_tx.max((pd.x1 - 1) / WORLD_BRUSH_TILE);
        min_ty = min_ty.min(pd.y0 / WORLD_BRUSH_TILE);
        max_ty = max_ty.max((pd.y1 - 1) / WORLD_BRUSH_TILE);
        prepared.push(pd);
    }

    if prepared.is_empty() || min_tx > max_tx || min_ty > max_ty {
        return Ok(None);
    }

    let grid_w = (max_tx - min_tx + 1) as usize;
    let grid_h = (max_ty - min_ty + 1) as usize;
    let grid_len = grid_w.checked_mul(grid_h).ok_or_else(|| err("Brush tile grid overflow"))?;
    let mut buckets: Vec<Vec<u32>> = vec![Vec::new(); grid_len];
    for (idx, pd) in prepared.iter().enumerate() {
        let t0x = pd.x0 / WORLD_BRUSH_TILE;
        let t1x = (pd.x1 - 1) / WORLD_BRUSH_TILE;
        let t0y = pd.y0 / WORLD_BRUSH_TILE;
        let t1y = (pd.y1 - 1) / WORLD_BRUSH_TILE;
        let idx_u32 = idx as u32;
        for ty in t0y..=t1y {
            let row = (ty - min_ty) as usize * grid_w;
            for tx in t0x..=t1x {
                if let Some(slot) = buckets.get_mut(row + (tx - min_tx) as usize) {
                    slot.push(idx_u32);
                }
            }
        }
    }

    let jobs: Vec<(u32, u32, Vec<u32>)> = buckets
        .into_iter()
        .enumerate()
        .filter_map(|(i, list)| {
            if list.is_empty() {
                None
            } else {
                let tx = min_tx + (i % grid_w) as u32;
                let ty = min_ty + (i / grid_w) as u32;
                Some((tx, ty, list))
            }
        })
        .collect();

    let run_tile = |(tx, ty, list): &(u32, u32, Vec<u32>)| -> Option<BrushTile> {
        let mut tile = AccumTile::new(ctx.wet, ctx.per_dab_color, ctx.has_dual);
        for (step, &idx) in list.iter().enumerate() {
            if (step & 15) == 0 && cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return None;
            }
            if let Some(pd) = prepared.get(idx as usize) {
                accumulate_dab_on_tile(&mut tile, &ctx, *tx, *ty, pd);
            }
        }
        finalize_single_tile(&ctx, *tx, *ty, &tile)
    };

    #[cfg(not(target_arch = "wasm32"))]
    let tiles: Vec<BrushTile> = if allow_parallel && jobs.len() >= PARALLEL_TILE_THRESHOLD && stroke_samples >= PARALLEL_SAMPLE_THRESHOLD {
        jobs.par_iter().filter_map(run_tile).collect()
    } else {
        jobs.iter().filter_map(run_tile).collect()
    };
    #[cfg(target_arch = "wasm32")]
    let tiles: Vec<BrushTile> = {
        let _ = allow_parallel;
        jobs.iter().filter_map(run_tile).collect()
    };

    if tiles.is_empty() || cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Ok(None);
    }
    Ok(Some(RasterizedBrushStroke {
        mode: blend_mode_to_paint(brush.mode),
        behind: brush.behind && !stroke.erase,
        erase: stroke.erase,
        aliased: brush.aliased,
        phys_w: xf.phys_w,
        phys_h: xf.phys_h,
        tiles,
    }))
}

/// Composite one finalized `RasterizedBrushStroke` onto a premultiplied `[f32; 4]`
/// device layer buffer (`width × height` in physical pixels).
pub fn composite_brush_onto_layer(raster: &RasterizedBrushStroke, layer: &mut [[f32; 4]], width: u32, mut touch: impl FnMut(usize)) {
    let stride = WORLD_BRUSH_TILE as usize;
    let w = width as usize;
    let direct_premul = raster.is_direct_premul();
    for tile in &raster.tiles {
        if let Some((src, k)) = tile.uniform_pixel {
            if direct_premul && src[3] <= 0.0 {
                continue;
            }
            if !direct_premul && k <= 0.0 {
                continue;
            }
            for ly in 0..tile.height as usize {
                let py = tile.y0 as usize + ly;
                let row_base = py.saturating_mul(w);
                for lx in 0..tile.width as usize {
                    let idx = row_base + tile.x0 as usize + lx;
                    touch(idx);
                    let Some(dst) = layer.get_mut(idx) else { continue };
                    composite_single_brush_pixel(raster, direct_premul, src, k, dst);
                }
            }
            continue;
        }
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
                    composite_single_brush_pixel(raster, true, src, 0.0, dst);
                } else {
                    let k = tile.mask.get(ti).copied().unwrap_or(0.0);
                    if k <= 0.0 {
                        continue;
                    }
                    let Some(&src_straight) = tile.pixels.get(ti) else { continue };
                    touch(idx);
                    let Some(dst) = layer.get_mut(idx) else { continue };
                    composite_single_brush_pixel(raster, false, src_straight, k, dst);
                }
            }
        }
    }
}

#[inline]
fn composite_single_brush_pixel(raster: &RasterizedBrushStroke, direct_premul: bool, src: [f32; 4], k: f32, dst: &mut [f32; 4]) {
    if direct_premul {
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
        let dst_straight = if dst[3] > 1e-6 {
            [(dst[0] / dst[3]).clamp(0.0, 1.0), (dst[1] / dst[3]).clamp(0.0, 1.0), (dst[2] / dst[3]).clamp(0.0, 1.0), dst[3].clamp(0.0, 1.0)]
        } else {
            [0.0; 4]
        };
        let out = photocraft_color::blend::composite(raster.mode, dst_straight, src, k);
        let a = out[3].clamp(0.0, 1.0);
        *dst = [(out[0] * a).clamp(0.0, 1.0), (out[1] * a).clamp(0.0, 1.0), (out[2] * a).clamp(0.0, 1.0), a];
    }
}

/// Fast UI-thread live-stroke compositing onto an already-rendered premultiplied `260×260` RGBA8
/// world tile (`~0.1 ms` vs rasterizing committed strokes). Exact (within 8-bit base quantization)
/// for associative `SrcOver` strokes on topmost devices, and provides immediate non-blocking visual
/// feedback while background `WarmSplitBands` prepares exact multi-band W4 caches on cold tiles.
pub fn composite_brush_onto_rgba8_tile(
    tile_key: photocraft_scene::spatial::TileKey,
    raster: &RasterizedBrushStroke,
    base_rgba8: &[u8],
    all_aliased: bool,
) -> Vec<u8> {
    use photocraft_scene::spatial::{TILE_APRON, TILE_LOGICAL, TILE_PHYSICAL};
    let side = TILE_PHYSICAL as usize;
    let byte_len = side * side * 4;
    let mut out = if base_rgba8.len() == byte_len { base_rgba8.to_vec() } else { vec![255u8; byte_len] };
    if raster.erase || raster.tiles.is_empty() {
        return out;
    }
    let stride = WORLD_BRUSH_TILE as usize;
    let direct_premul = raster.is_direct_premul();
    let base_wx = tile_key.tx.wrapping_mul(i128::from(TILE_LOGICAL)).wrapping_sub(i128::from(TILE_APRON));
    let base_wy = tile_key.ty.wrapping_mul(i128::from(TILE_LOGICAL)).wrapping_sub(i128::from(TILE_APRON));
    let blend_px = |px: usize, py: usize, src: [f32; 4], k: f32, out: &mut [u8]| {
        if px >= side || py >= side {
            return;
        }
        let off = (py * side + px) * 4;
        let Some(slot) = out.get_mut(off..off + 4) else {
            return;
        };
        let mut dst = [f32::from(slot[0]) / 255.0, f32::from(slot[1]) / 255.0, f32::from(slot[2]) / 255.0, f32::from(slot[3]) / 255.0];
        if direct_premul {
            let keep = (1.0 - src[3]).clamp(0.0, 1.0);
            for ch in 0..4 {
                dst[ch] = (src[ch] + dst[ch] * keep).clamp(0.0, 1.0);
            }
        } else {
            composite_single_brush_pixel(raster, false, src, k, &mut dst);
        }
        let d = if all_aliased {
            0.0
        } else {
            let wx = base_wx.wrapping_add(px as i128);
            let wy = base_wy.wrapping_add(py as i128);
            (world_dither_noise(wx, wy, tile_key.lod) - 0.5) / 255.0
        };
        let a = ((dst[3] + d).clamp(0.0, 1.0) * 255.0).round() as u8;
        slot[0] = (((dst[0] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
        slot[1] = (((dst[1] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
        slot[2] = (((dst[2] + d).clamp(0.0, 1.0) * 255.0).round() as u8).min(a);
        slot[3] = a;
    };
    for tile in &raster.tiles {
        if let Some((src, k)) = tile.uniform_pixel {
            if (direct_premul && src[3] <= 0.0) || (!direct_premul && k <= 0.0) {
                continue;
            }
            for ly in 0..tile.height as usize {
                let py = tile.y0 as usize + ly;
                for lx in 0..tile.width as usize {
                    blend_px(tile.x0 as usize + lx, py, src, k, &mut out);
                }
            }
            continue;
        }
        for ly in 0..tile.height as usize {
            let py = tile.y0 as usize + ly;
            for lx in 0..tile.width as usize {
                let ti = ly * stride + lx;
                if direct_premul {
                    let Some(&src) = tile.pixels.get(ti) else { continue };
                    if src[3] <= 0.0 {
                        continue;
                    }
                    blend_px(tile.x0 as usize + lx, py, src, 0.0, &mut out);
                } else {
                    let k = tile.mask.get(ti).copied().unwrap_or(0.0);
                    if k <= 0.0 {
                        continue;
                    }
                    let Some(&src) = tile.pixels.get(ti) else { continue };
                    blend_px(tile.x0 as usize + lx, py, src, k, &mut out);
                }
            }
        }
    }
    out
}

/// Incremental live brush renderer for the active local shared-canvas draft stroke.
/// Feeds only newly appended pointer samples or stationary hold ticks on each frame,
/// re-finalizing only dirty 64×64 tiles touched by the newest dabs.
pub struct LiveWorldBrush {
    view: Viewport,
    density: f32,
    p0: [f64; 2],
    brush_per_local: f64,
    generator: DabGenerator,
    accum: Accumulator,
    finalized_cache: BTreeMap<(u32, u32), BrushTile>,
    last_dirty: Vec<(u32, u32)>,
    fed_points: usize,
    work: u64,
}

impl LiveWorldBrush {
    pub fn new(stroke: &Stroke, view: &Viewport, density: f32) -> Result<Self> {
        let brush = stroke.brush.as_deref().ok_or_else(|| err("LiveWorldBrush requires StrokeBrush"))?;
        let p0 = *stroke.points.first().ok_or_else(|| err("LiveWorldBrush requires at least one point"))?;
        let xf = BrushTransform::new(stroke, brush, view, density)?;
        let assets = prepare_assets(brush)?;
        let settings = to_generator_settings(stroke, brush);
        let smoothing_zoom = brush.smoothing.as_ref().map_or(1.0, |sm| sm.zoom);
        let generator = DabGenerator::new(&settings, smoothing_zoom);
        let accum = Accumulator::new(stroke, brush, assets, xf);
        let mut live = Self {
            view: view.clone(),
            density,
            p0,
            brush_per_local: f64::from(brush.authored_size) / stroke.width,
            generator,
            accum,
            finalized_cache: BTreeMap::new(),
            last_dirty: Vec::new(),
            fed_points: 0,
            work: 0,
        };
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
        Some((self.accum.ctx.xf.brush_to_logical(a[0], a[1], &self.view), self.accum.ctx.xf.brush_to_logical(b[0], b[1], &self.view)))
    }

    /// 64×64 tile coordinates modified during the most recent `sync_points` call.
    pub fn last_dirty_tiles(&self) -> &[(u32, u32)] {
        &self.last_dirty
    }

    pub fn sync_points(&mut self, points: &[[f64; 2]], samples: Option<&[StrokeSample]>) -> Result<()> {
        self.last_dirty.clear();
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
        let mut dirty = BTreeSet::new();
        for d in &dabs {
            self.accum.accumulate_dab(d, false, &mut self.work, MAX_BRUSH_WORK_SAMPLES, Some(&mut dirty))?;
        }
        for d in &duals {
            self.accum.accumulate_dab(d, true, &mut self.work, MAX_BRUSH_WORK_SAMPLES, Some(&mut dirty))?;
        }
        for &(tx, ty) in &dirty {
            if let Some(tile) = self.accum.tiles.get(&(tx, ty)) {
                match finalize_single_tile(&self.accum.ctx, tx, ty, tile) {
                    Some(finalized) => {
                        self.finalized_cache.insert((tx, ty), finalized);
                    }
                    None => {
                        self.finalized_cache.remove(&(tx, ty));
                    }
                }
            }
        }
        self.last_dirty.extend(dirty);
        Ok(())
    }

    /// Produce the finalized live preview, including any pending first dab or smoothing catch-up tail,
    /// without mutating the live generator state or re-finalizing untouched tiles.
    pub fn preview_raster(&self) -> Result<Option<RasterizedBrushStroke>> {
        let mut tail_gen = self.generator.clone();
        let mut tail_dabs = Vec::new();
        let mut tail_duals = Vec::new();
        tail_gen.finish(&mut tail_dabs, &mut tail_duals);
        let ctx = &self.accum.ctx;
        let tiles = if tail_dabs.is_empty() && tail_duals.is_empty() {
            self.finalized_cache.values().cloned().collect::<Vec<_>>()
        } else {
            // Clone ONLY the small subset of tiles touched by the smoothing tail (never the full stroke).
            let mut work = self.work;
            let remaining = MAX_STROKE_DABS.saturating_sub(self.accum.dabs_done);
            let mut overrides: BTreeMap<(u32, u32), AccumTile> = BTreeMap::new();
            for (d, dual) in tail_dabs.iter().map(|d| (d, false)).chain(tail_duals.iter().map(|d| (d, true))).take(remaining) {
                let Some(pd) = ctx.prepare_dab(d, dual) else {
                    continue;
                };
                work = work.saturating_add(pd.samples());
                if work > MAX_BRUSH_WORK_SAMPLES {
                    return Err(err("World brush render budget reached; choose a closer view or smaller output"));
                }
                let t0x = pd.x0 / WORLD_BRUSH_TILE;
                let t1x = (pd.x1 - 1) / WORLD_BRUSH_TILE;
                let t0y = pd.y0 / WORLD_BRUSH_TILE;
                let t1y = (pd.y1 - 1) / WORLD_BRUSH_TILE;
                for ty in t0y..=t1y {
                    for tx in t0x..=t1x {
                        let tile = overrides.entry((tx, ty)).or_insert_with(|| {
                            self.accum.tiles.get(&(tx, ty)).cloned().unwrap_or_else(|| AccumTile::new(ctx.wet, ctx.per_dab_color, ctx.has_dual))
                        });
                        accumulate_dab_on_tile(tile, ctx, tx, ty, &pd);
                    }
                }
            }
            let mut merged = self.finalized_cache.clone();
            for ((tx, ty), tile) in overrides {
                match finalize_single_tile(ctx, tx, ty, &tile) {
                    Some(finalized) => {
                        merged.insert((tx, ty), finalized);
                    }
                    None => {
                        merged.remove(&(tx, ty));
                    }
                }
            }
            merged.into_values().collect()
        };
        if tiles.is_empty() {
            return Ok(None);
        }
        Ok(Some(RasterizedBrushStroke {
            mode: blend_mode_to_paint(ctx.brush.mode),
            behind: ctx.brush.behind && !ctx.erase,
            erase: ctx.erase,
            aliased: ctx.brush.aliased,
            phys_w: ctx.xf.phys_w,
            phys_h: ctx.xf.phys_h,
            tiles,
        }))
    }
}

struct LiveTileAccum {
    accum: Accumulator,
    slot_index: BTreeMap<(u32, u32), usize>,
    base_raster: RasterizedBrushStroke,
    tail_raster: Option<RasterizedBrushStroke>,
    applied_dabs: usize,
    applied_tail_rev: u64,
    had_tail: bool,
    tile_rev: u64,
    work: u64,
}

impl LiveTileAccum {
    fn new(stroke: &Stroke, brush: &StrokeBrush, assets: Arc<PreparedAssets>, xf: BrushTransform) -> Self {
        let base_raster = RasterizedBrushStroke {
            mode: blend_mode_to_paint(brush.mode),
            behind: brush.behind && !stroke.erase,
            erase: stroke.erase,
            aliased: brush.aliased,
            phys_w: xf.phys_w,
            phys_h: xf.phys_h,
            tiles: Vec::new(),
        };
        Self {
            accum: Accumulator::new(stroke, brush, assets, xf),
            slot_index: BTreeMap::new(),
            base_raster,
            tail_raster: None,
            applied_dabs: 0,
            applied_tail_rev: 0,
            had_tail: false,
            tile_rev: 0,
            work: 0,
        }
    }

    fn update_base_slot(&mut self, key: (u32, u32), tile: Option<BrushTile>) {
        match (self.slot_index.get(&key).copied(), tile) {
            (Some(idx), Some(t)) => {
                if let Some(slot) = self.base_raster.tiles.get_mut(idx) {
                    *slot = t;
                }
            }
            (None, Some(t)) => {
                let idx = self.base_raster.tiles.len();
                self.base_raster.tiles.push(t);
                self.slot_index.insert(key, idx);
            }
            (Some(idx), None) => {
                self.slot_index.remove(&key);
                let last_idx = self.base_raster.tiles.len().saturating_sub(1);
                self.base_raster.tiles.swap_remove(idx);
                if idx < last_idx
                    && let Some(moved) = self.base_raster.tiles.get(idx)
                {
                    self.slot_index.insert((moved.x0 / WORLD_BRUSH_TILE, moved.y0 / WORLD_BRUSH_TILE), idx);
                }
            }
            (None, None) => {}
        }
    }

    fn update_for_dabs(&mut self, committed_dabs: &[(Dab, bool)], tail_dabs: &[(Dab, bool)], tail_rev: u64) -> Result<()> {
        let mut changed = false;
        if self.applied_dabs < committed_dabs.len() {
            let mut dirty = BTreeSet::new();
            for (d, dual) in committed_dabs.get(self.applied_dabs..).unwrap_or_default() {
                self.accum.accumulate_dab(d, *dual, &mut self.work, MAX_BRUSH_WORK_SAMPLES, Some(&mut dirty))?;
            }
            self.applied_dabs = committed_dabs.len();
            if !dirty.is_empty() {
                changed = true;
                for &(tx, ty) in &dirty {
                    let finalized = self.accum.tiles.get(&(tx, ty)).and_then(|sub| finalize_single_tile(&self.accum.ctx, tx, ty, sub));
                    self.update_base_slot((tx, ty), finalized);
                }
            }
        }
        let tail_dirty = self.applied_tail_rev != tail_rev && (!tail_dabs.is_empty() || self.had_tail);
        if changed || tail_dirty {
            self.applied_tail_rev = tail_rev;
            let ctx = &self.accum.ctx;
            if tail_dabs.is_empty() {
                let prev_had = self.had_tail;
                self.had_tail = false;
                self.tail_raster = None;
                if changed || prev_had {
                    self.tile_rev = self.tile_rev.saturating_add(1);
                }
            } else {
                let mut work = self.work;
                let remaining = MAX_STROKE_DABS.saturating_sub(self.accum.dabs_done);
                let mut overrides: BTreeMap<(u32, u32), AccumTile> = BTreeMap::new();
                for (d, dual) in tail_dabs.iter().take(remaining) {
                    let Some(pd) = ctx.prepare_dab(d, *dual) else {
                        continue;
                    };
                    work = work.saturating_add(pd.samples());
                    if work > MAX_BRUSH_WORK_SAMPLES {
                        return Err(err("World brush render budget reached; choose a closer view or smaller output"));
                    }
                    let t0x = pd.x0 / WORLD_BRUSH_TILE;
                    let t1x = (pd.x1 - 1) / WORLD_BRUSH_TILE;
                    let t0y = pd.y0 / WORLD_BRUSH_TILE;
                    let t1y = (pd.y1 - 1) / WORLD_BRUSH_TILE;
                    for ty in t0y..=t1y {
                        for tx in t0x..=t1x {
                            let sub = overrides.entry((tx, ty)).or_insert_with(|| {
                                self.accum.tiles.get(&(tx, ty)).cloned().unwrap_or_else(|| AccumTile::new(ctx.wet, ctx.per_dab_color, ctx.has_dual))
                            });
                            accumulate_dab_on_tile(sub, ctx, tx, ty, &pd);
                        }
                    }
                }
                if overrides.is_empty() {
                    let prev_had = self.had_tail;
                    self.had_tail = false;
                    self.tail_raster = None;
                    if changed || prev_had {
                        self.tile_rev = self.tile_rev.saturating_add(1);
                    }
                } else {
                    self.had_tail = true;
                    let mut merged = self.base_raster.tiles.clone();
                    let mut merged_idx = self.slot_index.clone();
                    for ((tx, ty), sub) in overrides {
                        match (merged_idx.get(&(tx, ty)).copied(), finalize_single_tile(ctx, tx, ty, &sub)) {
                            (Some(idx), Some(f)) => {
                                if let Some(slot) = merged.get_mut(idx) {
                                    *slot = f;
                                }
                            }
                            (None, Some(f)) => {
                                let idx = merged.len();
                                merged.push(f);
                                merged_idx.insert((tx, ty), idx);
                            }
                            (Some(idx), None) => {
                                merged_idx.remove(&(tx, ty));
                                let last = merged.len().saturating_sub(1);
                                merged.swap_remove(idx);
                                if idx < last
                                    && let Some(moved) = merged.get(idx)
                                {
                                    merged_idx.insert((moved.x0 / WORLD_BRUSH_TILE, moved.y0 / WORLD_BRUSH_TILE), idx);
                                }
                            }
                            (None, None) => {}
                        }
                    }
                    self.tail_raster = (!merged.is_empty()).then_some(RasterizedBrushStroke {
                        mode: self.base_raster.mode,
                        behind: self.base_raster.behind,
                        erase: self.base_raster.erase,
                        aliased: self.base_raster.aliased,
                        phys_w: self.base_raster.phys_w,
                        phys_h: self.base_raster.phys_h,
                        tiles: merged,
                    });
                    self.tile_rev = self.tile_rev.saturating_add(1);
                }
            }
        }
        Ok(())
    }

    fn current_raster(&self) -> Option<(u64, &RasterizedBrushStroke)> {
        if self.had_tail {
            self.tail_raster.as_ref().map(|r| (self.tile_rev, r))
        } else if !self.base_raster.tiles.is_empty() {
            Some((self.tile_rev, &self.base_raster))
        } else {
            None
        }
    }
}

/// Incremental world-space LOD tile live brush renderer (`260×260` physical tiles).
/// Runs `DabGenerator` once per pointer sample and incrementally updates only the
/// `1–4` world tiles touched by the newest dabs (plus smoothing tail when enabled).
pub struct LiveTileBrush {
    stroke: Stroke,
    lod: u8,
    p0: [f64; 2],
    brush_per_local: f64,
    assets: Arc<PreparedAssets>,
    generator: DabGenerator,
    committed_dabs: Vec<(Dab, bool)>,
    tail_dabs: Vec<(Dab, bool)>,
    tail_rev: u64,
    tile_states: BTreeMap<photocraft_scene::spatial::TileKey, LiveTileAccum>,
    fed_points: usize,
}

impl LiveTileBrush {
    pub fn new(stroke: &Stroke, lod: u8) -> Result<Self> {
        let brush = stroke.brush.as_deref().ok_or_else(|| err("LiveTileBrush requires StrokeBrush"))?;
        let p0 = *stroke.points.first().ok_or_else(|| err("LiveTileBrush requires at least one point"))?;
        let assets = prepare_assets(brush)?;
        let settings = to_generator_settings(stroke, brush);
        let smoothing_zoom = brush.smoothing.as_ref().map_or(1.0, |sm| sm.zoom);
        let generator = DabGenerator::new(&settings, smoothing_zoom);
        let mut live = Self {
            stroke: stroke.clone(),
            lod,
            p0,
            brush_per_local: f64::from(brush.authored_size) / stroke.width,
            assets,
            generator,
            committed_dabs: Vec::new(),
            tail_dabs: Vec::new(),
            tail_rev: 0,
            tile_states: BTreeMap::new(),
            fed_points: 0,
        };
        live.sync_points(&stroke.points, stroke.samples.as_deref())?;
        Ok(live)
    }

    pub fn stroke_id(&self) -> photocraft_scene::StrokeId {
        self.stroke.id
    }

    pub fn lod(&self) -> u8 {
        self.lod
    }

    /// Re-target this live brush to `lod` (e.g. across LOD hysteresis transitions) while
    /// keeping `committed_dabs` and `generator` intact so dabs are never re-generated.
    pub fn ensure_lod(&mut self, lod: u8) {
        if self.lod != lod {
            self.lod = lod;
            self.tile_states.clear();
        }
    }

    pub fn wants_time(&self) -> bool {
        self.generator.wants_time()
    }

    pub fn smoothing_string(&self, view: &Viewport) -> Option<([f64; 2], [f64; 2])> {
        let (a, b) = self.generator.smoothing_string()?;
        let brush = self.stroke.brush.as_deref()?;
        let xf = BrushTransform::new(&self.stroke, brush, view, 1.0).ok()?;
        Some((xf.brush_to_logical(a[0], a[1], view), xf.brush_to_logical(b[0], b[1], view)))
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
        for d in dabs {
            if self.committed_dabs.len() >= MAX_STROKE_DABS {
                break;
            }
            self.committed_dabs.push((d, false));
        }
        for d in duals {
            if self.committed_dabs.len() >= MAX_STROKE_DABS {
                break;
            }
            self.committed_dabs.push((d, true));
        }
        // Precompute any pending smoothing tail ONCE per pointer update instead of per tile.
        let mut tail_gen = self.generator.clone();
        let mut tail_dabs = Vec::new();
        let mut tail_duals = Vec::new();
        tail_gen.finish(&mut tail_dabs, &mut tail_duals);
        let had_any_tail = !self.tail_dabs.is_empty() || !tail_dabs.is_empty() || !tail_duals.is_empty();
        self.tail_dabs.clear();
        for d in tail_dabs {
            self.tail_dabs.push((d, false));
        }
        for d in tail_duals {
            self.tail_dabs.push((d, true));
        }
        if had_any_tail {
            self.tail_rev = self.tail_rev.saturating_add(1);
        }
        Ok(())
    }

    /// Incrementally update a batch of world tiles (in parallel across cores on native when
    /// `tiles.len() >= 2`), eliminating per-tile serial bottlenecks for large zoomed brushes.
    pub fn prepare_tiles(&mut self, tiles: &[photocraft_scene::spatial::TileKey]) -> Result<()> {
        let Some(brush) = self.stroke.brush.as_deref() else {
            return Ok(());
        };
        let mut target_keys = Vec::with_capacity(tiles.len());
        for &tile in tiles {
            if tile.lod != self.lod {
                continue;
            }
            if !self.tile_states.contains_key(&tile) {
                let Some(xf) = BrushTransform::for_world_tile(&self.stroke, brush, tile)? else {
                    continue;
                };
                self.tile_states.insert(tile, LiveTileAccum::new(&self.stroke, brush, Arc::clone(&self.assets), xf));
            }
            target_keys.push(tile);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if target_keys.len() >= 2 {
            let committed = &self.committed_dabs;
            let tail = &self.tail_dabs;
            let tail_rev = self.tail_rev;
            let mut work_states: Vec<(photocraft_scene::spatial::TileKey, LiveTileAccum)> = Vec::with_capacity(target_keys.len());
            for k in target_keys {
                if let Some(st) = self.tile_states.remove(&k) {
                    work_states.push((k, st));
                }
            }
            let results: Vec<Result<(photocraft_scene::spatial::TileKey, LiveTileAccum)>> = work_states
                .into_par_iter()
                .map(|(k, mut st)| {
                    st.update_for_dabs(committed, tail, tail_rev)?;
                    Ok((k, st))
                })
                .collect();
            let mut first_err = None;
            for res in results {
                match res {
                    Ok((k, st)) => {
                        self.tile_states.insert(k, st);
                    }
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                    }
                }
            }
            if let Some(e) = first_err {
                return Err(e);
            }
            return Ok(());
        }
        for k in target_keys {
            if let Some(st) = self.tile_states.get_mut(&k) {
                st.update_for_dabs(&self.committed_dabs, &self.tail_dabs, self.tail_rev)?;
            }
        }
        Ok(())
    }

    /// Read-only lookup of a tile already updated by `prepare_tiles`.
    pub fn get_prepared_tile(&self, tile: photocraft_scene::spatial::TileKey) -> Option<(u64, &RasterizedBrushStroke)> {
        if tile.lod != self.lod {
            return None;
        }
        self.tile_states.get(&tile).and_then(LiveTileAccum::current_raster)
    }

    /// Incrementally update and borrow the `260×260` `RasterizedBrushStroke` and monotonic
    /// per-tile revision for `tile`. Zero allocation / zero clone when `tile` is untouched.
    pub fn raster_for_tile(&mut self, tile: photocraft_scene::spatial::TileKey) -> Result<Option<(u64, &RasterizedBrushStroke)>> {
        if tile.lod != self.lod {
            return Ok(None);
        }
        let Some(brush) = self.stroke.brush.as_deref() else {
            return Ok(None);
        };
        if !self.tile_states.contains_key(&tile) {
            let Some(xf) = BrushTransform::for_world_tile(&self.stroke, brush, tile)? else {
                return Ok(None);
            };
            self.tile_states.insert(tile, LiveTileAccum::new(&self.stroke, brush, Arc::clone(&self.assets), xf));
        }
        let Some(state) = self.tile_states.get_mut(&tile) else {
            return Ok(None);
        };
        state.update_for_dabs(&self.committed_dabs, &self.tail_dabs, self.tail_rev)?;
        Ok(state.current_raster())
    }
}

#[cfg(test)]
mod cp2_tests {
    use super::*;
    use photocraft_scene::{CanvasId, OwnerId, RegionKey, Srgba, StrokeId, Wide};

    /// Naive reference rasterizer (sequential, no saturation early-out, no core fast-fill,
    /// no fast-round specialization, full `to_paint_settings`) used as the exact equivalence oracle.
    fn rasterize_reference(stroke: &Stroke, view: &Viewport, density: f32) -> Option<RasterizedBrushStroke> {
        let brush = stroke.brush.as_deref()?;
        let xf = BrushTransform::new(stroke, brush, view, density).unwrap();
        let assets = prepare_assets(brush).unwrap();
        let settings = to_paint_settings(stroke, brush).unwrap();
        let smoothing_zoom = brush.smoothing.as_ref().map_or(1.0, |sm| sm.zoom);
        let mut generator = DabGenerator::new(&settings, smoothing_zoom);
        let points = stroke_points_in_brush_space(stroke, brush);
        let mut dabs = Vec::new();
        let mut duals = Vec::new();
        generator.push(&points, &mut dabs, &mut duals);
        generator.finish(&mut dabs, &mut duals);

        let ctx = StrokeRenderCtx::new(stroke, brush, assets, xf);
        let mut tiles: BTreeMap<(u32, u32), AccumTile> = BTreeMap::new();
        for (d, dual) in dabs.iter().map(|d| (d, false)).chain(duals.iter().map(|d| (d, true))).take(MAX_STROKE_DABS) {
            let Some(mut pd) = ctx.prepare_dab(d, dual) else { continue };
            // Force generic non-specialized path and disable core fast-fill & tile min_cov early-out.
            pd.fast_round = false;
            pd.core2 = 0.0;
            let t0x = pd.x0 / WORLD_BRUSH_TILE;
            let t1x = (pd.x1 - 1) / WORLD_BRUSH_TILE;
            let t0y = pd.y0 / WORLD_BRUSH_TILE;
            let t1y = (pd.y1 - 1) / WORLD_BRUSH_TILE;
            for ty in t0y..=t1y {
                for tx in t0x..=t1x {
                    let tile = tiles.entry((tx, ty)).or_insert_with(|| AccumTile::new_dense(ctx.wet, ctx.per_dab_color, ctx.has_dual));
                    tile.min_cov = -1.0; // disable whole-tile early-out
                    let mips = if pd.dual { ctx.assets.dual_tip.as_deref() } else { ctx.assets.tip.as_deref() };
                    let sampler = DabSampler { pd: &pd, dbx: ctx.xf.brush_per_phys_x, dby: ctx.xf.brush_per_phys_y, mips };
                    let tile_x0 = tx * WORLD_BRUSH_TILE;
                    let tile_y0 = ty * WORLD_BRUSH_TILE;
                    let tile_w = ctx.xf.phys_w.saturating_sub(tile_x0).min(WORLD_BRUSH_TILE);
                    let tile_h = ctx.xf.phys_h.saturating_sub(tile_y0).min(WORLD_BRUSH_TILE);
                    let px0 = tile_x0.max(pd.x0);
                    let px1 = (tile_x0 + tile_w).min(pd.x1);
                    let py0 = tile_y0.max(pd.y0);
                    let py1 = (tile_y0 + tile_h).min(pd.y1);
                    let half_w = f64::from(ctx.xf.phys_w) * 0.5;
                    let half_h = f64::from(ctx.xf.phys_h) * 0.5;
                    let dbx = ctx.xf.brush_per_phys_x;
                    let dby = ctx.xf.brush_per_phys_y;
                    let stride = WORLD_BRUSH_TILE as usize;
                    let tex_origin =
                        pd.tex_tip.then(|| ctx.assets.texture.as_deref().map(|pat| (pat, ctx.xf.tile_tex_origin(px0, py0, pat, pd.tex_scale)))).flatten();
                    let hash_origin = pd.noise.then(|| ctx.xf.tile_hash_origin(px0, py0));
                    for py in py0..py1 {
                        let dy_base = pd.dab_off_y + (f64::from(py) + 0.5 - half_h) * dby;
                        let ly = (py - tile_y0) as usize;
                        let sy_step = py - py0;
                        let hy = hash_origin.map(|(_, _, hy0, fy0)| ctx.xf.hash_cell_y(hy0, fy0, sy_step, py));
                        let row_tex_y = tex_origin.map(|(_, (_, ty0))| ty0 + f64::from(sy_step) * dby);
                        for px in px0..px1 {
                            let dx_base = pd.dab_off_x + (f64::from(px) + 0.5 - half_w) * dbx;
                            let Some((mut val, rn)) = sampler.sample(dx_base, dy_base) else { continue };
                            if val <= 0.0 {
                                continue;
                            }
                            let sx_step = px - px0;
                            if let (Some((hx0, fx0, _, _)), Some(hy_val)) = (hash_origin, hy)
                                && val < 1.0
                            {
                                let hx_val = ctx.xf.hash_cell_x(hx0, fx0, sx_step, px);
                                let n = hash2(hx_val, hy_val, NOISE_SALT);
                                let hard = if n < val { 1.0 } else { 0.0 };
                                val += (hard - val) * 0.7;
                            }
                            if let (Some((pat, (tx0, _))), Some(wy)) = (tex_origin, row_tex_y) {
                                let wx = tx0 + f64::from(sx_step) * dbx;
                                val = texture_combine(pd.tex_mode, val, sample_texture(pat, pd.tex_scale, wx, wy), pd.depth);
                            }
                            let v = val * pd.alpha;
                            if v <= 0.0 {
                                continue;
                            }
                            let lx = (px - tile_x0) as usize;
                            deposit_dab_pixel(tile, ly * stride + lx, &pd, ctx.per_dab_color, v, rn);
                        }
                    }
                }
            }
        }
        let out_tiles: Vec<BrushTile> = tiles.iter().filter_map(|(&(tx, ty), tile)| finalize_single_tile(&ctx, tx, ty, tile)).collect();
        (!out_tiles.is_empty()).then_some(RasterizedBrushStroke {
            mode: blend_mode_to_paint(brush.mode),
            behind: brush.behind && !stroke.erase,
            erase: stroke.erase,
            aliased: brush.aliased,
            phys_w: xf.phys_w,
            phys_h: xf.phys_h,
            tiles: out_tiles,
        })
    }

    fn test_ids() -> (CanvasId, OwnerId, StrokeId) {
        (
            CanvasId::new("11111111111111111111111111111111").unwrap(),
            OwnerId::new("22222222222222222222222222222222").unwrap(),
            StrokeId::new("33333333333333333333333333333333").unwrap(),
        )
    }

    fn make_view(canvas: CanvasId, zoom: f64, depth: u8, center_local: [f64; 2]) -> Viewport {
        let region = RegionKey { depth, x: Wide(0), y: Wide(0) };
        let pt = coordinates::point(&region, center_local).unwrap();
        let camera = coordinates::target(canvas, pt, depth, zoom).unwrap();
        Viewport { camera, width: 320, height: 240 }
    }

    fn tiles_map(r: &Option<RasterizedBrushStroke>) -> BTreeMap<(u32, u32), &BrushTile> {
        let mut m = BTreeMap::new();
        if let Some(r) = r {
            for t in &r.tiles {
                m.insert((t.x0, t.y0), t);
            }
        }
        m
    }

    fn max_raster_diff(a: &Option<RasterizedBrushStroke>, b: &Option<RasterizedBrushStroke>) -> f32 {
        let ma = tiles_map(a);
        let mb = tiles_map(b);
        assert_eq!(ma.keys().collect::<Vec<_>>(), mb.keys().collect::<Vec<_>>(), "tile keys differ");
        let stride = WORLD_BRUSH_TILE as usize;
        let pixel_at = |t: &BrushTile, lx: usize, ly: usize| -> ([f32; 4], f32) {
            if let Some(u) = t.uniform_pixel {
                return u;
            }
            let i = ly * stride + lx;
            (t.pixels.get(i).copied().unwrap_or([0.0; 4]), t.mask.get(i).copied().unwrap_or(0.0))
        };
        let mut max_d = 0.0f32;
        for (k, ta) in &ma {
            let tb = mb[k];
            assert_eq!((ta.width, ta.height), (tb.width, tb.height));
            for ly in 0..ta.height as usize {
                for lx in 0..ta.width as usize {
                    let (pa, xa) = pixel_at(ta, lx, ly);
                    let (pb, xb) = pixel_at(tb, lx, ly);
                    for c in 0..4 {
                        max_d = max_d.max((pa[c] - pb[c]).abs());
                    }
                    max_d = max_d.max((xa - xb).abs());
                }
            }
        }
        max_d
    }

    #[test]
    fn optimized_rasterizer_matches_reference_exact_across_brushes_and_zooms() {
        let (canvas, owner, id) = test_ids();
        let pts: Vec<[f64; 2]> = (0..25).map(|i| [40.0 + i as f64 * 9.0, 60.0 + (i as f64 * 0.4).sin() * 35.0]).collect();
        let samples: Vec<StrokeSample> = (0..25).map(|i| StrokeSample { time_ms: i as f64 * 40.0, ..Default::default() }).collect();

        let make_brush = |mut b: BrushSettings| {
            b.seed = 42;
            to_stroke_brush(&b, 1.0, false).unwrap()
        };
        let mut hard = BrushSettings { size: 30.0, hardness: 1.0, spacing: 0.05, opacity: 1.0, flow: 1.0, ..Default::default() };
        hard.smoothing.enabled = false;
        let mut airbrush =
            BrushSettings { size: 64.0, hardness: 0.0, spacing: 0.10, opacity: 1.0, flow: 0.10, build_up: true, build_up_rate: 25.0, ..Default::default() };
        airbrush.smoothing.enabled = false;
        let mut water = BrushSettings { size: 48.0, hardness: 0.3, spacing: 0.10, opacity: 0.8, flow: 0.6, wet_edges: true, ..Default::default() };
        water.texture.enabled = true;
        water.texture.pattern = Pattern::Procedural { style: PatternStyle::Paper, size: 128, seed: 7 };
        water.smoothing.enabled = false;
        let mut chalk =
            BrushSettings { size: 36.0, hardness: 1.0, spacing: 0.12, opacity: 0.9, flow: 0.8, tip: TipShape::Sampled(chalk_tip(64, 7)), ..Default::default() };
        chalk.smoothing.enabled = false;
        let mut color_dyn = BrushSettings { size: 32.0, hardness: 0.5, spacing: 0.15, opacity: 0.85, flow: 0.5, noise: true, ..Default::default() };
        color_dyn.color_dynamics.enabled = true;
        color_dyn.color_dynamics.per_tip = true;
        color_dyn.color_dynamics.hue_jitter = 0.4;
        color_dyn.mode = BlendMode::Multiply;
        color_dyn.smoothing.enabled = false;

        let cases = [
            ("hard_1x", make_brush(hard.clone()), 1.0, false),
            ("hard_4x", make_brush(hard.clone()), 4.0, false),
            ("hard_16x", make_brush(hard), 16.0, false),
            ("airbrush_4x", make_brush(airbrush.clone()), 4.0, true),
            ("airbrush_16x", make_brush(airbrush), 16.0, true),
            ("watercolor_4x", make_brush(water), 4.0, false),
            ("chalk_4x", make_brush(chalk), 4.0, false),
            ("colordyn_mult_2x", make_brush(color_dyn), 2.0, false),
        ];

        for (label, brush, zoom, with_samples) in cases {
            let stroke = Stroke {
                id,
                canvas,
                owner,
                revision: 0,
                anchor: RegionKey { depth: 0, x: Wide(0), y: Wide(0) },
                points: pts.clone(),
                width: f64::from(brush.authored_size),
                color: Srgba { rgba: [0.2, 0.5, 0.85, 1.0] },
                erase: false,
                brush: Some(Box::new(brush)),
                samples: with_samples.then(|| samples.clone()),
            };
            let view = make_view(canvas, zoom, 0, [140.0, 60.0]);
            let mut work = 0u64;
            let opt = rasterize_world_stroke(&stroke, &view, 1.0, &mut work, MAX_BRUSH_WORK_SAMPLES).unwrap();
            let reference = rasterize_reference(&stroke, &view, 1.0);
            let diff = max_raster_diff(&opt, &reference);
            assert!(diff <= 1e-4, "{label}: optimized vs reference max f32 diff = {diff}");
            if let (Some(a), Some(b)) = (&opt, &reference)
                && a.is_direct_premul()
            {
                let ma = tiles_map(&opt);
                let mb = tiles_map(&reference);
                let mut max_u8 = 0u8;
                for (k, ta) in &ma {
                    let pa = ta.to_premul_rgba8_padded(a.aliased);
                    let pb = mb[k].to_premul_rgba8_padded(b.aliased);
                    for (&xa, &xb) in pa.iter().zip(&pb) {
                        max_u8 = max_u8.max(xa.abs_diff(xb));
                    }
                }
                assert_eq!(max_u8, 0, "{label}: optimized vs reference max u8 diff = {max_u8}");
            }
        }
    }

    #[test]
    fn custom_non_pow2_pattern_preserves_exact_native_period_past_65536() {
        let (canvas, owner, id) = test_ids();
        // 96×96 custom gradient tile (96 does NOT divide 65536) at scale 1.37 -> exact period 131.52.
        let side = 96u16;
        let mut pixels = vec![0u8; (side as usize) * (side as usize)];
        for y in 0..side as usize {
            for x in 0..side as usize {
                pixels[y * side as usize + x] = ((x * 255) / (side as usize - 1)) as u8;
            }
        }
        let img = Image::from_u8(u32::from(side), u32::from(side), ChannelLayout::Gray, pixels).unwrap();
        let png = photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).unwrap();
        let png_b64 = b64_encode(&png);
        let scale = 1.37_f32;
        let period = f64::from(side) * f64::from(scale);

        let make_stroke_at = |x_offset: f64| {
            let mut b = BrushSettings { size: 40.0, hardness: 1.0, spacing: 0.1, opacity: 1.0, flow: 1.0, ..Default::default() };
            b.smoothing.enabled = false;
            let mut sb = to_stroke_brush(&b, 1.0, false).unwrap();
            sb.texture = Some(BrushTexture {
                pattern: BrushPattern::Tile { width: side, height: side, png_b64: png_b64.clone() },
                invert: false,
                scale,
                brightness: 0.0,
                contrast: 0.0,
                each_tip: false,
                mode: BrushMaskMode::Multiply,
                depth: 1.0,
                depth_jitter: BrushDynamic::default(),
            });
            Stroke {
                id,
                canvas,
                owner,
                revision: 0,
                anchor: RegionKey { depth: 0, x: Wide(0), y: Wide(0) },
                points: vec![[x_offset, 100.0], [x_offset + 60.0, 100.0]],
                width: 40.0,
                color: Srgba { rgba: [0.1, 0.2, 0.3, 1.0] },
                erase: false,
                brush: Some(Box::new(sb)),
                samples: None,
            }
        };

        // Shift by an exact multiple of the pattern's native period (`period * 600 = 78,912 > 65,536`).
        // With an arbitrary 65536 wrap, these two strokes would jump out of phase; with exact
        // per-pattern period wrapping, their texture phases match identically!
        let x0 = 50.0;
        let x1 = x0 + period * 600.0;
        assert!(x1 > 65536.0 && x1 <= 8192.0 * 10.0);
        // Use a fine brush_per_local (authored_size = 400, stroke.width = 40 -> 10x) so local coords stay within MAX_STROKE_LOCAL!
        let mut s0 = make_stroke_at(50.0);
        s0.brush.as_mut().unwrap().authored_size = 400.0;
        s0.width = 40.0; // brush_per_local = 10 -> p0 = 50 -> bx = 500
        let local_shift = (period * 600.0) / 10.0; // 7891.2 local -> 78912 brush units (> 65536!)
        let mut s1 = make_stroke_at(50.0 + local_shift);
        s1.brush.as_mut().unwrap().authored_size = 400.0;
        s1.width = 40.0;

        let v0 = make_view(canvas, 1.0, 0, [80.0, 100.0]);
        let v1 = make_view(canvas, 1.0, 0, [80.0 + local_shift, 100.0]);
        let mut w0 = 0u64;
        let mut w1 = 0u64;
        let r0 = rasterize_world_stroke(&s0, &v0, 1.0, &mut w0, MAX_BRUSH_WORK_SAMPLES).unwrap();
        let r1 = rasterize_world_stroke(&s1, &v1, 1.0, &mut w1, MAX_BRUSH_WORK_SAMPLES).unwrap();
        let diff = max_raster_diff(&r0, &r1);
        assert!(diff <= 2e-3, "pattern shifted by exact 600*native_period past 65536 drifted by {diff}");
    }

    #[test]
    fn live_brush_dirty_tile_cache_matches_full_raster_with_smoothing_tail() {
        let (canvas, owner, id) = test_ids();
        let mut b = BrushSettings { size: 32.0, hardness: 0.4, spacing: 0.1, opacity: 0.75, flow: 0.4, wet_edges: true, ..Default::default() };
        b.smoothing.enabled = true;
        b.smoothing.amount = 0.6;
        b.smoothing.catch_up_on_end = true;
        b.seed = 99;
        let sb = to_stroke_brush(&b, 1.0, false).unwrap();
        let pts: Vec<[f64; 2]> = (0..30).map(|i| [20.0 + i as f64 * 8.0, 40.0 + (i as f64 * 0.3).cos() * 40.0]).collect();
        let samples: Vec<StrokeSample> = (0..30).map(|i| StrokeSample { time_ms: i as f64 * 16.0, ..Default::default() }).collect();
        let full_stroke = Stroke {
            id,
            canvas,
            owner,
            revision: 0,
            anchor: RegionKey { depth: 0, x: Wide(0), y: Wide(0) },
            points: pts.clone(),
            width: 32.0,
            color: Srgba { rgba: [0.8, 0.2, 0.3, 1.0] },
            erase: false,
            brush: Some(Box::new(sb)),
            samples: Some(samples.clone()),
        };
        let view = make_view(canvas, 1.0, 0, [140.0, 40.0]);
        let mut init = full_stroke.clone();
        init.points = vec![pts[0]];
        init.samples = Some(vec![samples[0]]);
        let mut live = LiveWorldBrush::new(&init, &view, 1.0).unwrap();
        for end in 2..=pts.len() {
            live.sync_points(&pts[..end], Some(&samples[..end])).unwrap();
            let _ = live.preview_raster().unwrap();
        }
        let live_final = live.preview_raster().unwrap();
        let mut work = 0u64;
        let batch_final = rasterize_world_stroke(&full_stroke, &view, 1.0, &mut work, MAX_BRUSH_WORK_SAMPLES).unwrap();
        let diff = max_raster_diff(&live_final, &batch_final);
        assert!(diff <= 1e-5, "LiveWorldBrush incremental cache diverged from full raster by {diff}");
    }

    #[test]
    fn cp8_deep_zoom_uniform_equivalence_matches_dense_oracle() {
        let (canvas, owner, id) = test_ids();
        let pts: Vec<[f64; 2]> = (0..16).map(|i| [-60.0 + i as f64 * 10.0, -20.0 + (i as f64 * 0.35).sin() * 14.0]).collect();
        let samples: Vec<StrokeSample> = (0..16).map(|i| StrokeSample { time_ms: i as f64 * 16.0, ..Default::default() }).collect();
        let make_brush = |mut b: BrushSettings| {
            b.seed = 77;
            b.smoothing.enabled = false;
            to_stroke_brush(&b, 1.0, false).unwrap()
        };
        let hard = BrushSettings { size: 40.0, hardness: 1.0, spacing: 0.1, opacity: 1.0, flow: 1.0, ..Default::default() };
        let soft = BrushSettings { size: 48.0, hardness: 0.0, spacing: 0.12, opacity: 0.7, flow: 0.25, ..Default::default() };
        let airbrush =
            BrushSettings { size: 50.0, hardness: 0.0, spacing: 0.10, opacity: 1.0, flow: 0.10, build_up: true, build_up_rate: 20.0, ..Default::default() };
        let mut water = BrushSettings { size: 44.0, hardness: 0.3, spacing: 0.12, opacity: 0.85, flow: 0.6, wet_edges: true, ..Default::default() };
        water.texture.enabled = true;
        water.texture.pattern = Pattern::Procedural { style: PatternStyle::Paper, size: 256, seed: 5 };
        let chalk =
            BrushSettings { size: 36.0, hardness: 1.0, spacing: 0.12, opacity: 0.9, flow: 0.8, tip: TipShape::Sampled(chalk_tip(64, 5)), ..Default::default() };
        let mut dissolve_noise = BrushSettings { size: 36.0, hardness: 0.5, spacing: 0.15, opacity: 0.75, flow: 0.5, noise: true, ..Default::default() };
        dissolve_noise.mode = BlendMode::Dissolve;
        let mut color_dyn = BrushSettings { size: 38.0, hardness: 0.4, spacing: 0.15, opacity: 0.85, flow: 0.5, ..Default::default() };
        color_dyn.color_dynamics.enabled = true;
        color_dyn.color_dynamics.per_tip = true;
        color_dyn.color_dynamics.hue_jitter = 0.35;
        color_dyn.mode = BlendMode::Multiply;

        let cases = [
            ("hard_d6_interior", make_brush(hard.clone()), 6u8, [0.0, 0.0], true),
            ("hard_d6_rim_aa", make_brush(hard.clone()), 6u8, [-60.0, 0.0], false),
            ("soft_d6", make_brush(soft), 6u8, [0.0, 0.0], true),
            ("airbrush_d12", make_brush(airbrush), 12u8, [0.0, 0.0], true),
            ("watercolor_d6", make_brush(water.clone()), 6u8, [0.0, 0.0], true),
            ("watercolor_d12", make_brush(water), 12u8, [0.0, 0.0], true),
            ("chalk_d12", make_brush(chalk), 12u8, [0.0, 0.0], true),
            ("dissolve_noise_d6", make_brush(dissolve_noise), 6u8, [0.0, 0.0], true),
            ("colordyn_mult_d12", make_brush(color_dyn), 12u8, [0.0, 0.0], true),
        ];

        for (label, sb, depth, center_local, expect_uniform) in cases {
            let stroke = Stroke {
                id,
                canvas,
                owner,
                revision: 0,
                anchor: RegionKey { depth: 0, x: Wide(0), y: Wide(0) },
                points: pts.clone(),
                width: f64::from(sb.authored_size),
                color: Srgba { rgba: [0.18, 0.44, 0.82, 1.0] },
                erase: false,
                brush: Some(Box::new(sb)),
                samples: Some(samples.clone()),
            };
            let view = make_view(canvas, 1.0, depth, center_local);
            let mut work = 0u64;
            let fast = rasterize_world_stroke(&stroke, &view, 1.0, &mut work, MAX_BRUSH_WORK_SAMPLES).unwrap();
            let oracle = rasterize_reference(&stroke, &view, 1.0);
            let diff = max_raster_diff(&fast, &oracle);
            assert!(diff <= 1e-4, "{label}: uniform fast path diverged from dense oracle by {diff}");
            if expect_uniform && let Some(ref f) = fast {
                assert!(f.tiles.iter().any(|t| t.uniform_pixel.is_some()), "{label}: expected uniform sub-tile shortcut to engage at depth {depth}");
            }
            if let (Some(fa), Some(or)) = (&fast, &oracle) {
                let ma = tiles_map(&fast);
                let mb = tiles_map(&oracle);
                for (k, ta) in &ma {
                    let pa = ta.to_premul_rgba8_padded(fa.aliased);
                    let pb = mb[k].to_premul_rgba8_padded(or.aliased);
                    let max_u8 = pa.iter().zip(&pb).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
                    assert_eq!(max_u8, 0, "{label}: RGBA8 delta {max_u8} != 0 vs dense oracle");
                }
            }
        }
    }
}
