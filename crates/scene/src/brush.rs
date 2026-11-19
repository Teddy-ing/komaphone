//! Versioned, platform-independent brush and sample data for shared-canvas strokes.
//!
//! `photocraft-scene` stays at L1 (zero workspace dependencies), so this module holds
//! pure validated serde data matching `photocraft-paint`'s `BrushSettings` and
//! `StrokePoint` semantics. All built-in preset tips encode symbolically via
//! [`BrushTip::Procedural`], keeping wire payloads compact; custom user/`.abr` tips
//! use bounded grayscale PNG tiles.

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

pub const STROKE_BRUSH_SCHEMA: u16 = 1;
pub const MIN_BRUSH_SIZE: f32 = 0.5;
pub const MAX_BRUSH_SIZE: f32 = 5000.0;
pub const MIN_SPACING: f32 = 0.01;
pub const MAX_SPACING: f32 = 10.0;
pub const MAX_SCATTER: f32 = 10.0;
pub const MAX_CURVE_POINTS: usize = 16;
pub const MAX_PROCEDURAL_SIZE: u32 = 256;
pub const MIN_PROCEDURAL_SIZE: u32 = 8;
pub const MAX_PROCEDURAL_PARAM: u32 = 256;
pub const MAX_SAMPLED_SIDE: u16 = 256;
pub const MAX_SAMPLED_B64_BYTES: usize = 45_000;
pub const MAX_PREVIEW_B64_BYTES: usize = 12_000;
pub const MAX_STROKE_DURATION_MS: f64 = 3_600_000.0;

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}
fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}
fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}
fn is_one_f32(v: &f32) -> bool {
    *v == 1.0
}
fn is_true(v: &bool) -> bool {
    *v
}
fn is_false(v: &bool) -> bool {
    !*v
}
fn default_true() -> bool {
    true
}
fn default_one_f32() -> f32 {
    1.0
}
fn default_spacing() -> f32 {
    0.1
}
fn default_schema() -> u16 {
    STROKE_BRUSH_SCHEMA
}
fn default_background() -> [f32; 4] {
    [1.0, 1.0, 1.0, 1.0]
}
fn is_default_background(v: &[f32; 4]) -> bool {
    *v == default_background()
}
fn default_fade_steps() -> u32 {
    25
}
fn is_default_fade_steps(v: &u32) -> bool {
    *v == default_fade_steps()
}
fn default_count() -> u32 {
    1
}
fn is_default_count(v: &u32) -> bool {
    *v == default_count()
}
fn default_build_up_rate() -> f32 {
    20.0
}
fn is_default_build_up_rate(v: &f32) -> bool {
    *v == default_build_up_rate()
}
fn is_default_tip(v: &BrushTip) -> bool {
    matches!(v, BrushTip::Round)
}
fn is_default_mode(v: &BrushBlendMode) -> bool {
    matches!(v, BrushBlendMode::Normal)
}
fn is_default_mask_mode(v: &BrushMaskMode) -> bool {
    matches!(v, BrushMaskMode::Multiply)
}
fn is_default_control(v: &BrushControl) -> bool {
    matches!(v, BrushControl::Off)
}
fn is_default_dynamic(v: &BrushDynamic) -> bool {
    *v == BrushDynamic::default()
}
fn is_default_dual_spacing(v: &f32) -> bool {
    *v == 0.25
}
fn default_dual_spacing() -> f32 {
    0.25
}

fn validate_range_f32(value: f32, min: f32, max: f32, label: &'static str) -> Result<()> {
    if !value.is_finite() || value < min || value > max {
        return Err(Error::Invalid(label));
    }
    Ok(())
}

fn validate_png_b64(width: u16, height: u16, png_b64: &str, max_b64: usize) -> Result<()> {
    if width == 0 || height == 0 || width > MAX_SAMPLED_SIDE || height > MAX_SAMPLED_SIDE {
        return Err(Error::Invalid("brush sampled bitmap dimensions"));
    }
    if png_b64.len() < 16 || png_b64.len() > max_b64 || !png_b64.len().is_multiple_of(4) {
        return Err(Error::Limit("brush sampled bitmap bytes"));
    }
    if !png_b64.starts_with("iVBORw0KGgo") {
        return Err(Error::Invalid("brush sampled bitmap png header"));
    }
    let bytes = png_b64.as_bytes();
    let pad = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if pad > 2 {
        return Err(Error::Invalid("brush sampled bitmap base64 padding"));
    }
    let body = bytes.get(..bytes.len() - pad).ok_or(Error::Invalid("brush sampled bitmap base64"))?;
    if !body.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/') {
        return Err(Error::Invalid("brush sampled bitmap base64"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrushBlendMode {
    #[default]
    Normal,
    Dissolve,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    DarkerColor,
    Lighten,
    Screen,
    ColorDodge,
    LinearDodge,
    LighterColor,
    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
    Reflect,
    Glow,
    Negation,
    Xor,
    PaintNetColorBurn,
    PaintNetColorDodge,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrushControl {
    #[default]
    Off,
    Fade,
    PenPressure,
    PenTilt,
    StylusWheel,
    Rotation,
    InitialDirection,
    Direction,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrushMaskMode {
    #[default]
    Multiply,
    Subtract,
    Darken,
    Overlay,
    ColorDodge,
    ColorBurn,
    LinearBurn,
    HardMix,
    LinearHeight,
    Height,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrushPatternStyle {
    Noise,
    Canvas,
    #[default]
    Paper,
    Dots,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProceduralTipStyle {
    Chalk,
    Charcoal,
    Bristle,
    Rake,
    Sponge,
    Spatter,
    Leaf,
    Grass,
    Star,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrushTip {
    #[default]
    Round,
    Procedural {
        style: ProceduralTipStyle,
        size: u32,
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        seed: u32,
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        param: u32,
    },
    /// Bounded 8-bit grayscale PNG encoded as standard base64 (for user/imported `.abr` tips).
    Sampled { width: u16, height: u16, png_b64: String },
}

impl BrushTip {
    pub fn validate(&self, max_b64: usize) -> Result<()> {
        match self {
            BrushTip::Round => Ok(()),
            BrushTip::Procedural { size, param, .. } => {
                if *size < MIN_PROCEDURAL_SIZE || *size > MAX_PROCEDURAL_SIZE || *param > MAX_PROCEDURAL_PARAM {
                    return Err(Error::Invalid("procedural brush tip parameters"));
                }
                Ok(())
            }
            BrushTip::Sampled { width, height, png_b64 } => validate_png_b64(*width, *height, png_b64, max_b64),
        }
    }
    pub fn is_sampled(&self) -> bool {
        !matches!(self, BrushTip::Round)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrushPattern {
    Procedural {
        style: BrushPatternStyle,
        size: u32,
        #[serde(default, skip_serializing_if = "is_zero_u32")]
        seed: u32,
    },
    Tile {
        width: u16,
        height: u16,
        png_b64: String,
    },
}

impl Default for BrushPattern {
    fn default() -> Self {
        Self::Procedural { style: BrushPatternStyle::Paper, size: 128, seed: 1 }
    }
}

impl BrushPattern {
    pub fn validate(&self, max_b64: usize) -> Result<()> {
        match self {
            BrushPattern::Procedural { size, .. } => {
                if *size < MIN_PROCEDURAL_SIZE || *size > MAX_PROCEDURAL_SIZE {
                    return Err(Error::Invalid("procedural texture pattern size"));
                }
                Ok(())
            }
            BrushPattern::Tile { width, height, png_b64 } => validate_png_b64(*width, *height, png_b64, max_b64),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushDynamic {
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub jitter: f32,
    #[serde(default, skip_serializing_if = "is_default_control")]
    pub control: BrushControl,
    #[serde(default = "default_fade_steps", skip_serializing_if = "is_default_fade_steps")]
    pub fade_steps: u32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub minimum: f32,
    /// Optional non-linear response curve points (`[input, output]` in `0..=1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<Vec<[f32; 2]>>,
}

impl Default for BrushDynamic {
    fn default() -> Self {
        Self { jitter: 0.0, control: BrushControl::Off, fade_steps: default_fade_steps(), minimum: 0.0, curve: None }
    }
}

impl BrushDynamic {
    pub fn validate(&self, max_jitter: f32) -> Result<()> {
        validate_range_f32(self.jitter, 0.0, max_jitter, "brush dynamic jitter")?;
        validate_range_f32(self.minimum, 0.0, 1.0, "brush dynamic minimum")?;
        if self.fade_steps == 0 || self.fade_steps > 100_000 {
            return Err(Error::Invalid("brush fade steps"));
        }
        if let Some(points) = &self.curve {
            if points.len() < 2 || points.len() > MAX_CURVE_POINTS {
                return Err(Error::Invalid("brush response curve points"));
            }
            for point in points {
                validate_range_f32(point[0], 0.0, 1.0, "brush response curve x")?;
                validate_range_f32(point[1], 0.0, 1.0, "brush response curve y")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushShapeDynamics {
    #[serde(default, skip_serializing_if = "is_default_dynamic")]
    pub size: BrushDynamic,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub tilt_scale: f32,
    #[serde(default, skip_serializing_if = "is_default_dynamic")]
    pub angle: BrushDynamic,
    #[serde(default, skip_serializing_if = "is_default_dynamic")]
    pub roundness: BrushDynamic,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_x_jitter: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_y_jitter: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub brush_projection: bool,
}

impl BrushShapeDynamics {
    pub fn validate(&self) -> Result<()> {
        self.size.validate(1.0)?;
        validate_range_f32(self.tilt_scale, 0.0, 2.0, "brush tilt scale")?;
        self.angle.validate(1.0)?;
        self.roundness.validate(1.0)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushScattering {
    #[serde(default, skip_serializing_if = "is_default_dynamic")]
    pub scatter: BrushDynamic,
    #[serde(default, skip_serializing_if = "is_false")]
    pub both_axes: bool,
    #[serde(default = "default_count", skip_serializing_if = "is_default_count")]
    pub count: u32,
    #[serde(default, skip_serializing_if = "is_default_dynamic")]
    pub count_jitter: BrushDynamic,
}

impl Default for BrushScattering {
    fn default() -> Self {
        Self { scatter: BrushDynamic::default(), both_axes: false, count: 1, count_jitter: BrushDynamic::default() }
    }
}

impl BrushScattering {
    pub fn validate(&self) -> Result<()> {
        self.scatter.validate(MAX_SCATTER)?;
        if !(1..=16).contains(&self.count) {
            return Err(Error::Invalid("brush scatter count"));
        }
        self.count_jitter.validate(1.0)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushTexture {
    pub pattern: BrushPattern,
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert: bool,
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub scale: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub brightness: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub contrast: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub each_tip: bool,
    #[serde(default, skip_serializing_if = "is_default_mask_mode")]
    pub mode: BrushMaskMode,
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub depth: f32,
    #[serde(default, skip_serializing_if = "is_default_dynamic")]
    pub depth_jitter: BrushDynamic,
}

impl Default for BrushTexture {
    fn default() -> Self {
        Self {
            pattern: BrushPattern::default(),
            invert: false,
            scale: 1.0,
            brightness: 0.0,
            contrast: 0.0,
            each_tip: false,
            mode: BrushMaskMode::Multiply,
            depth: 1.0,
            depth_jitter: BrushDynamic::default(),
        }
    }
}

impl BrushTexture {
    pub fn validate(&self, max_b64: usize) -> Result<()> {
        self.pattern.validate(max_b64)?;
        validate_range_f32(self.scale, 0.01, 20.0, "brush texture scale")?;
        validate_range_f32(self.brightness, -1.0, 1.0, "brush texture brightness")?;
        validate_range_f32(self.contrast, -1.0, 1.0, "brush texture contrast")?;
        validate_range_f32(self.depth, 0.0, 1.0, "brush texture depth")?;
        self.depth_jitter.validate(1.0)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushDual {
    #[serde(default, skip_serializing_if = "is_default_mask_mode")]
    pub mode: BrushMaskMode,
    #[serde(default, skip_serializing_if = "is_default_tip")]
    pub tip: BrushTip,
    pub size: f32,
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub hardness: f32,
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub roundness: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub angle: f32,
    #[serde(default = "default_dual_spacing", skip_serializing_if = "is_default_dual_spacing")]
    pub spacing: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub scatter: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub both_axes: bool,
    #[serde(default = "default_count", skip_serializing_if = "is_default_count")]
    pub count: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip: bool,
}

impl BrushDual {
    pub fn validate(&self, max_b64: usize) -> Result<()> {
        self.tip.validate(max_b64)?;
        validate_range_f32(self.size, MIN_BRUSH_SIZE, MAX_BRUSH_SIZE, "dual brush size")?;
        validate_range_f32(self.hardness, 0.0, 1.0, "dual brush hardness")?;
        validate_range_f32(self.roundness, 0.01, 1.0, "dual brush roundness")?;
        validate_range_f32(self.angle, -360.0, 360.0, "dual brush angle")?;
        validate_range_f32(self.spacing, MIN_SPACING, MAX_SPACING, "dual brush spacing")?;
        validate_range_f32(self.scatter, 0.0, MAX_SCATTER, "dual brush scatter")?;
        if !(1..=16).contains(&self.count) {
            return Err(Error::Invalid("dual brush count"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushColorDynamics {
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub per_tip: bool,
    #[serde(default, skip_serializing_if = "is_default_dynamic")]
    pub fg_bg: BrushDynamic,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub hue_jitter: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub saturation_jitter: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub brightness_jitter: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub purity: f32,
}

impl Default for BrushColorDynamics {
    fn default() -> Self {
        Self { per_tip: true, fg_bg: BrushDynamic::default(), hue_jitter: 0.0, saturation_jitter: 0.0, brightness_jitter: 0.0, purity: 0.0 }
    }
}

impl BrushColorDynamics {
    pub fn validate(&self) -> Result<()> {
        self.fg_bg.validate(1.0)?;
        validate_range_f32(self.hue_jitter, 0.0, 1.0, "brush hue jitter")?;
        validate_range_f32(self.saturation_jitter, 0.0, 1.0, "brush saturation jitter")?;
        validate_range_f32(self.brightness_jitter, 0.0, 1.0, "brush brightness jitter")?;
        validate_range_f32(self.purity, -1.0, 1.0, "brush purity")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushTransfer {
    #[serde(default, skip_serializing_if = "is_default_dynamic")]
    pub opacity: BrushDynamic,
    #[serde(default, skip_serializing_if = "is_default_dynamic")]
    pub flow: BrushDynamic,
}

impl BrushTransfer {
    pub fn validate(&self) -> Result<()> {
        self.opacity.validate(1.0)?;
        self.flow.validate(1.0)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushPose {
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub tilt_x: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub tilt_y: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub rotation: f32,
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub pressure: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub override_tilt_x: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub override_tilt_y: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub override_rotation: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub override_pressure: bool,
}

impl Default for BrushPose {
    fn default() -> Self {
        Self {
            tilt_x: 0.0,
            tilt_y: 0.0,
            rotation: 0.0,
            pressure: 1.0,
            override_tilt_x: false,
            override_tilt_y: false,
            override_rotation: false,
            override_pressure: false,
        }
    }
}

impl BrushPose {
    pub fn validate(&self) -> Result<()> {
        validate_range_f32(self.tilt_x, -90.0, 90.0, "brush pose tilt_x")?;
        validate_range_f32(self.tilt_y, -90.0, 90.0, "brush pose tilt_y")?;
        validate_range_f32(self.rotation, 0.0, 360.0, "brush pose rotation")?;
        validate_range_f32(self.pressure, 0.0, 1.0, "brush pose pressure")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrushSmoothing {
    pub amount: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pulled_string: bool,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub catch_up: bool,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub catch_up_on_end: bool,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub adjust_for_zoom: bool,
    /// View zoom frozen at stroke start for `pulled_string && adjust_for_zoom`.
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub zoom: f32,
}

impl BrushSmoothing {
    pub fn validate(&self) -> Result<()> {
        validate_range_f32(self.amount, 0.0, 1.0, "brush smoothing amount")?;
        validate_range_f32(self.zoom, 0.01, 1000.0, "brush smoothing zoom")?;
        Ok(())
    }
}

/// Optional per-point sample metadata matching `Stroke::points` 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrokeSample {
    /// Monotonic milliseconds since stroke start (`0.0..=3_600_000.0`).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub time_ms: f64,
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub pressure: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub tilt_x: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub tilt_y: f32,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub rotation: f32,
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub wheel: f32,
}

impl Default for StrokeSample {
    fn default() -> Self {
        Self { time_ms: 0.0, pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0, rotation: 0.0, wheel: 1.0 }
    }
}

impl StrokeSample {
    pub fn is_default_mouse(&self) -> bool {
        *self == Self::default()
    }
    pub fn validate(&self) -> Result<()> {
        if !self.time_ms.is_finite() || self.time_ms < 0.0 || self.time_ms > MAX_STROKE_DURATION_MS {
            return Err(Error::Invalid("stroke sample time"));
        }
        validate_range_f32(self.pressure, 0.0, 1.0, "stroke sample pressure")?;
        validate_range_f32(self.tilt_x, -90.0, 90.0, "stroke sample tilt_x")?;
        validate_range_f32(self.tilt_y, -90.0, 90.0, "stroke sample tilt_y")?;
        validate_range_f32(self.rotation, 0.0, 360.0, "stroke sample rotation")?;
        validate_range_f32(self.wheel, 0.0, 1.0, "stroke sample wheel")?;
        Ok(())
    }
}

/// Complete brush specification frozen on a shared-canvas stroke.
///
/// The brush runs in canonical brush-space where diameter equals `authored_size`
/// (matching `Stroke::width` in region-local units), making dab spacing, scatter,
/// texture tiling, dual brush, pencil pixel-grid, and noise 100% invariant across
/// viewport zoom, pan, DPI, and export resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrokeBrush {
    #[serde(default = "default_schema")]
    pub schema: u16,
    /// Authored brush diameter in canonical brush-space pixels (`0.5..=5000.0`).
    pub authored_size: f32,
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub hardness: f32,
    #[serde(default = "default_spacing", skip_serializing_if = "is_default_spacing")]
    pub spacing: f32,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub spacing_enabled: bool,
    /// Stroke-level opacity ceiling (`0.0..=1.0`), distinct from `flow` and device layer order.
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub opacity: f32,
    /// Per-dab flow (`0.0..=1.0`).
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub flow: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pressure_size: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub pressure_opacity: bool,
    #[serde(default = "default_background", skip_serializing_if = "is_default_background")]
    pub background: [f32; 4],
    #[serde(default, skip_serializing_if = "is_false")]
    pub behind: bool,
    #[serde(default, skip_serializing_if = "is_default_mode")]
    pub mode: BrushBlendMode,
    #[serde(default, skip_serializing_if = "is_default_tip")]
    pub tip: BrushTip,
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub angle: f32,
    #[serde(default = "default_one_f32", skip_serializing_if = "is_one_f32")]
    pub roundness: f32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_x: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub flip_y: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub aliased: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub square: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape_dynamics: Option<BrushShapeDynamics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scattering: Option<BrushScattering>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<BrushTexture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dual_brush: Option<Box<BrushDual>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_dynamics: Option<BrushColorDynamics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<BrushTransfer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pose: Option<BrushPose>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub noise: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub wet_edges: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub build_up: bool,
    #[serde(default = "default_build_up_rate", skip_serializing_if = "is_default_build_up_rate")]
    pub build_up_rate: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<BrushSmoothing>,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub seed: u64,
}

fn is_default_spacing(v: &f32) -> bool {
    *v == default_spacing()
}

impl Default for StrokeBrush {
    fn default() -> Self {
        Self {
            schema: STROKE_BRUSH_SCHEMA,
            authored_size: 20.0,
            hardness: 1.0,
            spacing: default_spacing(),
            spacing_enabled: true,
            opacity: 1.0,
            flow: 1.0,
            pressure_size: false,
            pressure_opacity: false,
            background: default_background(),
            behind: false,
            mode: BrushBlendMode::Normal,
            tip: BrushTip::Round,
            angle: 0.0,
            roundness: 1.0,
            flip_x: false,
            flip_y: false,
            aliased: false,
            square: false,
            shape_dynamics: None,
            scattering: None,
            texture: None,
            dual_brush: None,
            color_dynamics: None,
            transfer: None,
            pose: None,
            noise: false,
            wet_edges: false,
            build_up: false,
            build_up_rate: default_build_up_rate(),
            smoothing: None,
            seed: 0,
        }
    }
}

impl StrokeBrush {
    pub fn validate(&self) -> Result<()> {
        self.validate_with_b64_limit(MAX_SAMPLED_B64_BYTES)
    }

    pub fn validate_preview(&self) -> Result<()> {
        self.validate_with_b64_limit(MAX_PREVIEW_B64_BYTES)
    }

    fn validate_with_b64_limit(&self, max_b64: usize) -> Result<()> {
        if self.schema != STROKE_BRUSH_SCHEMA {
            return Err(Error::Invalid("unsupported brush schema"));
        }
        validate_range_f32(self.authored_size, MIN_BRUSH_SIZE, MAX_BRUSH_SIZE, "brush authored size")?;
        validate_range_f32(self.hardness, 0.0, 1.0, "brush hardness")?;
        validate_range_f32(self.spacing, MIN_SPACING, MAX_SPACING, "brush spacing")?;
        validate_range_f32(self.opacity, 0.0, 1.0, "brush opacity")?;
        validate_range_f32(self.flow, 0.0, 1.0, "brush flow")?;
        for channel in self.background {
            validate_range_f32(channel, 0.0, 1.0, "brush background color")?;
        }
        self.tip.validate(max_b64)?;
        validate_range_f32(self.angle, -360.0, 360.0, "brush angle")?;
        validate_range_f32(self.roundness, 0.01, 1.0, "brush roundness")?;
        if let Some(sd) = &self.shape_dynamics {
            sd.validate()?;
        }
        if let Some(sc) = &self.scattering {
            sc.validate()?;
        }
        if let Some(tx) = &self.texture {
            tx.validate(max_b64)?;
        }
        if let Some(dual) = &self.dual_brush {
            dual.validate(max_b64)?;
        }
        if let Some(cd) = &self.color_dynamics {
            cd.validate()?;
        }
        if let Some(tr) = &self.transfer {
            tr.validate()?;
        }
        if let Some(pose) = &self.pose {
            pose.validate()?;
        }
        validate_range_f32(self.build_up_rate, 0.1, 1000.0, "brush build_up_rate")?;
        if let Some(sm) = &self.smoothing {
            sm.validate()?;
        }
        Ok(())
    }

    /// Worst-case radial reach relative to `stroke.width * 0.5`, accounting for
    /// soft tip Gaussian shoulders, rotated sampled/square tips, and scattering.
    /// Used by `render::spatial_cells` so halos and scattered dabs never clip at
    /// region seams.
    pub fn reach_factor(&self) -> f64 {
        let tip_ratio = if self.tip.is_sampled() || (self.aliased && self.square) {
            std::f64::consts::SQRT_2
        } else if self.aliased {
            1.0
        } else {
            // Matches photocraft-paint's `TIP_SHAPE` table: worst-case soft tip
            // (hardness 0) reaches `2.6 * 0.664 = 1.7264` nominal radii.
            let h = if self.hardness.is_finite() { f64::from(self.hardness.clamp(0.0, 1.0)) } else { 0.0 };
            const TABLE: [(f64, f64); 11] = [
                (0.00, 0.664),
                (0.14, 0.58),
                (0.28, 0.50),
                (0.40, 0.44),
                (0.52, 0.36),
                (0.63, 0.29),
                (0.70, 0.24),
                (0.78, 0.19),
                (0.84, 0.15),
                (0.89, 0.11),
                (1.00, 0.0),
            ];
            let scaled = h * 10.0;
            let idx = (scaled.floor() as usize).min(9);
            let frac = scaled - idx as f64;
            let a = TABLE.get(idx).copied().unwrap_or((0.0, 0.664));
            let b = TABLE.get(idx + 1).copied().unwrap_or((1.0, 0.0));
            let r0 = a.0 + (b.0 - a.0) * frac;
            let s = a.1 + (b.1 - a.1) * frac;
            let authored_r = if self.authored_size.is_finite() { f64::from(self.authored_size).max(0.5) * 0.5 } else { 0.5 };
            let edge = (1.2 / authored_r).min(0.3).max(0.25 / authored_r);
            let core = r0.min((1.0 - edge).max(0.0));
            let shoulder = s.max(edge);
            (core + 2.6 * shoulder).max(1.0)
        };
        let scatter_ratio = self.scattering.as_ref().map_or(0.0, |sc| {
            let amt = if sc.scatter.jitter.is_finite() { f64::from(sc.scatter.jitter.clamp(0.0, MAX_SCATTER)) } else { 0.0 };
            if sc.both_axes { amt * std::f64::consts::SQRT_2 } else { amt }
        });
        (tip_ratio + scatter_ratio + 0.1).max(1.0)
    }
}
