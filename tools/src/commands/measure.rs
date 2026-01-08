//! Image measurement tools for visual debugging
//!
//! Provides deterministic measurement of:
//! - Text regions (bounding boxes, heights)
//! - Colors (point sampling, dominant colors)
//! - Objects (connected components)

use anyhow::{anyhow, Result};
use image::{DynamicImage, GenericImageView, Rgba};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

// ============================================================================
// Data Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorInfo {
    pub rgb: [u8; 3],
    pub hex: String,
}

impl ColorInfo {
    pub fn from_rgba(rgba: Rgba<u8>) -> Self {
        Self {
            rgb: [rgba[0], rgba[1], rgba[2]],
            hex: format!("#{:02x}{:02x}{:02x}", rgba[0], rgba[1], rgba[2]),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextRegion {
    pub bounding_box: BoundingBox,
    pub height_px: u32,
    pub width_px: u32,
    pub center: Point,
    pub dominant_color: ColorInfo,
    pub pixel_count: u32,
    pub estimated_font_size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextMeasureResult {
    pub text_regions: Vec<TextRegion>,
    pub total_regions: usize,
    pub image_dimensions: (u32, u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DominantColor {
    pub color: ColorInfo,
    pub percentage: f32,
    pub pixel_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorMeasureResult {
    pub mode: String,
    pub color: Option<ColorInfo>,
    pub colors: Option<Vec<DominantColor>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectRegion {
    pub id: u32,
    pub bounds: BoundingBox,
    pub area_px: u32,
    pub dominant_color: ColorInfo,
    pub center: Point,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectsResult {
    pub objects: Vec<ObjectRegion>,
    pub total_objects: usize,
}

// ============================================================================
// Connected Component Labeling (Two-Pass Algorithm)
// ============================================================================

/// Union-Find data structure for equivalence class resolution
struct UnionFind {
    parent: Vec<u32>,
    rank: Vec<u32>,
}

impl UnionFind {
    fn new(size: usize) -> Self {
        Self {
            parent: (0..size as u32).collect(),
            rank: vec![0; size],
        }
    }

    fn find(&mut self, x: u32) -> u32 {
        if self.parent[x as usize] != x {
            self.parent[x as usize] = self.find(self.parent[x as usize]);
        }
        self.parent[x as usize]
    }

    fn union(&mut self, x: u32, y: u32) {
        let root_x = self.find(x);
        let root_y = self.find(y);
        if root_x != root_y {
            if self.rank[root_x as usize] < self.rank[root_y as usize] {
                self.parent[root_x as usize] = root_y;
            } else if self.rank[root_x as usize] > self.rank[root_y as usize] {
                self.parent[root_y as usize] = root_x;
            } else {
                self.parent[root_y as usize] = root_x;
                self.rank[root_x as usize] += 1;
            }
        }
    }
}

/// Component statistics gathered during labeling
#[derive(Debug, Default)]
struct ComponentStats {
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
    pixel_count: u32,
    color_sum: [u64; 3], // Sum of RGB for average color
}

impl ComponentStats {
    fn new(x: u32, y: u32, color: Rgba<u8>) -> Self {
        Self {
            min_x: x,
            min_y: y,
            max_x: x,
            max_y: y,
            pixel_count: 1,
            color_sum: [color[0] as u64, color[1] as u64, color[2] as u64],
        }
    }

    fn add_pixel(&mut self, x: u32, y: u32, color: Rgba<u8>) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
        self.pixel_count += 1;
        self.color_sum[0] += color[0] as u64;
        self.color_sum[1] += color[1] as u64;
        self.color_sum[2] += color[2] as u64;
    }

    fn merge(&mut self, other: &ComponentStats) {
        self.min_x = self.min_x.min(other.min_x);
        self.min_y = self.min_y.min(other.min_y);
        self.max_x = self.max_x.max(other.max_x);
        self.max_y = self.max_y.max(other.max_y);
        self.pixel_count += other.pixel_count;
        self.color_sum[0] += other.color_sum[0];
        self.color_sum[1] += other.color_sum[1];
        self.color_sum[2] += other.color_sum[2];
    }

    fn bounding_box(&self) -> BoundingBox {
        BoundingBox {
            x: self.min_x,
            y: self.min_y,
            width: self.max_x - self.min_x + 1,
            height: self.max_y - self.min_y + 1,
        }
    }

    fn center(&self) -> Point {
        Point {
            x: (self.min_x + self.max_x) / 2,
            y: (self.min_y + self.max_y) / 2,
        }
    }

    fn average_color(&self) -> ColorInfo {
        let count = self.pixel_count.max(1) as u64;
        ColorInfo {
            rgb: [
                (self.color_sum[0] / count) as u8,
                (self.color_sum[1] / count) as u8,
                (self.color_sum[2] / count) as u8,
            ],
            hex: format!(
                "#{:02x}{:02x}{:02x}",
                (self.color_sum[0] / count) as u8,
                (self.color_sum[1] / count) as u8,
                (self.color_sum[2] / count) as u8
            ),
        }
    }
}

/// Check if a pixel matches the target color within tolerance
fn color_matches(pixel: Rgba<u8>, target: Option<Rgba<u8>>, tolerance: u8) -> bool {
    match target {
        Some(t) => {
            let dr = (pixel[0] as i16 - t[0] as i16).unsigned_abs() as u8;
            let dg = (pixel[1] as i16 - t[1] as i16).unsigned_abs() as u8;
            let db = (pixel[2] as i16 - t[2] as i16).unsigned_abs() as u8;
            dr <= tolerance && dg <= tolerance && db <= tolerance
        }
        None => {
            // No target: match any non-white pixel (text detection mode)
            // Consider "white" as RGB > 240 for all channels
            pixel[0] < 240 || pixel[1] < 240 || pixel[2] < 240
        }
    }
}

/// Check if pixel is significantly different from background (for contrast-based detection)
fn is_foreground(pixel: Rgba<u8>, bg_color: Rgba<u8>, threshold: u8) -> bool {
    let dr = (pixel[0] as i16 - bg_color[0] as i16).unsigned_abs() as u8;
    let dg = (pixel[1] as i16 - bg_color[1] as i16).unsigned_abs() as u8;
    let db = (pixel[2] as i16 - bg_color[2] as i16).unsigned_abs() as u8;
    dr > threshold || dg > threshold || db > threshold
}

/// Detect background color by sampling corners
fn detect_background(img: &DynamicImage) -> Rgba<u8> {
    let (w, h) = img.dimensions();
    let corners = [
        img.get_pixel(0, 0),
        img.get_pixel(w - 1, 0),
        img.get_pixel(0, h - 1),
        img.get_pixel(w - 1, h - 1),
    ];

    // Average the corner colors
    let mut sum = [0u32; 4];
    for c in &corners {
        sum[0] += c[0] as u32;
        sum[1] += c[1] as u32;
        sum[2] += c[2] as u32;
        sum[3] += c[3] as u32;
    }

    Rgba([
        (sum[0] / 4) as u8,
        (sum[1] / 4) as u8,
        (sum[2] / 4) as u8,
        (sum[3] / 4) as u8,
    ])
}

/// Two-pass connected component labeling algorithm
///
/// Returns a map from canonical label to component statistics
fn connected_components(
    img: &DynamicImage,
    target_color: Option<Rgba<u8>>,
    tolerance: u8,
    min_size: u32,
) -> HashMap<u32, ComponentStats> {
    let (w, h) = img.dimensions();
    let total_pixels = (w * h) as usize;

    // Detect background for contrast-based detection
    let bg_color = detect_background(img);
    let contrast_threshold = 30u8;

    // Label array and union-find for equivalences
    let mut labels = vec![0u32; total_pixels];
    let mut uf = UnionFind::new(total_pixels / 4 + 1); // Rough upper bound on label count
    let mut next_label = 1u32;
    let mut stats: HashMap<u32, ComponentStats> = HashMap::new();

    // First pass: assign labels and record equivalences
    for y in 0..h {
        for x in 0..w {
            let pixel = img.get_pixel(x, y);
            let idx = (y * w + x) as usize;

            // Check if this pixel is part of foreground
            let is_fg = if target_color.is_some() {
                color_matches(pixel, target_color, tolerance)
            } else {
                is_foreground(pixel, bg_color, contrast_threshold)
            };

            if !is_fg {
                continue; // Background pixel
            }

            // Get labels of left and top neighbors
            let left_label = if x > 0 { labels[(y * w + x - 1) as usize] } else { 0 };
            let top_label = if y > 0 { labels[((y - 1) * w + x) as usize] } else { 0 };

            match (left_label, top_label) {
                (0, 0) => {
                    // New component
                    labels[idx] = next_label;
                    stats.insert(next_label, ComponentStats::new(x, y, pixel));
                    next_label += 1;
                }
                (l, 0) => {
                    // Extend left component
                    labels[idx] = l;
                    if let Some(s) = stats.get_mut(&l) {
                        s.add_pixel(x, y, pixel);
                    }
                }
                (0, t) => {
                    // Extend top component
                    labels[idx] = t;
                    if let Some(s) = stats.get_mut(&t) {
                        s.add_pixel(x, y, pixel);
                    }
                }
                (l, t) if l == t => {
                    // Same component
                    labels[idx] = l;
                    if let Some(s) = stats.get_mut(&l) {
                        s.add_pixel(x, y, pixel);
                    }
                }
                (l, t) => {
                    // Merge components: use smaller label, record equivalence
                    let min_label = l.min(t);
                    let max_label = l.max(t);
                    labels[idx] = min_label;
                    uf.union(min_label, max_label);
                    if let Some(s) = stats.get_mut(&min_label) {
                        s.add_pixel(x, y, pixel);
                    }
                }
            }
        }
    }

    // Second pass: resolve equivalences and merge stats
    let mut canonical_stats: HashMap<u32, ComponentStats> = HashMap::new();

    for (label, stat) in stats {
        let root = uf.find(label);
        canonical_stats
            .entry(root)
            .and_modify(|s| s.merge(&stat))
            .or_insert(stat);
    }

    // Filter by minimum size
    canonical_stats.retain(|_, s| s.pixel_count >= min_size);

    canonical_stats
}

// ============================================================================
// Text Measurement API
// ============================================================================

/// Measure text regions in an image
///
/// # Arguments
/// * `image_path` - Path to image file
/// * `target_color` - Optional: specific color to find (hex string like "#af3f3f")
/// * `tolerance` - Color matching tolerance (0-255)
/// * `min_size` - Minimum pixels for a region to be counted
/// * `region` - Optional: crop region to analyze
pub fn measure_text(
    image_path: &str,
    target_color: Option<&str>,
    tolerance: u8,
    min_size: u32,
    region: Option<(u32, u32, u32, u32)>, // (x, y, width, height)
) -> Result<TextMeasureResult> {
    let img = image::open(Path::new(image_path))
        .map_err(|e| anyhow!("Failed to open image '{}': {}", image_path, e))?;

    // Optionally crop to region
    let img = if let Some((rx, ry, rw, rh)) = region {
        img.crop_imm(rx, ry, rw, rh)
    } else {
        img
    };

    let (w, h) = img.dimensions();

    // Parse target color
    let target_rgba = target_color.map(|s| parse_hex_color(s)).transpose()?;

    // Run connected component labeling
    let components = connected_components(&img, target_rgba, tolerance, min_size);

    // Convert to TextRegion results, sorted by Y position (top to bottom)
    let mut text_regions: Vec<TextRegion> = components
        .values()
        .map(|stats| {
            let bbox = stats.bounding_box();
            let height = bbox.height;
            // Estimate font size: text height / ~1.2 (typical line-height ratio)
            let estimated_font_size = (height as f32 / 1.2).round() as u32;

            TextRegion {
                bounding_box: bbox.clone(),
                height_px: height,
                width_px: bbox.width,
                center: stats.center(),
                dominant_color: stats.average_color(),
                pixel_count: stats.pixel_count,
                estimated_font_size,
            }
        })
        .collect();

    // Sort by Y position (top to bottom), then X (left to right)
    text_regions.sort_by_key(|r| (r.bounding_box.y, r.bounding_box.x));

    let total = text_regions.len();

    Ok(TextMeasureResult {
        text_regions,
        total_regions: total,
        image_dimensions: (w, h),
    })
}

// ============================================================================
// Color Measurement API
// ============================================================================

/// Sample color at a single point
pub fn measure_color_point(image_path: &str, x: u32, y: u32) -> Result<ColorMeasureResult> {
    let img = image::open(Path::new(image_path))
        .map_err(|e| anyhow!("Failed to open image '{}': {}", image_path, e))?;

    let (w, h) = img.dimensions();
    if x >= w || y >= h {
        return Err(anyhow!(
            "Point ({}, {}) is outside image bounds ({}x{})",
            x,
            y,
            w,
            h
        ));
    }

    let pixel = img.get_pixel(x, y);

    Ok(ColorMeasureResult {
        mode: "point".to_string(),
        color: Some(ColorInfo::from_rgba(pixel)),
        colors: None,
    })
}

/// Get dominant colors in a region using K-means clustering
pub fn measure_color_dominant(
    image_path: &str,
    region: Option<(u32, u32, u32, u32)>,
    num_colors: usize,
) -> Result<ColorMeasureResult> {
    let img = image::open(Path::new(image_path))
        .map_err(|e| anyhow!("Failed to open image '{}': {}", image_path, e))?;

    // Optionally crop to region
    let img = if let Some((rx, ry, rw, rh)) = region {
        img.crop_imm(rx, ry, rw, rh)
    } else {
        img
    };

    // Collect all pixels
    let mut pixels: Vec<[u8; 3]> = Vec::new();
    for (_, _, pixel) in img.pixels() {
        pixels.push([pixel[0], pixel[1], pixel[2]]);
    }

    if pixels.is_empty() {
        return Err(anyhow!("No pixels in region"));
    }

    // Run K-means clustering
    let dominant = kmeans_colors(&pixels, num_colors, 10);

    Ok(ColorMeasureResult {
        mode: "dominant".to_string(),
        color: None,
        colors: Some(dominant),
    })
}

/// K-means color clustering
fn kmeans_colors(pixels: &[[u8; 3]], k: usize, iterations: usize) -> Vec<DominantColor> {
    if pixels.is_empty() || k == 0 {
        return vec![];
    }

    let k = k.min(pixels.len());
    let total_pixels = pixels.len();

    // Initialize centroids using k-means++ style: spread them out
    let mut centroids: Vec<[f32; 3]> = Vec::with_capacity(k);
    centroids.push([
        pixels[0][0] as f32,
        pixels[0][1] as f32,
        pixels[0][2] as f32,
    ]);

    for _ in 1..k {
        // Pick the pixel farthest from existing centroids
        let mut max_dist = 0.0f32;
        let mut best_pixel = pixels[0];

        for p in pixels.iter().step_by(pixels.len() / 100 + 1) {
            // Sample for speed
            let min_dist = centroids
                .iter()
                .map(|c| {
                    let dr = p[0] as f32 - c[0];
                    let dg = p[1] as f32 - c[1];
                    let db = p[2] as f32 - c[2];
                    dr * dr + dg * dg + db * db
                })
                .fold(f32::INFINITY, f32::min);

            if min_dist > max_dist {
                max_dist = min_dist;
                best_pixel = *p;
            }
        }

        centroids.push([best_pixel[0] as f32, best_pixel[1] as f32, best_pixel[2] as f32]);
    }

    // Iterate
    for _ in 0..iterations {
        // Assign pixels to nearest centroid
        let mut cluster_sums: Vec<[f64; 3]> = vec![[0.0; 3]; k];
        let mut cluster_counts: Vec<usize> = vec![0; k];

        for p in pixels {
            let nearest = centroids
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let dr = p[0] as f32 - c[0];
                    let dg = p[1] as f32 - c[1];
                    let db = p[2] as f32 - c[2];
                    (i, dr * dr + dg * dg + db * db)
                })
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap_or(0);

            cluster_sums[nearest][0] += p[0] as f64;
            cluster_sums[nearest][1] += p[1] as f64;
            cluster_sums[nearest][2] += p[2] as f64;
            cluster_counts[nearest] += 1;
        }

        // Update centroids
        for (i, centroid) in centroids.iter_mut().enumerate() {
            if cluster_counts[i] > 0 {
                let count = cluster_counts[i] as f64;
                centroid[0] = (cluster_sums[i][0] / count) as f32;
                centroid[1] = (cluster_sums[i][1] / count) as f32;
                centroid[2] = (cluster_sums[i][2] / count) as f32;
            }
        }
    }

    // Final assignment and counting
    let mut final_counts: Vec<usize> = vec![0; k];
    for p in pixels {
        let nearest = centroids
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let dr = p[0] as f32 - c[0];
                let dg = p[1] as f32 - c[1];
                let db = p[2] as f32 - c[2];
                (i, dr * dr + dg * dg + db * db)
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap_or(0);

        final_counts[nearest] += 1;
    }

    // Build result, sorted by percentage descending
    let mut results: Vec<DominantColor> = centroids
        .iter()
        .enumerate()
        .filter(|(i, _)| final_counts[*i] > 0)
        .map(|(i, c)| {
            let r = c[0].round() as u8;
            let g = c[1].round() as u8;
            let b = c[2].round() as u8;
            DominantColor {
                color: ColorInfo {
                    rgb: [r, g, b],
                    hex: format!("#{:02x}{:02x}{:02x}", r, g, b),
                },
                percentage: (final_counts[i] as f32 / total_pixels as f32) * 100.0,
                pixel_count: final_counts[i] as u32,
            }
        })
        .collect();

    results.sort_by(|a, b| b.percentage.partial_cmp(&a.percentage).unwrap());
    results
}

// ============================================================================
// Object Detection API
// ============================================================================

/// Find all distinct objects/regions in an image
pub fn find_objects(
    image_path: &str,
    min_size: u32,
    color_tolerance: u8,
) -> Result<ObjectsResult> {
    let img = image::open(Path::new(image_path))
        .map_err(|e| anyhow!("Failed to open image '{}': {}", image_path, e))?;

    // Use connected components with no target color (find all foreground)
    let components = connected_components(&img, None, color_tolerance, min_size);

    let mut objects: Vec<ObjectRegion> = components
        .iter()
        .enumerate()
        .map(|(id, (_, stats))| {
            let bbox = stats.bounding_box();
            ObjectRegion {
                id: id as u32 + 1,
                bounds: bbox,
                area_px: stats.pixel_count,
                dominant_color: stats.average_color(),
                center: stats.center(),
            }
        })
        .collect();

    // Sort by area descending
    objects.sort_by(|a, b| b.area_px.cmp(&a.area_px));

    // Re-assign IDs after sorting
    for (i, obj) in objects.iter_mut().enumerate() {
        obj.id = i as u32 + 1;
    }

    let total = objects.len();

    Ok(ObjectsResult {
        objects,
        total_objects: total,
    })
}

// ============================================================================
// Helpers
// ============================================================================

/// Parse hex color string to RGBA
fn parse_hex_color(s: &str) -> Result<Rgba<u8>> {
    let s = s.trim_start_matches('#');

    if s.len() != 6 {
        return Err(anyhow!(
            "Invalid hex color '{}': expected 6 hex digits",
            s
        ));
    }

    let r = u8::from_str_radix(&s[0..2], 16)
        .map_err(|_| anyhow!("Invalid hex color '{}': bad red component", s))?;
    let g = u8::from_str_radix(&s[2..4], 16)
        .map_err(|_| anyhow!("Invalid hex color '{}': bad green component", s))?;
    let b = u8::from_str_radix(&s[4..6], 16)
        .map_err(|_| anyhow!("Invalid hex color '{}': bad blue component", s))?;

    Ok(Rgba([r, g, b, 255]))
}

// ============================================================================
// CLI Entry Points
// ============================================================================

/// Run text measurement from CLI
pub fn run_measure_text(
    image_path: &str,
    target_color: Option<&str>,
    tolerance: u8,
    min_size: u32,
    region: Option<&str>,
    json_output: bool,
) -> Result<()> {
    // Parse region if provided: "x,y,width,height"
    let region_tuple = region
        .map(|s| {
            let parts: Vec<&str> = s.split(',').collect();
            if parts.len() != 4 {
                return Err(anyhow!(
                    "Invalid region format '{}': expected 'x,y,width,height'",
                    s
                ));
            }
            Ok((
                parts[0].parse::<u32>()?,
                parts[1].parse::<u32>()?,
                parts[2].parse::<u32>()?,
                parts[3].parse::<u32>()?,
            ))
        })
        .transpose()?;

    let result = measure_text(image_path, target_color, tolerance, min_size, region_tuple)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("=== Text Measurement Results ===");
        println!(
            "Image: {} ({}x{})",
            image_path, result.image_dimensions.0, result.image_dimensions.1
        );
        println!("Regions found: {}", result.total_regions);
        println!();

        for (i, region) in result.text_regions.iter().enumerate() {
            println!(
                "Region {}:",
                i + 1
            );
            println!(
                "  Bounding box: x={}, y={}, {}x{} px",
                region.bounding_box.x,
                region.bounding_box.y,
                region.width_px,
                region.height_px
            );
            println!("  Height: {} px", region.height_px);
            println!("  Estimated font-size: {} px", region.estimated_font_size);
            println!("  Center: ({}, {})", region.center.x, region.center.y);
            println!("  Color: {}", region.dominant_color.hex);
            println!("  Pixel count: {}", region.pixel_count);
            println!();
        }
    }

    Ok(())
}

/// Run color measurement from CLI
pub fn run_measure_color(
    image_path: &str,
    point: Option<&str>,
    region: Option<&str>,
    num_colors: usize,
    json_output: bool,
) -> Result<()> {
    let result = if let Some(pt) = point {
        // Point mode
        let parts: Vec<&str> = pt.split(',').collect();
        if parts.len() != 2 {
            return Err(anyhow!("Invalid point format '{}': expected 'x,y'", pt));
        }
        let x = parts[0].parse::<u32>()?;
        let y = parts[1].parse::<u32>()?;
        measure_color_point(image_path, x, y)?
    } else {
        // Dominant colors mode
        let region_tuple = region
            .map(|s| {
                let parts: Vec<&str> = s.split(',').collect();
                if parts.len() != 4 {
                    return Err(anyhow!(
                        "Invalid region format '{}': expected 'x,y,width,height'",
                        s
                    ));
                }
                Ok((
                    parts[0].parse::<u32>()?,
                    parts[1].parse::<u32>()?,
                    parts[2].parse::<u32>()?,
                    parts[3].parse::<u32>()?,
                ))
            })
            .transpose()?;
        measure_color_dominant(image_path, region_tuple, num_colors)?
    };

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("=== Color Measurement Results ===");
        println!("Mode: {}", result.mode);

        if let Some(color) = &result.color {
            println!("Color: {} (RGB: {:?})", color.hex, color.rgb);
        }

        if let Some(colors) = &result.colors {
            println!("Dominant colors:");
            for (i, dc) in colors.iter().enumerate() {
                println!(
                    "  {}. {} ({:.1}%, {} pixels)",
                    i + 1,
                    dc.color.hex,
                    dc.percentage,
                    dc.pixel_count
                );
            }
        }
    }

    Ok(())
}

/// Run object detection from CLI
pub fn run_find_objects(
    image_path: &str,
    min_size: u32,
    tolerance: u8,
    json_output: bool,
) -> Result<()> {
    let result = find_objects(image_path, min_size, tolerance)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("=== Object Detection Results ===");
        println!("Objects found: {}", result.total_objects);
        println!();

        for obj in &result.objects {
            println!("Object {}:", obj.id);
            println!(
                "  Bounds: x={}, y={}, {}x{}",
                obj.bounds.x, obj.bounds.y, obj.bounds.width, obj.bounds.height
            );
            println!("  Area: {} px", obj.area_px);
            println!("  Color: {}", obj.dominant_color.hex);
            println!("  Center: ({}, {})", obj.center.x, obj.center.y);
            println!();
        }
    }

    Ok(())
}
