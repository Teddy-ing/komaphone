//! Normalize the CPU composite to sRGB before canonical, metadata-free party encoding.
//!
//! The compositor retains RGB document values and gray tone curves; CMYK and Lab
//! composites have already become sRGB. Convert float samples before quantization,
//! without changing the editable checkpoint, alpha, or monitor-dependent previews.

use photocraft_cms::{Builtin, CmsError, ColorSpace, Intent, Profile, transform};
use photocraft_doc::{ColorMode, Document};

fn document_profile(doc: &Document, space: ColorSpace) -> Profile {
    doc.icc_profile
        .as_ref()
        .and_then(|bytes| Profile::parse(bytes).ok())
        .filter(|profile| profile.color_space == space)
        .unwrap_or_else(|| photocraft_cms::builtin::default_for(space).unwrap_or(Builtin::Srgb.profile()).clone())
}

fn composite_profile(doc: &Document) -> Profile {
    match doc.mode {
        ColorMode::Grayscale | ColorMode::Bitmap | ColorMode::Duotone => document_profile(doc, ColorSpace::Gray)
            .gray_as_rgb()
            .or_else(|| Builtin::SGray.profile().gray_as_rgb())
            .unwrap_or_else(|| Builtin::Srgb.profile().clone()),
        ColorMode::Cmyk | ColorMode::Lab => Builtin::Srgb.profile().clone(),
        _ => document_profile(doc, ColorSpace::Rgb),
    }
}

/// Convert straight-alpha CPU composite samples to sRGB, preserving alpha exactly.
///
/// Source-profile fallbacks match the editor's composite display policy. The shared
/// conversion is independent of the monitor profile, proof settings, and sample depth.
pub fn to_srgb(doc: &Document, pixels: &mut [[f32; 4]]) -> Result<(), CmsError> {
    let source = composite_profile(doc);
    let destination = Builtin::Srgb.profile();
    // Content hashes are cache hints, not identity proofs for untrusted ICC data.
    // Keep the common sRGB path bit-exact without trusting a hash collision.
    if source.content_hash() == destination.content_hash() && source.to_bytes() == destination.to_bytes() {
        return Ok(());
    }
    let conversion =
        transform::cached(&source, destination, transform::TransformOptions { intent: Intent::RelativeColorimetric, bpc: true, precise_float: true })?;
    conversion.apply(pixels.as_flattened_mut(), 4);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_cms::Transform;
    use photocraft_doc::{Color, SampleType, Size};

    fn document(mode: ColorMode, depth: SampleType) -> Document {
        Document::with_background("Canonical color fixture", Size { width: 1, height: 1 }, mode, depth, Color::rgba(0.5, 0.5, 0.5, 0.25))
    }

    fn code(value: f32) -> u8 {
        (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    }

    #[test]
    fn linear_rgb_is_encoded_before_quantization_at_every_depth() {
        for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let mut doc = document(ColorMode::Rgb, depth);
            doc.icc_profile = Some(Builtin::LinearSrgb.profile().to_bytes());
            let original = doc.layers.first().unwrap().surface().unwrap().rgba(0, 0);
            let mut pixels = [original];
            to_srgb(&doc, &mut pixels).unwrap();
            assert_eq!(code(original[0]), 128, "{depth:?}: old raw quantization");
            for value in &pixels[0][..3] {
                assert_eq!(code(*value), 188, "{depth:?}: linear midpoint must become sRGB");
            }
            assert_eq!(pixels[0][3].to_bits(), original[3].to_bits());
            assert_eq!(doc.layers.first().unwrap().surface().unwrap().rgba(0, 0), original, "Native checkpoint is unchanged");
        }
    }

    #[test]
    fn gray_uses_embedded_curve_and_the_editors_untagged_working_curve() {
        let mut doc = document(ColorMode::Grayscale, SampleType::F32);
        for profile in [None, Some(Builtin::DotGain20Compat.profile().to_bytes())] {
            doc.icc_profile = profile;
            let mut pixels = [[0.5, 0.5, 0.5, 0.37]];
            to_srgb(&doc, &mut pixels).unwrap();
            assert_eq!(code(pixels[0][0]), 149);
            assert!((pixels[0][0] - pixels[0][1]).abs() < 1e-4);
            assert!((pixels[0][1] - pixels[0][2]).abs() < 1e-4);
            assert_eq!(pixels[0][3], 0.37);
        }
        doc.icc_profile = Some(Builtin::SGray.profile().to_bytes());
        let mut pixels = [[0.5, 0.5, 0.5, 0.37]];
        to_srgb(&doc, &mut pixels).unwrap();
        assert_eq!(code(pixels[0][0]), 128);
    }

    #[test]
    fn p3_nonneutral_composite_matches_cms_and_keeps_alpha() {
        let mut doc = document(ColorMode::Rgb, SampleType::U16);
        doc.icc_profile = Some(Builtin::DisplayP3.profile().to_bytes());
        let original = [0.7, 0.3, 0.2, 0.42];
        let mut pixels = [original];
        let stored_profile = Profile::parse(doc.icc_profile.as_ref().unwrap()).unwrap();
        let reference = Transform::new(&stored_profile, Builtin::Srgb.profile(), Intent::RelativeColorimetric, true).unwrap();
        let mut expected = [0.0; 3];
        reference.eval(&original, &mut expected);
        to_srgb(&doc, &mut pixels).unwrap();
        assert!((pixels[0][0] - original[0]).abs() > 0.01);
        for (actual, expected) in pixels[0][..3].iter().zip(expected) {
            assert!((*actual - expected).abs() < 1e-5);
        }
        assert_eq!(pixels[0][3].to_bits(), original[3].to_bits());
    }

    #[test]
    fn cmyk_and_lab_composites_are_not_converted_twice() {
        let original = [[0.17, 0.61, 0.88, 0.33], [0.4, 0.3, 0.2, 0.0]];
        for (mode, profile) in [(ColorMode::Cmyk, Builtin::CoatedCmyk), (ColorMode::Lab, Builtin::LabD50)] {
            let mut doc = document(mode, SampleType::F32);
            doc.icc_profile = Some(profile.profile().to_bytes());
            let mut pixels = original;
            to_srgb(&doc, &mut pixels).unwrap();
            assert_eq!(pixels, original, "{mode:?}: compositor already returned sRGB");
        }
    }

    #[test]
    fn srgb_and_invalid_or_mismatched_rgb_profiles_keep_default_pixels_exact() {
        let original = [[0.12345, 0.98765, 1.25, 0.23456]];
        let mut doc = document(ColorMode::Rgb, SampleType::F32);
        for profile in [None, Some(Builtin::Srgb.profile().to_bytes()), Some(std::sync::Arc::new(vec![0, 1, 2])), Some(Builtin::SGray.profile().to_bytes())] {
            doc.icc_profile = profile;
            let mut pixels = original;
            to_srgb(&doc, &mut pixels).unwrap();
            assert_eq!(pixels, original);
        }
        to_srgb(&doc, &mut []).unwrap();
    }
}
