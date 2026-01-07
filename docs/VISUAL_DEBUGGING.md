# Visual Debugging Workflow Guide

This guide describes how to iteratively converge your UI render toward a reference image using the `boon-tools` visual debugging suite.

## Overview

When visual tests fail, you face a challenge: SSIM gives a single score but no spatial information about WHERE differences are or WHAT TYPE they are. This suite solves that by providing:

1. **Spatial Analysis** - 7x7 grid showing which regions differ most
2. **Semantic Analysis** - Identifies difference TYPES (color, position, font, size)
3. **Region Zoom** - Magnified side-by-side comparison of problem areas
4. **CSS Coordinates** - Exact pixel locations for targeted debugging

## Prerequisites

Before starting, ensure:
- Boon playground running (`cd playground && makers mzoon start`)
- Browser with extension connected to playground
- Reference image available

## The Convergence Loop

### Step 1: Initial Analysis

Run `pixel-diff` to get spatial metrics:

```bash
boon-tools pixel-diff \
  --reference /path/to/reference.png \
  --current /path/to/screenshot.png \
  --grid \
  --output /tmp/diff.png
```

This outputs:
- SSIM score (0.0-1.0)
- 7x7 ASCII grid with severity markers
- Hot regions ranked by difference percentage
- Affected line ranges
- Bounding box with CSS coordinates

### Step 2: Identify Problem Type

Add `--analyze-semantic` for type detection:

```bash
boon-tools pixel-diff \
  --reference reference.png \
  --current current.png \
  --analyze-semantic
```

The semantic analyzer detects:

| Type | Detection Method | Action |
|------|------------------|--------|
| `COLOR_SHIFT` | RGB delta + LAB ΔE | Fix CSS `color` or `background` |
| `POSITION_SHIFT` | Cross-correlation | Fix `margin`, `padding`, `transform` |
| `FONT_CHANGE` | Edge variance | Check `font-family` loading |
| `SIZE_CHANGE` | Edge density | Fix `font-size`, `zoom` |

### Step 3: Zoom Into Problem Region

Extract and magnify the worst region:

```bash
boon-tools pixel-diff \
  --reference reference.png \
  --current current.png \
  --zoom-region 3,3 \
  --output /tmp/zoom.png
```

This creates a side-by-side comparison at 4x scale with:
- Reference region (left)
- Current region (right)
- CSS coordinates in the title bar

### Step 4: Make Targeted Fix

Based on the analysis:

- **COLOR_SHIFT detected**: Check CSS color values, opacity
- **FONT_CHANGE detected**: Verify `@font-face` loaded, check fallback fonts
- **POSITION_SHIFT detected**: Check margins, padding, flexbox alignment
- **SIZE_CHANGE detected**: Compare font-size values, check viewport scaling

### Step 5: Re-verify

Take a new screenshot and check SSIM:

```bash
boon-tools exec screenshot-preview --output /tmp/new.png --hidpi
boon-tools pixel-diff --reference reference.png --current /tmp/new.png
```

### Step 6: Repeat Until Converged

Continue the loop until SSIM meets the threshold (default: 0.95).

## Interactive Debugging Script

For iterative debugging, use the `visual_debug.sh` helper:

```bash
cd tools/scripts
./visual_debug.sh --reference /path/to/reference.png --threshold 0.90
```

This script:
1. Takes screenshots automatically
2. Runs analysis with spatial metrics
3. Shows the worst region coordinates
4. Waits for you to make changes
5. Repeats until converged

## Quick Reference

### CLI Flags

| Flag | Description |
|------|-------------|
| `--reference` | Path to reference image |
| `--current` | Path to current screenshot |
| `--output` | Path for diff/zoom output image |
| `--threshold` | SSIM threshold (default: 0.95) |
| `--json` | Output as JSON for programmatic use |
| `--grid` | Add grid overlay to diff image |
| `--heatmap` | Generate heatmap visualization |
| `--composite` | Create side-by-side [ref\|cur\|diff] |
| `--zoom-region` | Zoom into grid cell (e.g., "3,3") |
| `--zoom-scale` | Zoom factor (default: 4) |
| `--analyze-semantic` | Run semantic type detection |

### Understanding the Grid

```
=== Region Analysis (7x7 grid, 200px cells) ===
     0    1    2    3    4    5    6       Legend:
 0   .    .    .    .    .    .    .       . = <0.1% diff
 1   .    .    X    X    .    .    .       x = 0.1-1% diff
 2   .    .    X    !    X    .    .       X = 1-5% diff
 3   .    .    .    #    .    .    .       ! = 5-10% diff
 4   .    .    .    .    .    .    .       # = >10% diff
```

The grid shows difference intensity per 200×200 pixel region (assuming 1400×1400 image).

### Semantic Analysis Output

```
=== Semantic Analysis ===

COLOR_SHIFT detected (HIGH confidence):
  Interpretation: warmer, more red
  Affected: 5.2% of pixels
  RGB delta: R=+37.4, G=+0.0, B=+0.0
  Perceptual ΔE: 16.9 (>5 = clearly visible)
  Action: Check CSS `color` or `background` properties

FONT_CHANGE detected (MEDIUM confidence):
  Reference appears: cursive/script
  Current appears: sans-serif
  Edge variance: ref=52.3, cur=28.1
  Action: Check `font-family` loading

Recommendations:
  • COLOR_SHIFT: warmer, more red - check CSS color properties
  • FONT_CHANGE: Reference uses cursive/script, current uses sans-serif
```

## Common Problems & Solutions

### Font not loading (cursive/serif)

**Symptom**: FONT_CHANGE detected with ref=cursive, cur=sans-serif

**Check**:
1. `@font-face` declarations are correct
2. Font files exist and are served
3. CORS headers allow font loading
4. Network tab shows font requests

### Color mismatch

**Symptom**: COLOR_SHIFT with RGB deltas

**Check**:
1. CSS `color` property values
2. CSS `background` property values
3. Alpha/opacity differences
4. Theme/dark mode differences

### Position offset

**Symptom**: POSITION_SHIFT with offset values

**Check**:
1. `margin` and `padding` values
2. `flexbox` alignment properties
3. `transform` translations
4. Font metrics (different fonts have different baselines)

### Size differences

**Symptom**: SIZE_CHANGE with scale_factor != 1.0

**Check**:
1. `font-size` values
2. Viewport/zoom settings
3. Device pixel ratio handling
4. Container width constraints

## JSON Output for Automation

For CI/CD integration, use `--json`:

```bash
boon-tools pixel-diff \
  --reference reference.png \
  --current current.png \
  --json
```

Output includes:
- `ssim`: float (0.0-1.0)
- `passed`: boolean
- `bounding_box`: {x1, y1, x2, y2}
- `regions`: array of hot regions
- `dense_bands`: line ranges with continuous diffs

## Anti-Cheat Protection

Reference images are protected by SHA256 hash verification to prevent "cheating" (replacing reference with broken render instead of fixing code).

The hash is stored in `verify_visual.sh`:

```bash
REFERENCE_HASH="4eed3835c50064087a378cae337df2a5e4b3499afd638e7e1afed79b6647d1d5"
```

If someone modifies the reference, the verification fails with:
```
ERROR: Reference image has been modified!
Expected: 4eed3835...
Actual:   abc123...
```

To legitimately update the reference (after team review):
1. Update `REFERENCE_HASH` in the script
2. Document the reason in the commit message

## MCP Tools for Claude

When Claude has access to Boon Browser MCP, the `boon_visual_debug` tool provides programmatic access to visual analysis. Claude can:

1. Take screenshots: `boon_screenshot_preview`
2. Get console errors: `boon_console`
3. Run visual analysis: `boon_visual_debug`
4. Navigate: `boon_navigate`

This enables automated debugging loops where Claude can identify and fix visual regressions.
