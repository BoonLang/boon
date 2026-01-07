//! Pixel difference comparison using SSIM (Structural Similarity Index)
//!
//! Compares two images and returns exit code 0 if SSIM >= threshold, 1 otherwise.
//! Optionally generates a diff visualization image.

use anyhow::{Context, Result};
use image::{GenericImage, GenericImageView, GrayImage};
use image_compare::Algorithm;
use std::path::Path;

/// Compare two images using SSIM and optionally generate a diff image.
///
/// # Arguments
/// * `reference` - Path to the reference image
/// * `current` - Path to the current/candidate image
/// * `output` - Optional path to save the diff visualization
/// * `threshold` - SSIM threshold (0.0 to 1.0), comparison passes if score >= threshold
///
/// # Returns
/// * `Ok(())` if SSIM >= threshold
/// * `Err` if SSIM < threshold or if any error occurs
pub fn run(reference: &str, current: &str, output: Option<&str>, threshold: f64) -> Result<()> {
    let ref_img = image::open(Path::new(reference))
        .with_context(|| format!("Failed to open reference image: {}", reference))?;
    let cur_img = image::open(Path::new(current))
        .with_context(|| format!("Failed to open current image: {}", current))?;

    let (ref_w, ref_h) = ref_img.dimensions();
    let (cur_w, cur_h) = cur_img.dimensions();

    if ref_img.dimensions() != cur_img.dimensions() {
        anyhow::bail!(
            "Dimension mismatch: reference {}x{}, current {}x{}",
            ref_w, ref_h, cur_w, cur_h
        );
    }

    // Convert to grayscale for SSIM comparison
    let ref_gray: GrayImage = ref_img.to_luma8();
    let cur_gray: GrayImage = cur_img.to_luma8();

    // Calculate SSIM using image_compare crate
    let result = image_compare::gray_similarity_structure(
        &Algorithm::MSSIMSimple,
        &ref_gray,
        &cur_gray,
    )
    .map_err(|e| anyhow::anyhow!("SSIM calculation failed: {:?}", e))?;

    let score = result.score;
    println!("SSIM: {:.4} (threshold: {:.4})", score, threshold);

    if score < threshold {
        // Generate diff visualization if output path provided
        if let Some(out) = output {
            generate_diff_image(&ref_img, &cur_img, out)?;
            println!("Diff image saved to: {}", out);
        }

        println!("FAIL: SSIM {:.4} is below threshold {:.4}", score, threshold);
        anyhow::bail!("SSIM below threshold");
    }

    println!("PASS: SSIM {:.4} meets threshold {:.4}", score, threshold);
    Ok(())
}

/// Generate a diff visualization image highlighting differences between two images.
///
/// The diff image shows:
/// - Areas with small differences: darkened original image (provides context)
/// - Areas with significant differences: amplified color difference (highlights issues)
fn generate_diff_image(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    output_path: &str,
) -> Result<()> {
    let (w, h) = ref_img.dimensions();
    let mut diff = image::DynamicImage::new_rgba8(w, h);

    for y in 0..h {
        for x in 0..w {
            let a = ref_img.get_pixel(x, y);
            let b = cur_img.get_pixel(x, y);

            // Calculate per-channel color difference
            let dr = (a[0] as i32 - b[0] as i32).unsigned_abs() as u8;
            let dg = (a[1] as i32 - b[1] as i32).unsigned_abs() as u8;
            let db = (a[2] as i32 - b[2] as i32).unsigned_abs() as u8;

            // Amplify difference by 3x for visibility
            let amp = 3u8;
            let dr_amp = dr.saturating_mul(amp);
            let dg_amp = dg.saturating_mul(amp);
            let db_amp = db.saturating_mul(amp);

            // If difference is small (< 30 per channel), show darkened original for context
            // Otherwise show amplified difference in color
            if dr < 30 && dg < 30 && db < 30 {
                diff.put_pixel(x, y, image::Rgba([a[0] / 4, a[1] / 4, a[2] / 4, 255]));
            } else {
                diff.put_pixel(x, y, image::Rgba([dr_amp, dg_amp, db_amp, 255]));
            }
        }
    }

    diff.save(output_path)
        .with_context(|| format!("Failed to save diff image: {}", output_path))?;

    Ok(())
}

/// Simplified comparison without diff output
pub fn run_simple(reference: &str, current: &str, threshold: f64) -> Result<()> {
    run(reference, current, None, threshold)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_images_pass() {
        // This would need actual test images - placeholder for now
        // The SSIM of identical images should be 1.0
    }
}
