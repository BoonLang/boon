//! Pixel difference comparison using SSIM (Structural Similarity Index)
//!
//! Compares two images and returns exit code 0 if SSIM >= threshold, 1 otherwise.
//! Provides detailed spatial analysis including:
//! - Region-based grid analysis (7x7 by default)
//! - Line-based diff detection (which rows have differences)
//! - Bounding box of all differences
//! - Optional JSON output for programmatic analysis
//! - Optional visual overlays (grid, heatmap, composite)

use anyhow::{Context, Result};
use image::{GenericImage, GenericImageView, GrayImage, Rgba, RgbaImage};
use image_compare::Algorithm;
use std::path::Path;

/// Region statistics for a grid cell
#[derive(Debug, Clone)]
pub struct RegionReport {
    pub row: u32,
    pub col: u32,
    pub diff_pixel_count: u32,
    pub diff_percentage: f32,
    pub max_delta: u8,
    pub dominant_channel: char,
    pub total_delta: u64,
}

/// Bounding box of differences
#[derive(Debug, Clone, Default)]
pub struct BoundingBox {
    pub x1: u32,
    pub y1: u32,
    pub x2: u32,
    pub y2: u32,
}

impl BoundingBox {
    pub fn width(&self) -> u32 {
        if self.x2 > self.x1 { self.x2 - self.x1 } else { 0 }
    }

    pub fn height(&self) -> u32 {
        if self.y2 > self.y1 { self.y2 - self.y1 } else { 0 }
    }

    pub fn area(&self) -> u32 {
        self.width() * self.height()
    }
}

/// Line range representing a continuous band of differences
#[derive(Debug, Clone)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

// ============================================================================
// SEMANTIC ANALYSIS STRUCTURES (Phase 7)
// ============================================================================

/// Color shift analysis - detects systematic color differences
#[derive(Debug, Clone, Default)]
pub struct ColorShiftAnalysis {
    /// Average R channel delta (positive = current is more red)
    pub avg_r_delta: f32,
    /// Average G channel delta (positive = current is more green)
    pub avg_g_delta: f32,
    /// Average B channel delta (positive = current is more blue)
    pub avg_b_delta: f32,
    /// Number of pixels with color differences
    pub affected_pixels: u32,
    /// Percentage of total pixels affected
    pub affected_percentage: f32,
    /// Interpretation of the color shift
    pub interpretation: String,
    /// Average perceptual difference in LAB ΔE
    pub avg_delta_e: f32,
}

/// Position shift analysis - detects elements that have moved
#[derive(Debug, Clone, Default)]
pub struct PositionShiftAnalysis {
    /// Detected horizontal offset (pixels)
    pub offset_x: i32,
    /// Detected vertical offset (pixels)
    pub offset_y: i32,
    /// Confidence score (0.0-1.0)
    pub confidence: f32,
    /// Percentage of diff pixels explained by this shift (for JSON output)
    #[allow(dead_code)]
    pub explained_percentage: f32,
}

/// Font change analysis - detects font family/style changes
#[derive(Debug, Clone, Default)]
pub struct FontChangeAnalysis {
    /// Whether reference appears to use cursive/script font
    pub ref_appears_cursive: bool,
    /// Whether current appears to use cursive/script font
    pub cur_appears_cursive: bool,
    /// Edge variance ratio (higher = more curves/decoration)
    pub ref_edge_variance: f32,
    /// Edge variance ratio for current
    pub cur_edge_variance: f32,
    /// Confidence score (0.0-1.0)
    pub confidence: f32,
}

/// Size change analysis - detects scaling differences
#[derive(Debug, Clone, Default)]
pub struct SizeChangeAnalysis {
    /// Detected scale factor (current / reference)
    pub scale_factor: f32,
    /// Confidence score (0.0-1.0)
    pub confidence: f32,
    /// Percentage of diff pixels explained by size change (for JSON output)
    #[allow(dead_code)]
    pub explained_percentage: f32,
}

/// Complete semantic analysis result
#[derive(Debug, Clone, Default)]
pub struct SemanticAnalysis {
    pub color_shift: Option<ColorShiftAnalysis>,
    pub position_shift: Option<PositionShiftAnalysis>,
    pub font_change: Option<FontChangeAnalysis>,
    pub size_change: Option<SizeChangeAnalysis>,
    pub recommendations: Vec<String>,
}

/// Complete analysis report
#[derive(Debug, Clone)]
pub struct DiffAnalysis {
    pub ssim: f64,
    pub threshold: f64,
    pub passed: bool,
    pub total_diff_pixels: u32,
    pub diff_percentage: f32,
    pub bounding_box: Option<BoundingBox>,
    pub regions: Vec<RegionReport>,
    pub affected_lines: Vec<u32>,
    pub dense_bands: Vec<LineRange>,
    pub grid_size: u32,
    pub image_width: u32,
    pub image_height: u32,
    /// Semantic analysis results (Phase 7)
    pub semantic: Option<SemanticAnalysis>,
}

/// Output options for pixel diff
#[derive(Debug, Clone, Default)]
pub struct OutputOptions {
    pub diff_path: Option<String>,
    pub json: bool,
    pub grid: bool,
    pub heatmap: bool,
    pub composite: bool,
    pub zoom_region: Option<String>,
    pub zoom_scale: u32,
    pub analyze_semantic: bool,
}

const DIFF_THRESHOLD: u8 = 30; // Per-channel threshold for "significant" difference
const DEFAULT_GRID_SIZE: u32 = 7;

/// Compare two images using SSIM with full spatial analysis.
#[allow(dead_code)]
pub fn run(reference: &str, current: &str, output: Option<&str>, threshold: f64) -> Result<()> {
    run_with_options(reference, current, threshold, OutputOptions {
        diff_path: output.map(String::from),
        ..Default::default()
    })
}

/// Compare two images with full options.
pub fn run_with_options(
    reference: &str,
    current: &str,
    threshold: f64,
    options: OutputOptions,
) -> Result<()> {
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

    // Calculate SSIM
    let result = image_compare::gray_similarity_structure(
        &Algorithm::MSSIMSimple,
        &ref_gray,
        &cur_gray,
    )
    .map_err(|e| anyhow::anyhow!("SSIM calculation failed: {:?}", e))?;

    let ssim = result.score;
    let passed = ssim >= threshold;

    // Perform full spatial analysis
    let mut analysis = analyze_differences(&ref_img, &cur_img, ssim, threshold, DEFAULT_GRID_SIZE);

    // Run semantic analysis if requested
    if options.analyze_semantic {
        let semantic = run_semantic_analysis(&ref_img, &cur_img, &analysis);
        analysis.semantic = Some(semantic);
    }

    // Output results based on options
    if options.json {
        print_json_report(&analysis);
    } else {
        print_console_report(&analysis);
    }

    // Handle zoom region if specified
    if let Some(ref region_str) = options.zoom_region {
        let (row, col) = parse_region_string(region_str)?;
        let output_path = options.diff_path.as_ref()
            .map(|p| p.clone())
            .unwrap_or_else(|| format!("/tmp/boon-visual-debug/zoom_{}_{}.png", row, col));

        generate_zoom_region(
            &ref_img,
            &cur_img,
            row,
            col,
            DEFAULT_GRID_SIZE,
            options.zoom_scale,
            &output_path,
            &analysis,
        )?;
    }
    // Generate diff visualizations if needed (skip if zoom_region was specified)
    else if !passed || options.diff_path.is_some() || options.grid || options.heatmap || options.composite {
        if let Some(ref path) = options.diff_path {
            if options.heatmap {
                generate_heatmap(&ref_img, &cur_img, path, &analysis)?;
            } else if options.grid {
                generate_grid_diff(&ref_img, &cur_img, path, &analysis)?;
            } else if options.composite {
                generate_composite(&ref_img, &cur_img, path)?;
            } else {
                generate_diff_image(&ref_img, &cur_img, path)?;
            }
            if !options.json {
                println!("Diff image saved to: {}", path);
            }
        }
    }

    if !passed {
        anyhow::bail!("SSIM below threshold");
    }

    Ok(())
}

/// Analyze differences between two images with full spatial metrics.
fn analyze_differences(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    ssim: f64,
    threshold: f64,
    grid_size: u32,
) -> DiffAnalysis {
    let (w, h) = ref_img.dimensions();
    let cell_w = w / grid_size;
    let cell_h = h / grid_size;

    // Initialize region data
    let mut regions: Vec<RegionReport> = (0..grid_size)
        .flat_map(|row| (0..grid_size).map(move |col| RegionReport {
            row,
            col,
            diff_pixel_count: 0,
            diff_percentage: 0.0,
            max_delta: 0,
            dominant_channel: ' ',
            total_delta: 0,
        }))
        .collect();

    // Track line differences
    let mut line_has_diff = vec![false; h as usize];

    // Track bounding box
    let mut min_x = w;
    let mut max_x = 0u32;
    let mut min_y = h;
    let mut max_y = 0u32;

    let mut total_diff_pixels = 0u32;

    // Track channel totals for each region
    let mut region_channel_totals: Vec<[u64; 3]> = vec![[0, 0, 0]; (grid_size * grid_size) as usize];

    // Scan all pixels
    for y in 0..h {
        for x in 0..w {
            let a = ref_img.get_pixel(x, y);
            let b = cur_img.get_pixel(x, y);

            let dr = (a[0] as i32 - b[0] as i32).unsigned_abs() as u8;
            let dg = (a[1] as i32 - b[1] as i32).unsigned_abs() as u8;
            let db = (a[2] as i32 - b[2] as i32).unsigned_abs() as u8;

            // Check if this pixel has significant difference
            if dr >= DIFF_THRESHOLD || dg >= DIFF_THRESHOLD || db >= DIFF_THRESHOLD {
                total_diff_pixels += 1;
                line_has_diff[y as usize] = true;

                // Update bounding box
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);

                // Update region stats
                let region_row = (y / cell_h).min(grid_size - 1);
                let region_col = (x / cell_w).min(grid_size - 1);
                let region_idx = (region_row * grid_size + region_col) as usize;

                let region = &mut regions[region_idx];
                region.diff_pixel_count += 1;

                let max_ch = dr.max(dg).max(db);
                region.max_delta = region.max_delta.max(max_ch);
                region.total_delta += dr as u64 + dg as u64 + db as u64;

                // Track channel totals
                region_channel_totals[region_idx][0] += dr as u64;
                region_channel_totals[region_idx][1] += dg as u64;
                region_channel_totals[region_idx][2] += db as u64;
            }
        }
    }

    // Calculate percentages and dominant channels
    let cell_pixels = cell_w * cell_h;
    for (idx, region) in regions.iter_mut().enumerate() {
        region.diff_percentage = (region.diff_pixel_count as f32 / cell_pixels as f32) * 100.0;

        let channels = &region_channel_totals[idx];
        region.dominant_channel = if channels[0] >= channels[1] && channels[0] >= channels[2] {
            'R'
        } else if channels[1] >= channels[0] && channels[1] >= channels[2] {
            'G'
        } else {
            'B'
        };
    }

    // Collect affected lines
    let affected_lines: Vec<u32> = line_has_diff
        .iter()
        .enumerate()
        .filter_map(|(i, &has_diff)| if has_diff { Some(i as u32) } else { None })
        .collect();

    // Find dense bands (continuous sequences of differing lines)
    let dense_bands = find_dense_bands(&affected_lines);

    // Build bounding box
    let bounding_box = if total_diff_pixels > 0 {
        Some(BoundingBox {
            x1: min_x,
            y1: min_y,
            x2: max_x + 1,
            y2: max_y + 1,
        })
    } else {
        None
    };

    DiffAnalysis {
        ssim,
        threshold,
        passed: ssim >= threshold,
        total_diff_pixels,
        diff_percentage: (total_diff_pixels as f32 / (w * h) as f32) * 100.0,
        bounding_box,
        regions,
        affected_lines,
        dense_bands,
        grid_size,
        image_width: w,
        image_height: h,
        semantic: None, // Populated separately if --analyze-semantic is used
    }
}

// ============================================================================
// SEMANTIC ANALYSIS FUNCTIONS (Phase 7)
// ============================================================================

/// Run complete semantic analysis on two images.
pub fn run_semantic_analysis(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    basic_analysis: &DiffAnalysis,
) -> SemanticAnalysis {
    let mut semantic = SemanticAnalysis::default();
    let mut recommendations = Vec::new();

    // Phase 7a: Color shift detection
    if let Some(color_analysis) = detect_color_shift(ref_img, cur_img, basic_analysis) {
        if color_analysis.affected_percentage >= 1.0 {
            recommendations.push(format!(
                "COLOR_SHIFT: {} - check CSS color properties (delta R={:+.0}, G={:+.0}, B={:+.0})",
                color_analysis.interpretation,
                color_analysis.avg_r_delta,
                color_analysis.avg_g_delta,
                color_analysis.avg_b_delta,
            ));
        }
        semantic.color_shift = Some(color_analysis);
    }

    // Phase 7b: Position shift detection
    if let Some(pos_analysis) = detect_position_shift(ref_img, cur_img, basic_analysis) {
        if pos_analysis.confidence >= 0.5 && (pos_analysis.offset_x != 0 || pos_analysis.offset_y != 0) {
            recommendations.push(format!(
                "POSITION_SHIFT: {}px horizontal, {}px vertical - check CSS margin/padding/transform",
                pos_analysis.offset_x, pos_analysis.offset_y
            ));
        }
        semantic.position_shift = Some(pos_analysis);
    }

    // Phase 7c: Font change detection
    if let Some(font_analysis) = detect_font_change(ref_img, cur_img, basic_analysis) {
        if font_analysis.confidence >= 0.5 {
            let ref_style = if font_analysis.ref_appears_cursive { "cursive/script" } else { "sans-serif" };
            let cur_style = if font_analysis.cur_appears_cursive { "cursive/script" } else { "sans-serif" };
            if font_analysis.ref_appears_cursive != font_analysis.cur_appears_cursive {
                recommendations.push(format!(
                    "FONT_CHANGE: Reference uses {}, current uses {} - check font-family loading",
                    ref_style, cur_style
                ));
            }
        }
        semantic.font_change = Some(font_analysis);
    }

    // Phase 7d: Size change detection
    if let Some(size_analysis) = detect_size_change(ref_img, cur_img, basic_analysis) {
        if size_analysis.confidence >= 0.5 && (size_analysis.scale_factor < 0.95 || size_analysis.scale_factor > 1.05) {
            let direction = if size_analysis.scale_factor < 1.0 { "smaller" } else { "larger" };
            recommendations.push(format!(
                "SIZE_CHANGE: Current is {:.0}% {} than reference - check font-size/zoom",
                ((1.0 - size_analysis.scale_factor).abs() * 100.0), direction
            ));
        }
        semantic.size_change = Some(size_analysis);
    }

    semantic.recommendations = recommendations;
    semantic
}

/// Detect font changes using edge variance analysis.
///
/// LIMITATIONS: This algorithm detects cursive/script vs sans-serif fonts
/// based on edge complexity. It CANNOT reliably detect:
/// - Italic vs regular (same edge complexity, just slanted)
/// - Font size differences (use SIZE_CHANGE for that)
/// - Subtle font-family changes within the same category
///
/// Always verify font issues visually using --zoom-region on hot spots.
fn detect_font_change(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    basic_analysis: &DiffAnalysis,
) -> Option<FontChangeAnalysis> {
    // Only analyze if there are differences
    if basic_analysis.total_diff_pixels == 0 {
        return None;
    }

    // Need a bounding box to focus analysis
    let bbox = basic_analysis.bounding_box.as_ref()?;

    // Skip if bounding box is too small
    if bbox.width() < 50 || bbox.height() < 20 {
        return None;
    }

    // Convert to grayscale
    let ref_gray = ref_img.to_luma8();
    let cur_gray = cur_img.to_luma8();

    // Compute edge variance for both images in the difference region
    let ref_variance = compute_edge_variance(&ref_gray, bbox);
    let cur_variance = compute_edge_variance(&cur_gray, bbox);

    // Thresholds determined empirically:
    // - Cursive/script fonts typically have variance > 50
    // - Sans-serif fonts typically have variance < 30
    // NOTE: Italic text has similar variance to regular - cannot detect italic!
    const CURSIVE_THRESHOLD: f32 = 40.0;

    let ref_appears_cursive = ref_variance > CURSIVE_THRESHOLD;
    let cur_appears_cursive = cur_variance > CURSIVE_THRESHOLD;

    // Confidence based on how different the variances are
    // CONSERVATIVE: Only report high confidence for large variance differences
    let variance_diff = (ref_variance - cur_variance).abs();
    let confidence = if variance_diff > 30.0 && (ref_appears_cursive != cur_appears_cursive) {
        // Only high confidence if we crossed the cursive threshold
        (variance_diff / 60.0).min(0.9)
    } else if variance_diff > 15.0 {
        0.4 // Medium-low - might be font change, verify visually
    } else {
        0.1 // Very low - probably not a font family change
    };

    Some(FontChangeAnalysis {
        ref_appears_cursive,
        cur_appears_cursive,
        ref_edge_variance: ref_variance,
        cur_edge_variance: cur_variance,
        confidence,
    })
}

/// Compute edge variance in a region using Sobel-like gradients.
///
/// Higher variance indicates more complex/curved edges (cursive fonts).
/// Lower variance indicates straighter, more uniform edges (sans-serif).
fn compute_edge_variance(gray: &GrayImage, bbox: &BoundingBox) -> f32 {
    let (w, h) = gray.dimensions();

    // Clamp bbox to image bounds
    let x1 = bbox.x1.min(w - 2);
    let y1 = bbox.y1.min(h - 2);
    let x2 = bbox.x2.min(w - 1);
    let y2 = bbox.y2.min(h - 1);

    if x2 <= x1 + 2 || y2 <= y1 + 2 {
        return 0.0;
    }

    let mut gradient_magnitudes: Vec<f32> = Vec::new();

    // Compute gradient magnitude at each pixel using Sobel-like kernel
    for y in (y1 + 1)..(y2 - 1) {
        for x in (x1 + 1)..(x2 - 1) {
            // Simplified Sobel gradient (horizontal and vertical)
            let gx = gray.get_pixel(x + 1, y)[0] as i32 - gray.get_pixel(x - 1, y)[0] as i32;
            let gy = gray.get_pixel(x, y + 1)[0] as i32 - gray.get_pixel(x, y - 1)[0] as i32;

            let magnitude = ((gx * gx + gy * gy) as f32).sqrt();

            // Only count significant edges (text vs background)
            if magnitude > 10.0 {
                gradient_magnitudes.push(magnitude);
            }
        }
    }

    if gradient_magnitudes.is_empty() {
        return 0.0;
    }

    // Compute variance of gradient magnitudes
    let mean: f32 = gradient_magnitudes.iter().sum::<f32>() / gradient_magnitudes.len() as f32;
    let variance: f32 = gradient_magnitudes.iter()
        .map(|&m| (m - mean) * (m - mean))
        .sum::<f32>() / gradient_magnitudes.len() as f32;

    variance.sqrt() // Return standard deviation for easier interpretation
}

/// Detect size changes using edge density comparison.
///
/// When fonts are scaled, edge density changes proportionally.
fn detect_size_change(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    basic_analysis: &DiffAnalysis,
) -> Option<SizeChangeAnalysis> {
    // Only analyze if there are differences
    if basic_analysis.total_diff_pixels == 0 {
        return None;
    }

    // Need a bounding box to focus analysis
    let bbox = basic_analysis.bounding_box.as_ref()?;

    // Skip if bounding box is too small
    if bbox.width() < 50 || bbox.height() < 20 {
        return None;
    }

    // Convert to grayscale
    let ref_gray = ref_img.to_luma8();
    let cur_gray = cur_img.to_luma8();

    // Count edge pixels in each image
    let ref_edges = count_edge_pixels(&ref_gray, bbox);
    let cur_edges = count_edge_pixels(&cur_gray, bbox);

    if ref_edges == 0 {
        return None;
    }

    // Scale factor estimation based on edge density
    // If current has fewer edges per area, it might be smaller (or vice versa)
    let scale_factor = (cur_edges as f32) / (ref_edges as f32);

    // Confidence based on edge difference
    let edge_diff_pct = ((cur_edges as f32 - ref_edges as f32) / ref_edges as f32).abs();
    let confidence = if edge_diff_pct > 0.1 {
        (edge_diff_pct * 5.0).min(1.0)
    } else {
        0.2
    };

    // Estimate how many diff pixels this explains
    let explained_pct = if confidence > 0.5 {
        (edge_diff_pct * 100.0).min(80.0)
    } else {
        0.0
    };

    Some(SizeChangeAnalysis {
        scale_factor,
        confidence,
        explained_percentage: explained_pct,
    })
}

/// Count edge pixels in a region.
fn count_edge_pixels(gray: &GrayImage, bbox: &BoundingBox) -> u32 {
    let (w, h) = gray.dimensions();

    let x1 = bbox.x1.min(w - 2);
    let y1 = bbox.y1.min(h - 2);
    let x2 = bbox.x2.min(w - 1);
    let y2 = bbox.y2.min(h - 1);

    if x2 <= x1 + 2 || y2 <= y1 + 2 {
        return 0;
    }

    let mut count = 0u32;

    for y in (y1 + 1)..(y2 - 1) {
        for x in (x1 + 1)..(x2 - 1) {
            let gx = gray.get_pixel(x + 1, y)[0] as i32 - gray.get_pixel(x - 1, y)[0] as i32;
            let gy = gray.get_pixel(x, y + 1)[0] as i32 - gray.get_pixel(x, y - 1)[0] as i32;

            let magnitude = ((gx * gx + gy * gy) as f32).sqrt();

            if magnitude > 30.0 {
                count += 1;
            }
        }
    }

    count
}

/// Detect position shifts using normalized cross-correlation.
///
/// Searches for the offset that best aligns the current image with the reference
/// by computing correlation scores at different displacements.
fn detect_position_shift(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    basic_analysis: &DiffAnalysis,
) -> Option<PositionShiftAnalysis> {
    // Only analyze if there are differences
    if basic_analysis.total_diff_pixels == 0 {
        return None;
    }

    // Use bounding box to focus analysis on the area with differences
    let bbox = basic_analysis.bounding_box.as_ref()?;

    // Skip if bounding box is too small or too large
    let bbox_area = bbox.area();
    let image_area = basic_analysis.image_width * basic_analysis.image_height;
    if bbox_area < 100 || bbox_area > image_area / 2 {
        return None;
    }

    // Convert to grayscale for correlation
    let ref_gray = ref_img.to_luma8();
    let cur_gray = cur_img.to_luma8();

    // Extract the region around the bounding box with padding
    let padding = 20;
    let roi_x = bbox.x1.saturating_sub(padding);
    let roi_y = bbox.y1.saturating_sub(padding);
    let roi_w = (bbox.width() + 2 * padding).min(basic_analysis.image_width - roi_x);
    let roi_h = (bbox.height() + 2 * padding).min(basic_analysis.image_height - roi_y);

    // Search range for offset detection
    const MAX_OFFSET: i32 = 15;

    let (best_offset, best_score) = find_best_offset(
        &ref_gray,
        &cur_gray,
        roi_x, roi_y, roi_w, roi_h,
        MAX_OFFSET,
    );

    // Calculate baseline score (no offset)
    let baseline_score = calculate_region_similarity(
        &ref_gray, &cur_gray,
        roi_x, roi_y, roi_w, roi_h,
        0, 0,
    );

    // If offset doesn't improve alignment significantly, confidence is low
    let improvement = best_score - baseline_score;
    let confidence = if improvement > 0.05 && best_score > 0.7 {
        (improvement * 10.0).min(1.0) as f32
    } else {
        0.0
    };

    // Estimate how many diff pixels this offset explains
    let explained_pct = if confidence > 0.5 {
        (improvement * 100.0).min(90.0) as f32
    } else {
        0.0
    };

    Some(PositionShiftAnalysis {
        offset_x: best_offset.0,
        offset_y: best_offset.1,
        confidence,
        explained_percentage: explained_pct,
    })
}

/// Find the offset that produces the best alignment between two image regions.
fn find_best_offset(
    ref_gray: &GrayImage,
    cur_gray: &GrayImage,
    roi_x: u32, roi_y: u32, roi_w: u32, roi_h: u32,
    max_offset: i32,
) -> ((i32, i32), f64) {
    let mut best_score = 0.0f64;
    let mut best_offset = (0i32, 0i32);

    // Search all offsets in the range
    for dy in -max_offset..=max_offset {
        for dx in -max_offset..=max_offset {
            let score = calculate_region_similarity(
                ref_gray, cur_gray,
                roi_x, roi_y, roi_w, roi_h,
                dx, dy,
            );

            if score > best_score {
                best_score = score;
                best_offset = (dx, dy);
            }
        }
    }

    (best_offset, best_score)
}

/// Calculate similarity between two image regions with an offset applied.
///
/// Uses normalized cross-correlation (NCC) which is robust to brightness changes.
fn calculate_region_similarity(
    ref_gray: &GrayImage,
    cur_gray: &GrayImage,
    roi_x: u32, roi_y: u32, roi_w: u32, roi_h: u32,
    dx: i32, dy: i32,
) -> f64 {
    let (w, h) = ref_gray.dimensions();

    let mut sum_ref = 0.0f64;
    let mut sum_cur = 0.0f64;
    let mut sum_ref_sq = 0.0f64;
    let mut sum_cur_sq = 0.0f64;
    let mut sum_product = 0.0f64;
    let mut count = 0u32;

    for y in roi_y..roi_y + roi_h {
        for x in roi_x..roi_x + roi_w {
            // Reference pixel
            if x >= w || y >= h {
                continue;
            }
            let ref_val = ref_gray.get_pixel(x, y)[0] as f64;

            // Current pixel with offset applied
            let cur_x = (x as i32 + dx) as u32;
            let cur_y = (y as i32 + dy) as u32;
            if cur_x >= w || cur_y >= h {
                continue;
            }
            let cur_val = cur_gray.get_pixel(cur_x, cur_y)[0] as f64;

            sum_ref += ref_val;
            sum_cur += cur_val;
            sum_ref_sq += ref_val * ref_val;
            sum_cur_sq += cur_val * cur_val;
            sum_product += ref_val * cur_val;
            count += 1;
        }
    }

    if count == 0 {
        return 0.0;
    }

    let n = count as f64;

    // Normalized cross-correlation
    let numerator = n * sum_product - sum_ref * sum_cur;
    let denom_ref = (n * sum_ref_sq - sum_ref * sum_ref).sqrt();
    let denom_cur = (n * sum_cur_sq - sum_cur * sum_cur).sqrt();

    if denom_ref < 1.0 || denom_cur < 1.0 {
        return 0.0;
    }

    numerator / (denom_ref * denom_cur)
}

/// Detect systematic color shifts between images.
///
/// Analyzes pixels that differ to find average color delta vectors.
/// Uses both RGB deltas and LAB perceptual ΔE for accurate analysis.
fn detect_color_shift(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    basic_analysis: &DiffAnalysis,
) -> Option<ColorShiftAnalysis> {
    if basic_analysis.total_diff_pixels == 0 {
        return None;
    }

    let (w, h) = ref_img.dimensions();

    // Accumulators for signed deltas (to detect direction of shift)
    let mut total_r_delta: i64 = 0;
    let mut total_g_delta: i64 = 0;
    let mut total_b_delta: i64 = 0;
    let mut total_delta_e: f64 = 0.0;
    let mut diff_count: u32 = 0;

    // Scan all pixels for color differences
    for y in 0..h {
        for x in 0..w {
            let ref_p = ref_img.get_pixel(x, y);
            let cur_p = cur_img.get_pixel(x, y);

            let dr = cur_p[0] as i32 - ref_p[0] as i32;
            let dg = cur_p[1] as i32 - ref_p[1] as i32;
            let db = cur_p[2] as i32 - ref_p[2] as i32;

            // Only count pixels with significant difference
            if dr.unsigned_abs() >= DIFF_THRESHOLD as u32
                || dg.unsigned_abs() >= DIFF_THRESHOLD as u32
                || db.unsigned_abs() >= DIFF_THRESHOLD as u32
            {
                total_r_delta += dr as i64;
                total_g_delta += dg as i64;
                total_b_delta += db as i64;

                // Calculate LAB ΔE for perceptual difference
                let delta_e = calculate_delta_e(
                    ref_p[0], ref_p[1], ref_p[2],
                    cur_p[0], cur_p[1], cur_p[2],
                );
                total_delta_e += delta_e as f64;
                diff_count += 1;
            }
        }
    }

    if diff_count == 0 {
        return None;
    }

    let avg_r = total_r_delta as f32 / diff_count as f32;
    let avg_g = total_g_delta as f32 / diff_count as f32;
    let avg_b = total_b_delta as f32 / diff_count as f32;
    let avg_delta_e = (total_delta_e / diff_count as f64) as f32;

    // Interpret the color shift
    let interpretation = interpret_color_shift(avg_r, avg_g, avg_b);

    Some(ColorShiftAnalysis {
        avg_r_delta: avg_r,
        avg_g_delta: avg_g,
        avg_b_delta: avg_b,
        affected_pixels: diff_count,
        affected_percentage: (diff_count as f32 / (w * h) as f32) * 100.0,
        interpretation,
        avg_delta_e,
    })
}

/// Interpret what a color shift vector means in human terms.
fn interpret_color_shift(r: f32, g: f32, b: f32) -> String {
    let magnitude = (r * r + g * g + b * b).sqrt();

    if magnitude < 5.0 {
        return "minimal color shift".to_string();
    }

    // Determine dominant characteristic
    let mut descriptions = Vec::new();

    // Warmth (more red/yellow vs blue)
    if r > 10.0 && r > b {
        descriptions.push("warmer");
    } else if b > 10.0 && b > r {
        descriptions.push("cooler");
    }

    // Brightness (all channels shift together)
    let avg = (r + g + b) / 3.0;
    if avg > 15.0 {
        descriptions.push("lighter");
    } else if avg < -15.0 {
        descriptions.push("darker");
    }

    // Saturation hints
    if r.abs() > 20.0 && g.abs() < 10.0 && b.abs() < 10.0 {
        if r > 0.0 {
            descriptions.push("more red");
        } else {
            descriptions.push("less red");
        }
    }
    if g.abs() > 20.0 && r.abs() < 10.0 && b.abs() < 10.0 {
        if g > 0.0 {
            descriptions.push("more green");
        } else {
            descriptions.push("less green");
        }
    }
    if b.abs() > 20.0 && r.abs() < 10.0 && g.abs() < 10.0 {
        if b > 0.0 {
            descriptions.push("more blue");
        } else {
            descriptions.push("less blue");
        }
    }

    if descriptions.is_empty() {
        format!("color shift (Δ={:.0})", magnitude)
    } else {
        descriptions.join(", ")
    }
}

/// Calculate perceptual color difference using CIE76 ΔE formula.
///
/// This converts RGB to LAB color space and computes Euclidean distance.
/// ΔE < 1.0 is imperceptible, ΔE > 5.0 is clearly visible.
fn calculate_delta_e(r1: u8, g1: u8, b1: u8, r2: u8, g2: u8, b2: u8) -> f32 {
    // Convert RGB to XYZ
    let (x1, y1, z1) = rgb_to_xyz(r1, g1, b1);
    let (x2, y2, z2) = rgb_to_xyz(r2, g2, b2);

    // Convert XYZ to LAB
    let (l1, a1, b1_lab) = xyz_to_lab(x1, y1, z1);
    let (l2, a2, b2_lab) = xyz_to_lab(x2, y2, z2);

    // CIE76 ΔE = sqrt((L2-L1)² + (a2-a1)² + (b2-b1)²)
    let dl = l2 - l1;
    let da = a2 - a1;
    let db = b2_lab - b1_lab;

    (dl * dl + da * da + db * db).sqrt()
}

/// Convert RGB to XYZ color space.
fn rgb_to_xyz(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    // Linearize sRGB
    fn linearize(c: u8) -> f32 {
        let c = c as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    let r = linearize(r) * 100.0;
    let g = linearize(g) * 100.0;
    let b = linearize(b) * 100.0;

    // sRGB to XYZ matrix (D65 illuminant)
    let x = r * 0.4124564 + g * 0.3575761 + b * 0.1804375;
    let y = r * 0.2126729 + g * 0.7151522 + b * 0.0721750;
    let z = r * 0.0193339 + g * 0.1191920 + b * 0.9503041;

    (x, y, z)
}

/// Convert XYZ to LAB color space.
fn xyz_to_lab(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    // D65 reference white point
    const REF_X: f32 = 95.047;
    const REF_Y: f32 = 100.000;
    const REF_Z: f32 = 108.883;

    fn f(t: f32) -> f32 {
        const DELTA: f32 = 6.0 / 29.0;
        if t > DELTA * DELTA * DELTA {
            t.powf(1.0 / 3.0)
        } else {
            t / (3.0 * DELTA * DELTA) + 4.0 / 29.0
        }
    }

    let fx = f(x / REF_X);
    let fy = f(y / REF_Y);
    let fz = f(z / REF_Z);

    let l = 116.0 * fy - 16.0;
    let a = 500.0 * (fx - fy);
    let b = 200.0 * (fy - fz);

    (l, a, b)
}

/// Find continuous bands of lines with differences.
fn find_dense_bands(lines: &[u32]) -> Vec<LineRange> {
    if lines.is_empty() {
        return vec![];
    }

    let mut bands = Vec::new();
    let mut start = lines[0];
    let mut prev = lines[0];

    for &line in lines.iter().skip(1) {
        if line != prev + 1 {
            // Gap found - emit band if it's substantial (>= 5 lines)
            if prev - start >= 5 {
                bands.push(LineRange { start, end: prev });
            }
            start = line;
        }
        prev = line;
    }

    // Don't forget the last band
    if prev - start >= 5 {
        bands.push(LineRange { start, end: prev });
    }

    bands
}

/// Print console report with ASCII grid visualization.
fn print_console_report(analysis: &DiffAnalysis) {
    println!("SSIM: {:.4} (threshold: {:.4})", analysis.ssim, analysis.threshold);
    println!();

    // ASCII grid visualization
    println!("=== Region Analysis ({}x{} grid, {}px cells) ===",
             analysis.grid_size, analysis.grid_size,
             analysis.image_width / analysis.grid_size);

    // Header row
    print!("   ");
    for col in 0..analysis.grid_size {
        print!(" {:^4}", col);
    }
    println!("     Legend:");

    // Grid rows
    for row in 0..analysis.grid_size {
        print!("{:2} ", row);
        for col in 0..analysis.grid_size {
            let region = &analysis.regions[(row * analysis.grid_size + col) as usize];
            let symbol = if region.diff_percentage >= 10.0 {
                '#'
            } else if region.diff_percentage >= 5.0 {
                '!'
            } else if region.diff_percentage >= 1.0 {
                'X'
            } else if region.diff_percentage >= 0.1 {
                'x'
            } else {
                '.'
            };
            print!("  {}  ", symbol);
        }
        // Add legend on first few rows
        match row {
            0 => println!("     . = <0.1% diff"),
            1 => println!("     x = 0.1-1% diff"),
            2 => println!("     X = 1-5% diff"),
            3 => println!("     ! = 5-10% diff"),
            4 => println!("     # = >10% diff"),
            _ => println!(),
        }
    }
    println!();

    // Hot regions (sorted by diff percentage)
    let mut hot_regions: Vec<_> = analysis.regions.iter()
        .filter(|r| r.diff_percentage >= 0.1)
        .collect();
    hot_regions.sort_by(|a, b| b.diff_percentage.partial_cmp(&a.diff_percentage).unwrap());

    if !hot_regions.is_empty() {
        println!("Hot regions:");
        for (i, region) in hot_regions.iter().enumerate() {
            let marker = if i == 0 { " << WORST" } else { "" };
            println!("  [{},{}] {:.1}% diff ({} pixels), max_delta={}, channel={}{}",
                     region.row, region.col,
                     region.diff_percentage,
                     region.diff_pixel_count,
                     region.max_delta,
                     region.dominant_channel,
                     marker);
        }
        println!();
    }

    // Affected lines summary
    println!("=== Affected Lines ===");
    println!("Lines with differences: {}", analysis.affected_lines.len());
    if !analysis.affected_lines.is_empty() {
        println!("First affected: line {}", analysis.affected_lines.first().unwrap());
        println!("Last affected: line {}", analysis.affected_lines.last().unwrap());
    }

    if !analysis.dense_bands.is_empty() {
        println!("Dense bands:");
        for band in &analysis.dense_bands {
            println!("  - lines {}-{} (continuous differences)", band.start, band.end);
        }
    }
    println!();

    // Bounding box
    if let Some(ref bbox) = analysis.bounding_box {
        println!("=== Difference Bounding Box ===");
        println!("Top-left:     ({}, {})", bbox.x1, bbox.y1);
        println!("Bottom-right: ({}, {})", bbox.x2, bbox.y2);
        println!("Size:         {} x {} pixels ({:.1}% of image area)",
                 bbox.width(), bbox.height(),
                 (bbox.area() as f32 / (analysis.image_width * analysis.image_height) as f32) * 100.0);

        // CSS coordinates (assuming 2x HiDPI)
        println!();
        println!("CSS coordinates (assuming 2x HiDPI):");
        println!("  top: {}px, left: {}px", bbox.y1 / 2, bbox.x1 / 2);
        println!("  width: {}px, height: {}px", bbox.width() / 2, bbox.height() / 2);
    }
    println!();

    // Semantic analysis (if performed)
    if let Some(ref semantic) = analysis.semantic {
        println!("=== Semantic Analysis ===");
        println!();

        // Color shift analysis
        if let Some(ref color) = semantic.color_shift {
            let confidence = if color.affected_percentage >= 5.0 {
                "HIGH"
            } else if color.affected_percentage >= 1.0 {
                "MEDIUM"
            } else {
                "LOW"
            };

            println!("COLOR_SHIFT detected ({} confidence):", confidence);
            println!("  Interpretation: {}", color.interpretation);
            println!("  Affected: {:.1}% of pixels ({} pixels)", color.affected_percentage, color.affected_pixels);
            println!("  RGB delta: R={:+.1}, G={:+.1}, B={:+.1}", color.avg_r_delta, color.avg_g_delta, color.avg_b_delta);
            println!("  Perceptual ΔE: {:.1} (>5 = clearly visible)", color.avg_delta_e);
            println!("  Action: Check CSS `color` or `background` properties");
            println!();
        }

        // Position shift (Phase 7b - placeholder)
        if let Some(ref pos) = semantic.position_shift {
            if pos.confidence >= 0.5 {
                println!("POSITION_SHIFT detected:");
                println!("  Offset: {}px horizontal, {}px vertical", pos.offset_x, pos.offset_y);
                println!("  Confidence: {:.0}%", pos.confidence * 100.0);
                println!("  Action: Check `margin`, `padding`, or `transform` CSS");
                println!();
            }
        }

        // Font change (Phase 7c)
        // NOTE: Only shown if confidence is reasonably high
        if let Some(ref font) = semantic.font_change {
            if font.confidence >= 0.4 {
                let conf_label = if font.confidence >= 0.7 { "HIGH" } else { "LOW" };
                println!("FONT_CHANGE detected ({} confidence):", conf_label);
                println!("  Reference appears: {}", if font.ref_appears_cursive { "cursive/script" } else { "sans-serif/serif" });
                println!("  Current appears: {}", if font.cur_appears_cursive { "cursive/script" } else { "sans-serif/serif" });
                println!("  Edge variance: ref={:.2}, cur={:.2}", font.ref_edge_variance, font.cur_edge_variance);
                println!("  ⚠ LIMITATION: Cannot detect italic vs regular - verify visually!");
                println!("  Action: Check `font-family` loading, verify @font-face or web font import");
                println!();
            }
        }

        // Size change (Phase 7d - placeholder)
        if let Some(ref size) = semantic.size_change {
            if size.confidence >= 0.5 {
                println!("SIZE_CHANGE detected:");
                println!("  Scale factor: {:.2}x (current {} than reference)",
                         size.scale_factor,
                         if size.scale_factor < 1.0 { "smaller" } else { "larger" });
                println!("  Action: Check `font-size`, `width`, `height`, or `zoom` CSS");
                println!();
            }
        }

        // Recommendations
        if !semantic.recommendations.is_empty() {
            println!("Recommendations:");
            for rec in &semantic.recommendations {
                println!("  • {}", rec);
            }
            println!();
        }

        // Always suggest visual verification for worst region
        let worst_region = analysis.regions.iter()
            .max_by(|a, b| a.diff_percentage.partial_cmp(&b.diff_percentage).unwrap());
        if let Some(worst) = worst_region {
            if worst.diff_percentage >= 1.0 {
                println!("💡 Verify visually: --zoom-region {},{} --output /tmp/zoom.png",
                         worst.row, worst.col);
                println!();
            }
        }
    }

    // Final verdict
    if analysis.passed {
        println!("PASS: SSIM {:.4} meets threshold {:.4}", analysis.ssim, analysis.threshold);
    } else {
        println!("FAIL: SSIM {:.4} is below threshold {:.4}", analysis.ssim, analysis.threshold);
    }
}

/// Print JSON report for programmatic analysis.
fn print_json_report(analysis: &DiffAnalysis) {
    // Build JSON manually to avoid serde dependency
    println!("{{");
    println!("  \"ssim\": {:.6},", analysis.ssim);
    println!("  \"threshold\": {:.6},", analysis.threshold);
    println!("  \"passed\": {},", analysis.passed);
    println!("  \"total_diff_pixels\": {},", analysis.total_diff_pixels);
    println!("  \"diff_percentage\": {:.4},", analysis.diff_percentage);
    println!("  \"image_width\": {},", analysis.image_width);
    println!("  \"image_height\": {},", analysis.image_height);

    // Bounding box
    if let Some(ref bbox) = analysis.bounding_box {
        println!("  \"bounding_box\": {{\"x1\": {}, \"y1\": {}, \"x2\": {}, \"y2\": {}}},",
                 bbox.x1, bbox.y1, bbox.x2, bbox.y2);
    } else {
        println!("  \"bounding_box\": null,");
    }

    // Hot regions (only those with >= 0.1% diff)
    let hot_regions: Vec<_> = analysis.regions.iter()
        .filter(|r| r.diff_percentage >= 0.1)
        .collect();

    println!("  \"regions\": [");
    for (i, region) in hot_regions.iter().enumerate() {
        let comma = if i < hot_regions.len() - 1 { "," } else { "" };
        println!("    {{\"row\": {}, \"col\": {}, \"diff_pct\": {:.2}, \"pixels\": {}, \"max_delta\": {}, \"channel\": \"{}\"}}{}",
                 region.row, region.col, region.diff_percentage,
                 region.diff_pixel_count, region.max_delta, region.dominant_channel, comma);
    }
    println!("  ],");

    // Dense bands
    println!("  \"affected_line_count\": {},", analysis.affected_lines.len());
    println!("  \"dense_bands\": [");
    for (i, band) in analysis.dense_bands.iter().enumerate() {
        let comma = if i < analysis.dense_bands.len() - 1 { "," } else { "" };
        println!("    [{}, {}]{}", band.start, band.end, comma);
    }
    println!("  ]");
    println!("}}");
}

/// Generate a diff visualization image highlighting differences between two images.
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

            let dr = (a[0] as i32 - b[0] as i32).unsigned_abs() as u8;
            let dg = (a[1] as i32 - b[1] as i32).unsigned_abs() as u8;
            let db = (a[2] as i32 - b[2] as i32).unsigned_abs() as u8;

            // Amplify difference by 3x for visibility
            let amp = 3u8;
            let dr_amp = dr.saturating_mul(amp);
            let dg_amp = dg.saturating_mul(amp);
            let db_amp = db.saturating_mul(amp);

            if dr < DIFF_THRESHOLD && dg < DIFF_THRESHOLD && db < DIFF_THRESHOLD {
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

/// Generate a diff image with grid overlay showing region boundaries.
fn generate_grid_diff(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    output_path: &str,
    analysis: &DiffAnalysis,
) -> Result<()> {
    let (w, h) = ref_img.dimensions();
    let mut diff = RgbaImage::new(w, h);
    let cell_w = w / analysis.grid_size;
    let cell_h = h / analysis.grid_size;

    // First pass: generate base diff
    for y in 0..h {
        for x in 0..w {
            let a = ref_img.get_pixel(x, y);
            let b = cur_img.get_pixel(x, y);

            let dr = (a[0] as i32 - b[0] as i32).unsigned_abs() as u8;
            let dg = (a[1] as i32 - b[1] as i32).unsigned_abs() as u8;
            let db = (a[2] as i32 - b[2] as i32).unsigned_abs() as u8;

            let amp = 3u8;
            let dr_amp = dr.saturating_mul(amp);
            let dg_amp = dg.saturating_mul(amp);
            let db_amp = db.saturating_mul(amp);

            if dr < DIFF_THRESHOLD && dg < DIFF_THRESHOLD && db < DIFF_THRESHOLD {
                diff.put_pixel(x, y, Rgba([a[0] / 4, a[1] / 4, a[2] / 4, 255]));
            } else {
                diff.put_pixel(x, y, Rgba([dr_amp, dg_amp, db_amp, 255]));
            }
        }
    }

    // Second pass: draw grid lines
    let grid_color = Rgba([128, 128, 128, 255]);
    for i in 1..analysis.grid_size {
        let x = i * cell_w;
        let y = i * cell_h;

        // Vertical line
        for py in 0..h {
            if x < w {
                diff.put_pixel(x, py, grid_color);
            }
        }

        // Horizontal line
        for px in 0..w {
            if y < h {
                diff.put_pixel(px, y, grid_color);
            }
        }
    }

    // Third pass: color-code hot regions with semi-transparent overlay
    for region in &analysis.regions {
        if region.diff_percentage >= 1.0 {
            let start_x = region.col * cell_w;
            let start_y = region.row * cell_h;
            let end_x = (start_x + cell_w).min(w);
            let end_y = (start_y + cell_h).min(h);

            // Determine overlay color based on severity
            let overlay = if region.diff_percentage >= 10.0 {
                Rgba([255, 0, 0, 60]) // Red
            } else if region.diff_percentage >= 5.0 {
                Rgba([255, 165, 0, 50]) // Orange
            } else {
                Rgba([255, 255, 0, 40]) // Yellow
            };

            // Apply overlay with alpha blending
            for y in start_y..end_y {
                for x in start_x..end_x {
                    let base = diff.get_pixel(x, y);
                    let alpha = overlay[3] as f32 / 255.0;
                    let blended = Rgba([
                        ((1.0 - alpha) * base[0] as f32 + alpha * overlay[0] as f32) as u8,
                        ((1.0 - alpha) * base[1] as f32 + alpha * overlay[1] as f32) as u8,
                        ((1.0 - alpha) * base[2] as f32 + alpha * overlay[2] as f32) as u8,
                        255,
                    ]);
                    diff.put_pixel(x, y, blended);
                }
            }
        }
    }

    diff.save(output_path)
        .with_context(|| format!("Failed to save grid diff image: {}", output_path))?;

    Ok(())
}

/// Generate a heatmap visualization.
fn generate_heatmap(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    output_path: &str,
    _analysis: &DiffAnalysis,
) -> Result<()> {
    let (w, h) = ref_img.dimensions();
    let mut heatmap = RgbaImage::new(w, h);

    for y in 0..h {
        for x in 0..w {
            let a = ref_img.get_pixel(x, y);
            let b = cur_img.get_pixel(x, y);

            let dr = (a[0] as i32 - b[0] as i32).unsigned_abs() as u8;
            let dg = (a[1] as i32 - b[1] as i32).unsigned_abs() as u8;
            let db = (a[2] as i32 - b[2] as i32).unsigned_abs() as u8;

            let max_diff = dr.max(dg).max(db);

            // Map to heatmap colors: blue -> cyan -> green -> yellow -> red
            let color = diff_to_heatmap_color(max_diff);
            heatmap.put_pixel(x, y, color);
        }
    }

    heatmap.save(output_path)
        .with_context(|| format!("Failed to save heatmap image: {}", output_path))?;

    Ok(())
}

/// Convert a difference value to a heatmap color.
fn diff_to_heatmap_color(diff: u8) -> Rgba<u8> {
    if diff < 10 {
        // Blue (no/minimal difference)
        Rgba([30, 30, 100, 255])
    } else if diff < 30 {
        // Cyan (small difference)
        Rgba([0, 150, 150, 255])
    } else if diff < 60 {
        // Green (moderate difference)
        Rgba([0, 200, 0, 255])
    } else if diff < 100 {
        // Yellow (significant difference)
        Rgba([255, 255, 0, 255])
    } else if diff < 150 {
        // Orange (large difference)
        Rgba([255, 165, 0, 255])
    } else {
        // Red (extreme difference)
        Rgba([255, 0, 0, 255])
    }
}

/// Generate a side-by-side composite image.
fn generate_composite(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    output_path: &str,
) -> Result<()> {
    let (w, h) = ref_img.dimensions();

    // Generate diff
    let mut diff = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let a = ref_img.get_pixel(x, y);
            let b = cur_img.get_pixel(x, y);

            let dr = (a[0] as i32 - b[0] as i32).unsigned_abs() as u8;
            let dg = (a[1] as i32 - b[1] as i32).unsigned_abs() as u8;
            let db = (a[2] as i32 - b[2] as i32).unsigned_abs() as u8;

            let amp = 3u8;
            let dr_amp = dr.saturating_mul(amp);
            let dg_amp = dg.saturating_mul(amp);
            let db_amp = db.saturating_mul(amp);

            if dr < DIFF_THRESHOLD && dg < DIFF_THRESHOLD && db < DIFF_THRESHOLD {
                diff.put_pixel(x, y, Rgba([a[0] / 4, a[1] / 4, a[2] / 4, 255]));
            } else {
                diff.put_pixel(x, y, Rgba([dr_amp, dg_amp, db_amp, 255]));
            }
        }
    }

    // Create composite: [Reference] | [Current] | [Diff]
    let composite_w = w * 3 + 4; // 2px separator between each
    let mut composite = RgbaImage::new(composite_w, h);

    // Fill with separator color
    for y in 0..h {
        for x in 0..composite_w {
            composite.put_pixel(x, y, Rgba([50, 50, 50, 255]));
        }
    }

    // Copy reference
    for y in 0..h {
        for x in 0..w {
            let p = ref_img.get_pixel(x, y);
            composite.put_pixel(x, y, Rgba([p[0], p[1], p[2], 255]));
        }
    }

    // Copy current
    let offset = w + 2;
    for y in 0..h {
        for x in 0..w {
            let p = cur_img.get_pixel(x, y);
            composite.put_pixel(offset + x, y, Rgba([p[0], p[1], p[2], 255]));
        }
    }

    // Copy diff
    let offset = w * 2 + 4;
    for y in 0..h {
        for x in 0..w {
            let p = diff.get_pixel(x, y);
            composite.put_pixel(offset + x, y, *p);
        }
    }

    composite.save(output_path)
        .with_context(|| format!("Failed to save composite image: {}", output_path))?;

    Ok(())
}

/// Simplified comparison without diff output
#[allow(dead_code)]
pub fn run_simple(reference: &str, current: &str, threshold: f64) -> Result<()> {
    run(reference, current, None, threshold)
}

/// Generate a zoomed side-by-side comparison of a specific grid region.
///
/// Creates an image showing:
/// ```
/// ┌────────────────────┬────────────────────┐
/// │    REFERENCE       │     CURRENT        │
/// │  (region row,col)  │   (region row,col) │
/// │   scaled by Nx     │   scaled by Nx     │
/// └────────────────────┴────────────────────┘
/// ```
fn generate_zoom_region(
    ref_img: &image::DynamicImage,
    cur_img: &image::DynamicImage,
    row: u32,
    col: u32,
    grid_size: u32,
    scale: u32,
    output_path: &str,
    analysis: &DiffAnalysis,
) -> Result<()> {
    let (w, h) = ref_img.dimensions();
    let cell_w = w / grid_size;
    let cell_h = h / grid_size;

    // Calculate region bounds
    let region_x = col * cell_w;
    let region_y = row * cell_h;

    // Validate region bounds
    if row >= grid_size || col >= grid_size {
        anyhow::bail!("Region [{},{}] out of bounds for {}x{} grid", row, col, grid_size, grid_size);
    }

    // Extract regions from both images
    let ref_region = ref_img.crop_imm(region_x, region_y, cell_w, cell_h);
    let cur_region = cur_img.crop_imm(region_x, region_y, cell_w, cell_h);

    // Scale up using nearest-neighbor to preserve pixel edges
    let scaled_w = cell_w * scale;
    let scaled_h = cell_h * scale;

    let ref_scaled = ref_region.resize_exact(
        scaled_w,
        scaled_h,
        image::imageops::FilterType::Nearest,
    );
    let cur_scaled = cur_region.resize_exact(
        scaled_w,
        scaled_h,
        image::imageops::FilterType::Nearest,
    );

    // Create output image: side-by-side with separator and title bar
    let separator_w = 4;
    let title_h = 30;
    let output_w = scaled_w * 2 + separator_w;
    let output_h = scaled_h + title_h;

    let mut output = RgbaImage::new(output_w, output_h);

    // Fill background (title bar area)
    for y in 0..output_h {
        for x in 0..output_w {
            output.put_pixel(x, y, Rgba([40, 40, 40, 255]));
        }
    }

    // Draw separator line
    for y in title_h..output_h {
        for x in scaled_w..(scaled_w + separator_w) {
            output.put_pixel(x, y, Rgba([80, 80, 80, 255]));
        }
    }

    // Copy reference (left side)
    for y in 0..scaled_h {
        for x in 0..scaled_w {
            let p = ref_scaled.get_pixel(x, y);
            output.put_pixel(x, y + title_h, Rgba([p[0], p[1], p[2], 255]));
        }
    }

    // Copy current (right side)
    let right_offset = scaled_w + separator_w;
    for y in 0..scaled_h {
        for x in 0..scaled_w {
            let p = cur_scaled.get_pixel(x, y);
            output.put_pixel(right_offset + x, y + title_h, Rgba([p[0], p[1], p[2], 255]));
        }
    }

    // Draw simple labels in the title bar using basic shapes
    // "REF" on left, "CUR" on right
    let label_y = 8;
    draw_text_simple(&mut output, "REFERENCE", 10, label_y, Rgba([200, 200, 200, 255]));
    draw_text_simple(&mut output, "CURRENT", right_offset + 10, label_y, Rgba([200, 200, 200, 255]));

    // Find region stats for subtitle
    let region_stats = analysis.regions.iter()
        .find(|r| r.row == row && r.col == col);

    // Draw region info
    if let Some(stats) = region_stats {
        let info = format!("[{},{}] {:.1}% diff", row, col, stats.diff_percentage);
        draw_text_simple(&mut output, &info, 10, label_y + 12, Rgba([150, 150, 150, 255]));

        let css_x = region_x / 2; // Assuming 2x HiDPI
        let css_y = region_y / 2;
        let css_info = format!("CSS: top={}px left={}px", css_y, css_x);
        draw_text_simple(&mut output, &css_info, right_offset + 10, label_y + 12, Rgba([150, 150, 150, 255]));
    }

    output.save(output_path)
        .with_context(|| format!("Failed to save zoom region image: {}", output_path))?;

    println!("Zoom region [{},{}] saved to: {}", row, col, output_path);
    println!("  Scale: {}x ({}x{} → {}x{})", scale, cell_w, cell_h, scaled_w, scaled_h);
    println!("  CSS coordinates: top={}px, left={}px", region_y / 2, region_x / 2);

    Ok(())
}

/// Draw simple text using basic pixel patterns.
/// This is a minimal ASCII renderer for labels - no font dependencies.
fn draw_text_simple(img: &mut RgbaImage, text: &str, x: u32, y: u32, color: Rgba<u8>) {
    // Very basic 5x7 pixel font patterns for common chars
    // Each char is represented as a 5-wide bitmask for 7 rows
    fn char_pattern(c: char) -> [u8; 7] {
        match c.to_ascii_uppercase() {
            'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
            'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
            'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
            'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
            'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
            'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
            'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
            'S' => [0b01110, 0b10001, 0b10000, 0b01110, 0b00001, 0b10001, 0b01110],
            '[' => [0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110],
            ']' => [0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110],
            ',' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00100],
            '.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00110, 0b00110],
            '%' => [0b11001, 0b11010, 0b00100, 0b00100, 0b01000, 0b01011, 0b10011],
            '=' => [0b00000, 0b00000, 0b11111, 0b00000, 0b11111, 0b00000, 0b00000],
            ':' => [0b00000, 0b00110, 0b00110, 0b00000, 0b00110, 0b00110, 0b00000],
            '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
            '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
            '2' => [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
            '3' => [0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110],
            '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
            '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
            '6' => [0b01110, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b01110],
            '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
            '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
            '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
            'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
            'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
            'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
            'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
            'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
            'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
            'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
            ' ' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000],
            _ => [0b11111, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11111], // Box for unknown
        }
    }

    let mut cursor_x = x;
    let (img_w, img_h) = img.dimensions();

    for c in text.chars() {
        let pattern = char_pattern(c);
        for (row, &bits) in pattern.iter().enumerate() {
            for col in 0..5 {
                if (bits >> (4 - col)) & 1 == 1 {
                    let px = cursor_x + col;
                    let py = y + row as u32;
                    if px < img_w && py < img_h {
                        img.put_pixel(px, py, color);
                    }
                }
            }
        }
        cursor_x += 6; // 5 pixels + 1 spacing
    }
}

/// Parse a region string like "3,3" into (row, col).
fn parse_region_string(s: &str) -> Result<(u32, u32)> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 2 {
        anyhow::bail!("Invalid region format '{}'. Expected 'row,col' (e.g., '3,3')", s);
    }

    let row: u32 = parts[0].trim().parse()
        .with_context(|| format!("Invalid row number: '{}'", parts[0]))?;
    let col: u32 = parts[1].trim().parse()
        .with_context(|| format!("Invalid column number: '{}'", parts[1]))?;

    Ok((row, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_dense_bands_empty() {
        let bands = find_dense_bands(&[]);
        assert!(bands.is_empty());
    }

    #[test]
    fn test_find_dense_bands_single_band() {
        let lines: Vec<u32> = (100..120).collect();
        let bands = find_dense_bands(&lines);
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].start, 100);
        assert_eq!(bands[0].end, 119);
    }

    #[test]
    fn test_find_dense_bands_multiple() {
        let mut lines: Vec<u32> = (100..120).collect();
        lines.extend(200..230);
        let bands = find_dense_bands(&lines);
        assert_eq!(bands.len(), 2);
    }
}
