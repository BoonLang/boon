//! Font patcher: adds infinite-length dash/equals ligatures to JetBrains Mono.
//!
//! Uses the same start/mid/end glyph technique as Fira Code for truly unlimited
//! sequence lengths. Three GSUB passes (type 6 L→R, type 8 R→L, type 6 L→R)
//! transform consecutive hyphens/equals into seamless lines of any length.

use read_fonts::{
    tables::{
        cmap::CmapSubtable,
        glyf::{CurvePoint, Glyph as ReadGlyph},
    },
    types::{GlyphId, Tag},
    FontRef, TableProvider,
};
use std::path::Path;
use write_fonts::{
    from_obj::FromTableRef,
    tables::{
        glyf::{Bbox, Contour, Glyph, GlyfLocaBuilder, SimpleGlyph},
        gsub::{
            ReverseChainSingleSubstFormat1, SingleSubst, SubstitutionChainContext,
            SubstitutionLookup,
        },
        layout::{
            ChainedSequenceContextFormat3, CoverageTable, Lookup,
            LookupFlag, SequenceLookupRecord,
        },
    },
    FontBuilder,
};

fn main() {
    let input = Path::new("../public/fonts/JetBrainsMono-Regular.woff2");
    let output_ttf = Path::new("../public/fonts/JetBrainsMono-Patched.ttf");
    let output_woff2 = Path::new("../public/fonts/JetBrainsMono-Patched.woff2");

    // --- 1. Read and decode WOFF2 ---
    let woff2_bytes = std::fs::read(input).expect("failed to read input font");
    let ttf_bytes =
        woff2_patched::decode::convert_woff2_to_ttf(&mut std::io::Cursor::new(&woff2_bytes))
            .expect("failed to decode WOFF2");
    let font = FontRef::new(&ttf_bytes).expect("failed to parse font");

    let num_glyphs = font.maxp().expect("no maxp").num_glyphs();
    println!("Original font: {num_glyphs} glyphs");

    // --- 2. Find glyph IDs for hyphen and equal ---
    let cmap = font.cmap().expect("no cmap");
    let hyphen_gid = find_glyph_id(&cmap, 0x002D).expect("hyphen not found");
    let equal_gid = find_glyph_id(&cmap, 0x003D).expect("equal not found");
    println!(
        "Hyphen GID: {}, Equal GID: {}",
        hyphen_gid.to_u32(),
        equal_gid.to_u32()
    );

    // New glyph IDs (appended after existing glyphs)
    let hyphen_start_gid = num_glyphs;
    let hyphen_mid_gid = num_glyphs + 1;
    let hyphen_end_gid = num_glyphs + 2;
    let equal_start_gid = num_glyphs + 3;
    let equal_mid_gid = num_glyphs + 4;
    let equal_end_gid = num_glyphs + 5;
    let new_num_glyphs = num_glyphs + 6;

    println!("New glyphs: {hyphen_start_gid}..={}", new_num_glyphs - 1);

    // --- 3. Read original glyph metrics ---
    let glyf_table = font.glyf().expect("no glyf");
    let loca_table = font.loca(None).expect("no loca");
    // Extract hyphen outline metrics
    let hyphen_glyph = loca_table
        .get_glyf(hyphen_gid, &glyf_table)
        .ok()
        .flatten()
        .expect("can't read hyphen glyph");
    let (h_y_min, h_y_max) = match &hyphen_glyph {
        ReadGlyph::Simple(s) => (s.y_min(), s.y_max()),
        _ => panic!("hyphen is not a simple glyph"),
    };

    // Extract equal outline metrics
    let equal_glyph = loca_table
        .get_glyf(equal_gid, &glyf_table)
        .ok()
        .flatten()
        .expect("can't read equal glyph");
    let (eq_top_y_min, eq_top_y_max, eq_bot_y_min, eq_bot_y_max) = match &equal_glyph {
        ReadGlyph::Simple(s) => {
            // Equal has 2 contours: top bar and bottom bar
            // Top bar: points 0-3, Bottom bar: points 4-7
            let points: Vec<_> = s.points().collect();
            let top_y_min = points[0].y; // 410
            let top_y_max = points[1].y; // 490
            let bot_y_min = points[4].y; // 170
            let bot_y_max = points[5].y; // 250
            (top_y_min, top_y_max, bot_y_min, bot_y_max)
        }
        _ => panic!("equal is not a simple glyph"),
    };

    println!("Hyphen bar: y={h_y_min}..{h_y_max}");
    println!("Equal top: y={eq_top_y_min}..{eq_top_y_max}, bottom: y={eq_bot_y_min}..{eq_bot_y_max}");

    // Original x-coordinates from glyph outlines
    let h_x_start = 140i16; // original hyphen left edge
    let h_x_end = 460i16; // original hyphen right edge
    let eq_x_start = 85i16; // original equal left edge
    let eq_x_end = 515i16; // original equal right edge
    let cell_width = 600i16; // monospace cell width

    // --- 4. Create 6 new glyphs ---
    let new_glyphs = [
        // hyphen.start: original left → cell boundary
        make_single_bar(h_x_start, cell_width, h_y_min, h_y_max),
        // hyphen.mid: cell boundary → cell boundary (full width)
        make_single_bar(0, cell_width, h_y_min, h_y_max),
        // hyphen.end: cell boundary → original right
        make_single_bar(0, h_x_end, h_y_min, h_y_max),
        // equal.start
        make_double_bar(
            eq_x_start,
            cell_width,
            eq_top_y_min,
            eq_top_y_max,
            eq_bot_y_min,
            eq_bot_y_max,
        ),
        // equal.mid
        make_double_bar(
            0,
            cell_width,
            eq_top_y_min,
            eq_top_y_max,
            eq_bot_y_min,
            eq_bot_y_max,
        ),
        // equal.end
        make_double_bar(
            0,
            eq_x_end,
            eq_top_y_min,
            eq_top_y_max,
            eq_bot_y_min,
            eq_bot_y_max,
        ),
    ];

    // --- 5. Rebuild glyf + loca tables ---
    let mut glyf_builder = GlyfLocaBuilder::new();

    // Copy all existing glyphs
    for gid_val in 0..num_glyphs {
        let gid = GlyphId::new(gid_val.into());
        match loca_table.get_glyf(gid, &glyf_table) {
            Ok(Some(read_glyph)) => {
                let write_glyph = Glyph::from_table_ref(&read_glyph);
                glyf_builder
                    .add_glyph(&write_glyph)
                    .unwrap_or_else(|e| panic!("failed to re-add glyph {gid_val}: {e}"));
            }
            _ => {
                // Empty glyph (e.g., .notdef, space)
                glyf_builder
                    .add_glyph(&Glyph::Empty)
                    .expect("failed to add empty glyph");
            }
        }
    }

    // Add 6 new glyphs
    for (i, glyph) in new_glyphs.iter().enumerate() {
        glyf_builder
            .add_glyph(glyph)
            .unwrap_or_else(|e| panic!("failed to add new glyph {i}: {e}"));
    }

    let (glyf_out, loca_out, loca_format) = glyf_builder.build();
    println!("Rebuilt glyf+loca ({new_num_glyphs} glyphs, loca format: {loca_format:?})");

    // --- 6. Modify GSUB: add our ligature lookups ---
    let gsub = build_gsub(
        &font,
        hyphen_gid,
        equal_gid,
        hyphen_start_gid,
        hyphen_mid_gid,
        hyphen_end_gid,
        equal_start_gid,
        equal_mid_gid,
        equal_end_gid,
    );

    // --- 7. Modify hmtx: append metrics for new glyphs ---
    let hmtx_raw = font
        .data_for_tag(Tag::new(b"hmtx"))
        .expect("no hmtx data");
    let mut hmtx_bytes = hmtx_raw.as_ref().to_vec();

    let new_metrics: [(u16, i16); 6] = [
        (600, h_x_start),  // hyphen.start: advance=600, lsb=140
        (600, 0),          // hyphen.mid: advance=600, lsb=0
        (600, 0),          // hyphen.end: advance=600, lsb=0
        (600, eq_x_start), // equal.start: advance=600, lsb=85
        (600, 0),          // equal.mid: advance=600, lsb=0
        (600, 0),          // equal.end: advance=600, lsb=0
    ];

    // numberOfHMetrics == numGlyphs in this font, so each entry is 4 bytes
    // The leftSideBearings array (if any) follows after numberOfHMetrics entries
    let hhea = font.hhea().expect("no hhea");
    let n_hmetrics = hhea.number_of_h_metrics();

    if n_hmetrics == num_glyphs {
        // All glyphs have full metrics — append at end of hMetrics array
        // (which is at lsb_array_start, since there's no leftSideBearings array)
        let mut new_bytes = Vec::new();
        for (advance, lsb) in &new_metrics {
            new_bytes.extend_from_slice(&advance.to_be_bytes());
            new_bytes.extend_from_slice(&lsb.to_be_bytes());
        }
        // Insert before any leftSideBearings (which don't exist in this case)
        hmtx_bytes.extend_from_slice(&new_bytes);
    } else {
        // Some glyphs only have leftSideBearings — insert new full metrics
        // at the boundary and add new leftSideBearings at the end
        // For simplicity, add as leftSideBearings only
        for (_, lsb) in &new_metrics {
            hmtx_bytes.extend_from_slice(&lsb.to_be_bytes());
        }
    }

    // --- 8. Update hhea (numberOfHMetrics) ---
    let mut hhea_raw = font
        .data_for_tag(Tag::new(b"hhea"))
        .expect("no hhea data")
        .as_ref()
        .to_vec();
    if n_hmetrics == num_glyphs {
        // Update numberOfHMetrics (last 2 bytes of hhea table, at offset 34)
        let new_n = new_num_glyphs;
        hhea_raw[34..36].copy_from_slice(&new_n.to_be_bytes());
    }

    // --- 9. Update maxp (numGlyphs) ---
    let mut maxp_raw = font
        .data_for_tag(Tag::new(b"maxp"))
        .expect("no maxp data")
        .as_ref()
        .to_vec();
    // numGlyphs is at offset 4 in maxp table
    maxp_raw[4..6].copy_from_slice(&new_num_glyphs.to_be_bytes());

    // --- 10. Update head (indexToLocFormat) ---
    let mut head_raw = font
        .data_for_tag(Tag::new(b"head"))
        .expect("no head data")
        .as_ref()
        .to_vec();
    let new_loc_format: i16 = match loca_format {
        write_fonts::tables::loca::LocaFormat::Short => 0,
        write_fonts::tables::loca::LocaFormat::Long => 1,
    };
    // indexToLocFormat is at offset 50 in head table
    head_raw[50..52].copy_from_slice(&new_loc_format.to_be_bytes());

    // --- 11. Update post table (add glyph names) ---
    let post_raw = font
        .data_for_tag(Tag::new(b"post"))
        .expect("no post data");
    let post_bytes = patch_post_table(post_raw.as_ref(), num_glyphs, &[
        "hyphen.start",
        "hyphen.mid",
        "hyphen.end",
        "equal.start",
        "equal.mid",
        "equal.end",
    ]);

    // --- 12. Assemble the font ---
    let mut builder = FontBuilder::new();

    // Add modified tables as raw bytes
    builder.add_raw(Tag::new(b"glyf"), write_fonts::dump_table(&glyf_out).expect("glyf serialize"));
    builder.add_raw(Tag::new(b"loca"), write_fonts::dump_table(&loca_out).expect("loca serialize"));
    builder.add_raw(Tag::new(b"hmtx"), hmtx_bytes);
    builder.add_raw(Tag::new(b"hhea"), hhea_raw);
    builder.add_raw(Tag::new(b"maxp"), maxp_raw);
    builder.add_raw(Tag::new(b"head"), head_raw);
    builder.add_raw(Tag::new(b"post"), post_bytes);

    // Add modified GSUB via proper serialization
    builder
        .add_table(&gsub)
        .expect("failed to serialize GSUB");

    // Copy all other tables unchanged
    builder.copy_missing_tables(font);

    let font_bytes = builder.build();

    // Write TTF
    std::fs::write(output_ttf, &font_bytes).expect("failed to write TTF");
    println!("Wrote {}", output_ttf.display());

    // Convert to WOFF2 using external tool
    let status = std::process::Command::new("woff2_compress")
        .arg(output_ttf)
        .status();
    match status {
        Ok(s) if s.success() => {
            // woff2_compress writes to same path with .woff2 extension
            println!("Wrote {}", output_woff2.display());
            // Clean up TTF
            let _ = std::fs::remove_file(output_ttf);
        }
        Ok(s) => {
            eprintln!("woff2_compress failed with status {s}");
            eprintln!("TTF file kept at {}", output_ttf.display());
        }
        Err(e) => {
            eprintln!("woff2_compress not found ({e}), keeping TTF");
            eprintln!(
                "Install with: sudo apt install woff2 (or brew install woff2)"
            );
        }
    }
}

// ─── Glyph Creation ─────────────────────────────────────────────────────

/// Create a single-bar rectangular glyph (for hyphen variants)
fn make_single_bar(x_min: i16, x_max: i16, y_min: i16, y_max: i16) -> SimpleGlyph {
    let contour = Contour::from(vec![
        CurvePoint::on_curve(x_min, y_min),
        CurvePoint::on_curve(x_min, y_max),
        CurvePoint::on_curve(x_max, y_max),
        CurvePoint::on_curve(x_max, y_min),
    ]);
    SimpleGlyph {
        bbox: Bbox {
            x_min,
            y_min,
            x_max,
            y_max,
        },
        contours: vec![contour],
        instructions: vec![],
    }
}

/// Create a double-bar rectangular glyph (for equal variants)
fn make_double_bar(
    x_min: i16,
    x_max: i16,
    top_y_min: i16,
    top_y_max: i16,
    bot_y_min: i16,
    bot_y_max: i16,
) -> SimpleGlyph {
    let top_contour = Contour::from(vec![
        CurvePoint::on_curve(x_min, top_y_min),
        CurvePoint::on_curve(x_min, top_y_max),
        CurvePoint::on_curve(x_max, top_y_max),
        CurvePoint::on_curve(x_max, top_y_min),
    ]);
    let bot_contour = Contour::from(vec![
        CurvePoint::on_curve(x_min, bot_y_min),
        CurvePoint::on_curve(x_min, bot_y_max),
        CurvePoint::on_curve(x_max, bot_y_max),
        CurvePoint::on_curve(x_max, bot_y_min),
    ]);
    let all_y_min = bot_y_min.min(top_y_min);
    let all_y_max = bot_y_max.max(top_y_max);
    SimpleGlyph {
        bbox: Bbox {
            x_min,
            y_min: all_y_min,
            x_max,
            y_max: all_y_max,
        },
        contours: vec![top_contour, bot_contour],
        instructions: vec![],
    }
}

// ─── GSUB Modification ──────────────────────────────────────────────────

fn build_gsub(
    font: &FontRef,
    hyphen_gid: GlyphId,
    equal_gid: GlyphId,
    hyphen_start: u16,
    hyphen_mid: u16,
    hyphen_end: u16,
    equal_start: u16,
    equal_mid: u16,
    equal_end: u16,
) -> write_fonts::tables::gsub::Gsub {
    use read_fonts::types::GlyphId16;
    use write_fonts::from_obj::ToOwnedTable;

    let read_gsub = font.gsub().expect("no GSUB");
    let mut gsub: write_fonts::tables::gsub::Gsub = read_gsub.to_owned_table();

    let hyphen = GlyphId16::new(u16::try_from(hyphen_gid.to_u32()).expect("hyphen GID overflow"));
    let equal = GlyphId16::new(u16::try_from(equal_gid.to_u32()).expect("equal GID overflow"));
    let h_start = GlyphId16::new(hyphen_start);
    let h_mid = GlyphId16::new(hyphen_mid);
    let h_end = GlyphId16::new(hyphen_end);
    let e_start = GlyphId16::new(equal_start);
    let e_mid = GlyphId16::new(equal_mid);
    let e_end = GlyphId16::new(equal_end);

    // --- Create 4 SingleSubst leaf lookups (for Start + End passes) ---
    // Mid pass uses type 8 which does substitution directly — no leaf needed.

    let leaf_lookups = [
        // 0: hyphen → hyphen.start
        make_single_subst_lookup(hyphen, h_start),
        // 1: hyphen → hyphen.end
        make_single_subst_lookup(hyphen, h_end),
        // 2: equal → equal.start
        make_single_subst_lookup(equal, e_start),
        // 3: equal → equal.end
        make_single_subst_lookup(equal, e_end),
    ];

    // Append leaf lookups to the lookup list and record their indices
    let lookup_list = &mut gsub.lookup_list;
    let base_idx = lookup_list.lookups.len() as u16;
    for leaf in leaf_lookups {
        lookup_list.lookups.push(leaf.into());
    }

    let sub_h_start_idx = base_idx;
    let sub_h_end_idx = base_idx + 1;
    let sub_e_start_idx = base_idx + 2;
    let sub_e_end_idx = base_idx + 3;

    // --- 3-pass design using type 6 (L→R) and type 8 (R→L) ---
    //
    // For `------` (6 hyphens):
    //   Pass 1 (type 6, L→R): hyphen followed by hyphen → h.start
    //     Result: h.start h.start h.start h.start h.start hyphen
    //   Pass 2 (type 8, R→L): h.start preceded by h.start/h.mid → h.mid
    //     R→L: pos4 h.start preceded by h.start → h.mid
    //           pos3 h.start preceded by h.start → h.mid
    //           pos2 h.start preceded by h.start → h.mid
    //           pos1 h.start preceded by h.start → h.mid
    //           pos0 h.start with no left neighbor → stays
    //     Result: h.start h.mid h.mid h.mid h.mid hyphen
    //   Pass 3 (type 6, L→R): hyphen preceded by h.start/h.mid → h.end
    //     Result: h.start h.mid h.mid h.mid h.mid h.end ✓

    // Pass 1 — Start (type 6): hyphen followed by hyphen → hyphen.start
    let chain_h_start = make_chain_lookup(
        &[],                  // no backtrack
        &[hyphen],            // input: hyphen
        &[hyphen],            // lookahead: hyphen
        sub_h_start_idx,
    );

    let chain_e_start = make_chain_lookup(
        &[],
        &[equal],
        &[equal],
        sub_e_start_idx,
    );

    // Pass 2 — Mid (type 8, R→L): h.start preceded by h.start/h.mid → h.mid
    // In type 8: backtrack = left context, lookahead = right context
    // (same meaning as type 6, just R→L processing order)
    let reverse_h_mid = make_reverse_chain_lookup(
        &[h_start, h_mid],    // backtrack (left): must see start or mid
        &[h_start],           // coverage (input): match h.start glyphs
        &[],                  // no lookahead (right)
        &[h_mid],             // substitute: h.start → h.mid
    );

    let reverse_e_mid = make_reverse_chain_lookup(
        &[e_start, e_mid],
        &[e_start],
        &[],
        &[e_mid],
    );

    // Pass 3 — End (type 6): hyphen preceded by start/mid → hyphen.end
    let chain_h_end = make_chain_lookup(
        &[h_start, h_mid],    // backtrack: start or mid
        &[hyphen],            // input: hyphen
        &[],                  // no lookahead (end of sequence)
        sub_h_end_idx,
    );

    let chain_e_end = make_chain_lookup(
        &[e_start, e_mid],
        &[equal],
        &[],
        sub_e_end_idx,
    );

    // Append chain lookups in pass order: Start, Mid (reverse), End
    let chain_lookups: Vec<SubstitutionLookup> = vec![
        chain_h_start,
        chain_e_start,
        reverse_h_mid,
        reverse_e_mid,
        chain_h_end,
        chain_e_end,
    ];

    let mut chain_indices = Vec::new();
    for chain in chain_lookups {
        let idx = lookup_list.lookups.len() as u16;
        chain_indices.push(idx);
        lookup_list.lookups.push(chain.into());
    }

    // --- Prepend chain lookup indices to the calt feature ---
    // Find the calt feature and prepend our indices so they fire first
    let feature_list = &mut gsub.feature_list;
    for record in feature_list.feature_records.iter_mut() {
        if record.feature_tag == Tag::new(b"calt") {
            let feature = &mut record.feature;
            let mut new_indices = chain_indices.clone();
            new_indices.extend_from_slice(&feature.lookup_list_indices);
            feature.lookup_list_indices = new_indices;
            println!(
                "Updated calt feature: {} lookup indices (6 new + {} existing)",
                feature.lookup_list_indices.len(),
                feature.lookup_list_indices.len() - 6
            );
            break;
        }
    }

    println!(
        "GSUB: {} total lookups (10 new: 4 leaf + 4 chain type6 + 2 reverse type8)",
        lookup_list.lookups.len()
    );

    gsub
}

/// Create a SingleSubst lookup: from_gid → to_gid
fn make_single_subst_lookup(
    from: read_fonts::types::GlyphId16,
    to: read_fonts::types::GlyphId16,
) -> SubstitutionLookup {
    let coverage: CoverageTable = [from].into_iter().collect();
    let subtable = SingleSubst::format_2(coverage, vec![to]);
    SubstitutionLookup::Single(Lookup::new(LookupFlag::empty(), vec![subtable]))
}

/// Create a ChainContextSubst (type 6, format 3) lookup.
///
/// - `backtrack_glyphs`: glyphs that must precede the input (any of these)
/// - `input_glyphs`: the glyph being substituted
/// - `lookahead_glyphs`: glyphs that must follow the input (any of these)
/// - `leaf_lookup_idx`: index of the SingleSubst leaf lookup to apply
fn make_chain_lookup(
    backtrack_glyphs: &[read_fonts::types::GlyphId16],
    input_glyphs: &[read_fonts::types::GlyphId16],
    lookahead_glyphs: &[read_fonts::types::GlyphId16],
    leaf_lookup_idx: u16,
) -> SubstitutionLookup {
    let backtrack_coverages: Vec<CoverageTable> = if backtrack_glyphs.is_empty() {
        vec![]
    } else {
        vec![backtrack_glyphs.iter().copied().collect()]
    };

    let input_coverages: Vec<CoverageTable> =
        vec![input_glyphs.iter().copied().collect()];

    let lookahead_coverages: Vec<CoverageTable> = if lookahead_glyphs.is_empty() {
        vec![]
    } else {
        vec![lookahead_glyphs.iter().copied().collect()]
    };

    let seq_lookup = SequenceLookupRecord::new(
        0, // sequence_index: position 0 in input
        leaf_lookup_idx,
    );

    use write_fonts::tables::layout::ChainedSequenceContext;
    let chain_ctx = ChainedSequenceContext::Format3(ChainedSequenceContextFormat3::new(
        backtrack_coverages,
        input_coverages,
        lookahead_coverages,
        vec![seq_lookup],
    ));
    let subtable = SubstitutionChainContext::from(chain_ctx);

    SubstitutionLookup::ChainContextual(Lookup::new(LookupFlag::empty(), vec![subtable]))
}

/// Create a ReverseChainSingleSubst (type 8) lookup for right-to-left processing.
///
/// Type 8 processes glyphs right-to-left and performs substitution directly
/// (no leaf lookup indirection). Despite R→L processing, backtrack = left context
/// and lookahead = right context (same as type 6).
///
/// - `backtrack_glyphs`: glyphs that must appear to the LEFT of input
/// - `input_glyphs`: the glyphs to match (coverage)
/// - `lookahead_glyphs`: glyphs that must appear to the RIGHT of input
/// - `substitute_glyphs`: replacement glyphs, one per input glyph in coverage order
fn make_reverse_chain_lookup(
    backtrack_glyphs: &[read_fonts::types::GlyphId16],
    input_glyphs: &[read_fonts::types::GlyphId16],
    lookahead_glyphs: &[read_fonts::types::GlyphId16],
    substitute_glyphs: &[read_fonts::types::GlyphId16],
) -> SubstitutionLookup {
    let coverage: CoverageTable = input_glyphs.iter().copied().collect();

    let backtrack_coverages: Vec<CoverageTable> = if backtrack_glyphs.is_empty() {
        vec![]
    } else {
        vec![backtrack_glyphs.iter().copied().collect()]
    };

    let lookahead_coverages: Vec<CoverageTable> = if lookahead_glyphs.is_empty() {
        vec![]
    } else {
        vec![lookahead_glyphs.iter().copied().collect()]
    };

    let subtable = ReverseChainSingleSubstFormat1::new(
        coverage,
        backtrack_coverages,
        lookahead_coverages,
        substitute_glyphs.to_vec(),
    );

    SubstitutionLookup::Reverse(Lookup::new(LookupFlag::empty(), vec![subtable]))
}

// ─── Post Table Patching ─────────────────────────────────────────────────

/// Patch the post table to add names for new glyphs.
///
/// Post v2.0 format:
///   - Header (32 bytes)
///   - numGlyphs (u16)
///   - glyphNameIndex[numGlyphs] (u16 array)
///   - stringData (Pascal strings: length byte + chars)
///
/// We update numGlyphs, extend the index array, and append new strings.
fn patch_post_table(raw: &[u8], old_num_glyphs: u16, new_names: &[&str]) -> Vec<u8> {
    // Check version (first 4 bytes should be 0x00020000 for v2.0)
    let version = u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
    if version != 0x0002_0000 {
        // Not v2.0 — just return unchanged
        eprintln!("Post table is not v2.0 (version={version:#010x}), skipping name patch");
        return raw.to_vec();
    }

    let mut out = raw.to_vec();

    // Update numGlyphs at offset 32
    let new_num = old_num_glyphs + new_names.len() as u16;
    out[32..34].copy_from_slice(&new_num.to_be_bytes());

    // Find where the glyphNameIndex array ends
    // It starts at offset 34, each entry is 2 bytes
    let index_end = 34 + (old_num_glyphs as usize) * 2;

    // Find the highest name index currently used
    let mut max_index: u16 = 0;
    for i in 0..old_num_glyphs as usize {
        let idx = u16::from_be_bytes([out[34 + i * 2], out[34 + i * 2 + 1]]);
        max_index = max_index.max(idx);
    }

    // Names with index < 258 are standard Mac glyph names
    // Custom names start at index 258 and reference the string data
    let first_custom_index = if max_index >= 258 {
        max_index + 1
    } else {
        258
    };

    // Build new index entries and string data
    let mut new_index_bytes = Vec::new();
    let mut new_string_bytes = Vec::new();
    for (i, name) in new_names.iter().enumerate() {
        let idx = first_custom_index + i as u16;
        new_index_bytes.extend_from_slice(&idx.to_be_bytes());
        // Pascal string: length byte + ASCII chars
        new_string_bytes.push(name.len() as u8);
        new_string_bytes.extend_from_slice(name.as_bytes());
    }

    // Insert new index entries at `index_end` (before string data)
    // Then append new string data at the end
    let string_data_start = index_end;
    let existing_strings = out[string_data_start..].to_vec();
    out.truncate(string_data_start);
    out.extend_from_slice(&new_index_bytes);
    out.extend_from_slice(&existing_strings);
    out.extend_from_slice(&new_string_bytes);

    out
}

// ─── Utilities ───────────────────────────────────────────────────────────

fn find_glyph_id(
    cmap: &read_fonts::tables::cmap::Cmap,
    codepoint: u32,
) -> Option<GlyphId> {
    for record in cmap.encoding_records() {
        if let Ok(subtable) = record.subtable(cmap.offset_data()) {
            match subtable {
                CmapSubtable::Format4(f4) => {
                    if let Some(gid) = f4.map_codepoint(codepoint) {
                        return Some(gid);
                    }
                }
                CmapSubtable::Format12(f12) => {
                    if let Some(gid) = f12.map_codepoint(codepoint) {
                        return Some(gid);
                    }
                }
                _ => {}
            }
        }
    }
    None
}
