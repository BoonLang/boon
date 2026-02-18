//! Value → Zoon element conversion.
//!
//! Takes DD Value descriptors (Element/button, Element/stripe, etc.)
//! and creates corresponding Zoon UI elements.
//!
//! ## Architecture
//!
//! Three rendering paths:
//! - **Retained tree** (General programs): Build element tree once with `Mutable<Value>`
//!   per element. On state changes, diff old vs new Value tree and update only changed
//!   Mutables. Zoon's signal system handles granular DOM updates.
//! - **Worker** (SingleHold/LatestSum): Full rebuild per state change (simple programs).
//! - **Static**: Single render, no signals needed.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use futures_channel::mpsc;
use pin_project::pin_project;
use zoon::*;

use super::super::core::types::{KeyedDiff, LIST_TAG, LINK_PATH_FIELD, HOVER_PATH_FIELD};
use super::super::core::value::Value;
use super::super::io::worker::{DdWorkerHandle, Event};

type Fields = BTreeMap<Arc<str>, Value>;

// ═══════════════════════════════════════════════════════════════════════════
// VecDiffStreamSignalVec adapter
// ═══════════════════════════════════════════════════════════════════════════

/// Wraps a `Stream<Item = VecDiff<T>>` as a `SignalVec` for Zoon's
/// `items_signal_vec` / `layers_signal_vec` / `contents_signal_vec` APIs.
#[pin_project]
#[must_use = "SignalVecs do nothing unless polled"]
struct VecDiffStreamSignalVec<A>(#[pin] A);

impl<A, T> SignalVec for VecDiffStreamSignalVec<A>
where
    A: Stream<Item = VecDiff<T>>,
{
    type Item = T;

    #[inline]
    fn poll_vec_change(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> std::task::Poll<Option<VecDiff<Self::Item>>> {
        self.project().0.poll_next(cx)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Color helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Convert an Oklch tagged value to a CSS `oklch()` string.
fn oklch_to_css(fields: &Fields) -> Option<String> {
    let l = fields.get("lightness").and_then(|v| v.as_number()).unwrap_or(0.0);
    let c = fields
        .get("chroma")
        .and_then(|v| v.as_number())
        .unwrap_or(0.0);
    let h = fields
        .get("hue")
        .and_then(|v| v.as_number())
        .unwrap_or(0.0);
    let a = fields.get("alpha").and_then(|v| v.as_number());

    if let Some(alpha) = a {
        Some(format!("oklch({} {} {} / {})", l, c, h, alpha))
    } else {
        Some(format!("oklch({} {} {})", l, c, h))
    }
}

/// Convert a color Value to a CSS color string.
/// Handles Oklch[...] tagged objects AND named color tags (White, Black, etc.)
fn value_to_css_color(value: &Value) -> Option<String> {
    match value {
        Value::Tagged { tag, fields } if tag.as_ref() == "Oklch" => oklch_to_css(fields),
        Value::Tag(tag) => {
            let css = match tag.as_ref() {
                "White" => "white",
                "Black" => "black",
                "Red" => "red",
                "Green" => "green",
                "Blue" => "blue",
                "Yellow" => "yellow",
                "Cyan" => "cyan",
                "Magenta" => "magenta",
                "Orange" => "orange",
                "Purple" => "purple",
                "Pink" => "pink",
                "Brown" => "brown",
                "Gray" | "Grey" => "gray",
                "Transparent" => "transparent",
                _ => return None,
            };
            Some(css.to_string())
        }
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Data extraction helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Get the style sub-object from element fields.
fn get_style_obj(fields: &Fields) -> Option<&Fields> {
    match fields.get("style") {
        Some(Value::Object(obj)) => Some(obj.as_ref()),
        _ => None,
    }
}

/// Get fields from a Value (works for Object and Tagged).
fn get_fields(value: &Value) -> Option<&Fields> {
    match value {
        Value::Object(f) => Some(f.as_ref()),
        Value::Tagged { fields, .. } => Some(fields.as_ref()),
        _ => None,
    }
}

/// Extract sorted list items from a "List" tagged field.
fn extract_sorted_list_items(fields: &Fields, field_name: &str) -> Vec<Value> {
    if let Some(Value::Tagged {
        tag,
        fields: list_fields,
    }) = fields.get(field_name)
    {
        if tag.as_ref() == LIST_TAG {
            // BTreeMap iteration is already sorted by key
            return list_fields.values().cloned().collect();
        }
    }
    Vec::new()
}


/// Extract effective link path for event handlers.
fn extract_effective_link(fields: &Fields, parent_link_path: &str) -> String {
    fields
        .get("press_link")
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .or_else(|| {
            fields
                .get(LINK_PATH_FIELD)
                .and_then(|v| v.as_text())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| parent_link_path.to_string())
}

/// Extract hover link path from element's `element: [hovered: LINK]` field.
fn extract_hover_link_path(fields: &Fields, parent_link_path: &str) -> Option<String> {
    // First check __hover_path__ (injected by DD compiler for per-item hover)
    if let Some(path) = fields
        .get(HOVER_PATH_FIELD)
        .and_then(|v| v.as_text())
    {
        if !path.is_empty() {
            return Some(path.to_string());
        }
    }
    // Then check __link_path__ (set by |> LINK { alias } pipe)
    if let Some(path) = fields
        .get(LINK_PATH_FIELD)
        .and_then(|v| v.as_text())
    {
        if !path.is_empty() {
            return Some(path.to_string());
        }
    }
    // Then check element.hovered.__path__ (set by element: [hovered: LINK])
    if let Some(element_val) = fields.get("element") {
        let hovered_val = match element_val {
            Value::Object(obj) => obj.get("hovered"),
            _ => None,
        };
        if let Some(Value::Tagged {
            tag,
            fields: link_fields,
        }) = hovered_val
        {
            if tag.as_ref() == "LINK" {
                if let Some(path) = link_fields
                    .get("__path__")
                    .and_then(|v| v.as_text())
                {
                    if !path.is_empty() {
                        // Strip ".hovered" suffix — the interpreter stores hover state
                        // under the base element prefix, not the field-qualified path
                        let base_path = path.strip_suffix(".hovered").unwrap_or(path);
                        return Some(base_path.to_string());
                    }
                }
            }
        }
    }
    if !parent_link_path.is_empty() {
        Some(parent_link_path.to_string())
    } else {
        None
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Zoon style extractors (from Value fields → Zoon typed styles)
// ═══════════════════════════════════════════════════════════════════════════

/// Extract Font from element fields' style.font sub-object.
fn extract_font_from_fields(fields: &Fields) -> Option<Font<'static>> {
    let style = get_style_obj(fields)?;
    let font_obj = match style.get("font") {
        Some(Value::Object(f)) => f,
        _ => return None,
    };
    let mut font = Font::new();
    if let Some(size) = font_obj.get("size").and_then(|v| v.as_number()) {
        font = font.size(size as u32);
    }
    if let Some(color_val) = font_obj.get("color") {
        if let Some(css) = value_to_css_color(color_val) {
            font = font.color(css);
        }
    }
    if let Some(weight_val) = font_obj.get("weight") {
        if let Some(tag) = weight_val.as_tag() {
            let w = match tag {
                "Hairline" => Some(FontWeight::Hairline),
                "ExtraLight" | "UltraLight" => Some(FontWeight::ExtraLight),
                "Light" => Some(FontWeight::Light),
                "Regular" | "Normal" => Some(FontWeight::Regular),
                "Medium" => Some(FontWeight::Medium),
                "SemiBold" | "DemiBold" => Some(FontWeight::SemiBold),
                "Bold" => Some(FontWeight::Bold),
                "ExtraBold" | "UltraBold" => Some(FontWeight::ExtraBold),
                "Black" | "Heavy" => Some(FontWeight::Heavy),
                _ => None,
            };
            if let Some(w) = w {
                font = font.weight(w);
            }
        } else if let Some(n) = weight_val.as_number() {
            font = font.weight(FontWeight::Number(n as u32));
        }
    }
    if let Some(align_val) = font_obj.get("align") {
        if let Some(tag) = align_val.as_tag() {
            font = match tag {
                "Center" => font.center(),
                "Left" => font.left(),
                "Right" => font.right(),
                "Justify" => font.justify(),
                _ => font,
            };
        }
    }
    if font_obj.get("style").and_then(|v| v.as_tag()) == Some("Italic") {
        font = font.italic();
    }
    if let Some(Value::Tagged {
        tag,
        fields: list_fields,
    }) = font_obj.get("family")
    {
        if tag.as_ref() == LIST_TAG {
            let mut families = Vec::new();
            // BTreeMap iteration is already sorted by key
            for item in list_fields.values() {
                match item {
                    Value::Text(name) => families.push(FontFamily::new(name.to_string())),
                    Value::Tag(t) => match t.as_ref() {
                        "SansSerif" => families.push(FontFamily::SansSerif),
                        "Serif" => families.push(FontFamily::Serif),
                        "Monospace" => families.push(FontFamily::Monospace),
                        _ => {}
                    },
                    _ => {}
                }
            }
            if !families.is_empty() {
                font = font.family(families);
            }
        }
    }
    if let Some(Value::Object(line)) = font_obj.get("line") {
        let strike = line
            .get("strikethrough")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let underline = line
            .get("underline")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if strike || underline {
            let mut fl = FontLine::new();
            if strike { fl = fl.strike(); }
            if underline { fl = fl.underline(); }
            font = font.line(fl);
        }
    }
    Some(font)
}

/// Extract Font from a full Value.
fn extract_font(value: &Value) -> Option<Font<'static>> {
    extract_font_from_fields(get_fields(value)?)
}

/// Also check align.row for text alignment (used when font.align is not set).
fn extract_align_font_from_fields(fields: &Fields) -> Option<Font<'static>> {
    let style = get_style_obj(fields)?;
    if let Some(Value::Object(align)) = style.get("align") {
        if let Some(tag) = align.get("row").and_then(|v| v.as_tag()) {
            return match tag {
                "Center" => Some(Font::new().center()),
                "Start" | "Left" => Some(Font::new().left()),
                "End" | "Right" => Some(Font::new().right()),
                _ => None,
            };
        }
    }
    None
}

fn extract_align_font(value: &Value) -> Option<Font<'static>> {
    extract_align_font_from_fields(get_fields(value)?)
}

/// Extract Width from element fields' style.
fn extract_width_from_fields(fields: &Fields) -> Option<Width<'static>> {
    let style = get_style_obj(fields)?;
    // Size field sets both width and height
    if let Some(size) = style.get("size").and_then(|v| v.as_number()) {
        return Some(Width::exact(size as u32));
    }
    let w = style.get("width")?;
    if let Some(n) = w.as_number() {
        Some(Width::exact(n as u32))
    } else if w.as_tag() == Some("Fill") {
        Some(Width::fill())
    } else {
        None
    }
}

fn extract_width(value: &Value) -> Option<Width<'static>> {
    extract_width_from_fields(get_fields(value)?)
}

/// Extract Height from element fields' style.
fn extract_height_from_fields(fields: &Fields) -> Option<Height<'static>> {
    let style = get_style_obj(fields)?;
    if let Some(size) = style.get("size").and_then(|v| v.as_number()) {
        return Some(Height::exact(size as u32));
    }
    let h = style.get("height")?;
    if let Some(n) = h.as_number() {
        Some(Height::exact(n as u32))
    } else if h.as_tag() == Some("Fill") {
        Some(Height::fill())
    } else {
        None
    }
}

fn extract_height(value: &Value) -> Option<Height<'static>> {
    extract_height_from_fields(get_fields(value)?)
}

/// Extract Padding from element fields' style.
fn extract_padding_from_fields(fields: &Fields) -> Option<Padding<'static>> {
    let style = get_style_obj(fields)?;
    let padding_val = style.get("padding")?;
    match padding_val {
        Value::Number(n) => Some(Padding::all(n.0 as u32)),
        Value::Object(padding) => {
            let row = padding
                .get("row")
                .and_then(|v| v.as_number())
                .unwrap_or(0.0);
            let col = padding
                .get("column")
                .and_then(|v| v.as_number())
                .unwrap_or(0.0);
            let top = padding
                .get("top")
                .and_then(|v| v.as_number())
                .unwrap_or(col) as u32;
            let bottom = padding
                .get("bottom")
                .and_then(|v| v.as_number())
                .unwrap_or(col) as u32;
            let left = padding
                .get("left")
                .and_then(|v| v.as_number())
                .unwrap_or(row) as u32;
            let right = padding
                .get("right")
                .and_then(|v| v.as_number())
                .unwrap_or(row) as u32;
            Some(Padding::new().top(top).right(right).bottom(bottom).left(left))
        }
        _ => None,
    }
}

fn extract_padding(value: &Value) -> Option<Padding<'static>> {
    extract_padding_from_fields(get_fields(value)?)
}

/// Extract Background from element fields' style.
fn extract_background_from_fields(fields: &Fields) -> Option<Background<'static>> {
    let style = get_style_obj(fields)?;
    let bg = match style.get("background") {
        Some(Value::Object(bg)) => bg,
        _ => return None,
    };
    let mut background = Background::new();
    let mut has_any = false;
    if let Some(color_css) = bg.get("color").and_then(value_to_css_color) {
        background = background.color(color_css);
        has_any = true;
    }
    if let Some(url) = bg.get("url").and_then(|v| v.as_text()) {
        background = background.url(url.to_string());
        has_any = true;
    }
    if has_any {
        Some(background)
    } else {
        None
    }
}

fn extract_background(value: &Value) -> Option<Background<'static>> {
    extract_background_from_fields(get_fields(value)?)
}

/// Extract background color from a Value (for signal closures).
fn extract_bg_color(value: &Value) -> Option<String> {
    let fields = get_fields(value)?;
    let style = get_style_obj(fields)?;
    let bg = match style.get("background") {
        Some(Value::Object(bg)) => bg,
        _ => return None,
    };
    bg.get("color").and_then(value_to_css_color)
}

/// Extract background URL from a Value (for signal closures).
fn extract_bg_url(value: &Value) -> Option<String> {
    let fields = get_fields(value)?;
    let style = get_style_obj(fields)?;
    let bg = match style.get("background") {
        Some(Value::Object(bg)) => bg,
        _ => return None,
    };
    bg.get("url").and_then(|v| v.as_text()).map(|s| s.to_string())
}

/// Extract RoundedCorners from element fields' style.
fn extract_rounded_corners_from_fields(fields: &Fields) -> Option<RoundedCorners> {
    let style = get_style_obj(fields)?;
    style
        .get("rounded_corners")
        .and_then(|v| v.as_number())
        .map(|n| RoundedCorners::all(n as u32))
}

fn extract_rounded_corners(value: &Value) -> Option<RoundedCorners> {
    extract_rounded_corners_from_fields(get_fields(value)?)
}

/// Extract Transform from element fields' style.
fn extract_transform_from_fields(fields: &Fields) -> Option<Transform> {
    let style = get_style_obj(fields)?;
    let transform = match style.get("transform") {
        Some(Value::Object(t)) => t,
        _ => return None,
    };
    let move_right = transform
        .get("move_right")
        .and_then(|v| v.as_number())
        .unwrap_or(0.0);
    let move_down = transform
        .get("move_down")
        .and_then(|v| v.as_number())
        .unwrap_or(0.0);
    let move_left = transform
        .get("move_left")
        .and_then(|v| v.as_number())
        .unwrap_or(0.0);
    let move_up = transform
        .get("move_up")
        .and_then(|v| v.as_number())
        .unwrap_or(0.0);
    let rotate = transform
        .get("rotate")
        .and_then(|v| v.as_number())
        .unwrap_or(0.0);
    let has_transform =
        move_right != 0.0 || move_down != 0.0 || move_left != 0.0 || move_up != 0.0 || rotate != 0.0;
    if !has_transform {
        return None;
    }
    let mut t = Transform::new();
    if move_right != 0.0 {
        t = t.move_right(move_right);
    }
    if move_left != 0.0 {
        t = t.move_left(move_left);
    }
    if move_down != 0.0 {
        t = t.move_down(move_down);
    }
    if move_up != 0.0 {
        t = t.move_up(move_up);
    }
    if rotate != 0.0 {
        t = t.rotate(rotate);
    }
    Some(t)
}

fn extract_transform(value: &Value) -> Option<Transform> {
    extract_transform_from_fields(get_fields(value)?)
}

/// Extract Outline from element fields' style.
fn extract_outline_from_fields(fields: &Fields) -> Option<Outline> {
    let style = get_style_obj(fields)?;
    let outline = style.get("outline")?;
    match outline {
        Value::Tag(t) if t.as_ref() == "NoOutline" => None,
        Value::Object(obj) => {
            let color_css = obj.get("color").and_then(value_to_css_color)?;
            let side = obj
                .get("side")
                .and_then(|v| v.as_tag())
                .unwrap_or("Outer");
            let mut o = if side == "Inner" {
                Outline::inner()
            } else {
                Outline::outer()
            };
            o = o.color(color_css);
            Some(o)
        }
        _ => None,
    }
}

fn extract_outline(value: &Value) -> Option<Outline> {
    extract_outline_from_fields(get_fields(value)?)
}

/// Extract Shadows from element fields' style.
fn extract_shadows_from_fields(fields: &Fields) -> Option<Vec<Shadow>> {
    let style = get_style_obj(fields)?;
    if let Some(Value::Tagged {
        tag,
        fields: list_fields,
    }) = style.get("shadows")
    {
        if tag.as_ref() == LIST_TAG {
            let mut shadows = Vec::new();
            for item in list_fields.values() {
                if let Value::Object(shadow_obj) = item {
                    let x = shadow_obj
                        .get("x")
                        .and_then(|v| v.as_number())
                        .unwrap_or(0.0) as i32;
                    let y = shadow_obj
                        .get("y")
                        .and_then(|v| v.as_number())
                        .unwrap_or(0.0) as i32;
                    let blur = shadow_obj
                        .get("blur")
                        .and_then(|v| v.as_number())
                        .unwrap_or(0.0) as u32;
                    let spread = shadow_obj
                        .get("spread")
                        .and_then(|v| v.as_number())
                        .unwrap_or(0.0) as i32;
                    let inset = shadow_obj
                        .get("direction")
                        .and_then(|v| v.as_tag())
                        .map(|t| t == "Inwards")
                        .unwrap_or(false);
                    let color_css = shadow_obj
                        .get("color")
                        .and_then(value_to_css_color);
                    let mut shadow = Shadow::new().x(x).y(y).blur(blur).spread(spread);
                    if inset {
                        shadow = shadow.inner();
                    }
                    if let Some(css) = color_css {
                        shadow = shadow.color(css);
                    }
                    shadows.push(shadow);
                }
            }
            if !shadows.is_empty() {
                return Some(shadows);
            }
        }
    }
    None
}

fn extract_shadows(value: &Value) -> Option<Vec<Shadow>> {
    extract_shadows_from_fields(get_fields(value)?)
}

/// Extract Borders from element fields' style.
fn extract_border_side(side_obj: &Fields) -> Option<Border> {
    let width = side_obj
        .get("width")
        .and_then(|v| v.as_number())
        .unwrap_or(1.0) as u32;
    let color_css = side_obj.get("color").and_then(value_to_css_color)?;
    Some(Border::new().width(width).color(color_css))
}

fn extract_borders_from_fields(fields: &Fields) -> Option<Borders<'static>> {
    let style = get_style_obj(fields)?;
    let borders = match style.get("borders") {
        Some(Value::Object(b)) => b,
        _ => return None,
    };
    let mut result = Borders::new();
    let mut has_any = false;
    if let Some(Value::Object(top)) = borders.get("top") {
        if let Some(b) = extract_border_side(top) {
            result = result.top(b);
            has_any = true;
        }
    }
    if let Some(Value::Object(bottom)) = borders.get("bottom") {
        if let Some(b) = extract_border_side(bottom) {
            result = result.bottom(b);
            has_any = true;
        }
    }
    if let Some(Value::Object(left)) = borders.get("left") {
        if let Some(b) = extract_border_side(left) {
            result = result.left(b);
            has_any = true;
        }
    }
    if let Some(Value::Object(right)) = borders.get("right") {
        if let Some(b) = extract_border_side(right) {
            result = result.right(b);
            has_any = true;
        }
    }
    if has_any {
        Some(result)
    } else {
        None
    }
}

fn extract_borders(value: &Value) -> Option<Borders<'static>> {
    extract_borders_from_fields(get_fields(value)?)
}

/// Extract AlignContent from element fields' style.
fn extract_align_content_from_fields(fields: &Fields) -> Option<AlignContent> {
    let style = get_style_obj(fields)?;
    let align = match style.get("align") {
        Some(Value::Object(a)) => a,
        _ => return None,
    };
    let mut result = AlignContent::new();
    let mut has_any = false;
    if let Some(tag) = align.get("row").and_then(|v| v.as_tag()) {
        result = match tag {
            "Center" => result.center_x(),
            "Start" | "Left" => result.left(),
            "End" | "Right" => result.right(),
            _ => result,
        };
        has_any = true;
    }
    if let Some(tag) = align.get("column").and_then(|v| v.as_tag()) {
        result = match tag {
            "Center" => result.center_y(),
            "Top" | "Start" => result.top(),
            "Bottom" | "End" => result.bottom(),
            _ => result,
        };
        has_any = true;
    }
    if has_any {
        Some(result)
    } else {
        None
    }
}

fn extract_align_content(value: &Value) -> Option<AlignContent> {
    extract_align_content_from_fields(get_fields(value)?)
}

/// Extract self-alignment (Align) from style.align when it's a simple Tag (e.g., Right, Center).
/// This differs from AlignContent which positions content INSIDE the element;
/// Align positions the element ITSELF within its parent (margin-left: auto, etc.).
fn extract_self_align(value: &Value) -> Option<Align> {
    let fields = get_fields(value)?;
    let style = get_style_obj(fields)?;
    match style.get("align") {
        Some(Value::Tag(tag)) => match tag.as_ref() {
            "Right" | "End" => Some(Align::new().right()),
            "Left" | "Start" => Some(Align::new().left()),
            "Center" => Some(Align::new().center_x()),
            "Top" => Some(Align::new().top()),
            "Bottom" => Some(Align::new().bottom()),
            _ => None,
        },
        _ => None,
    }
}

/// Apply all Zoon typed style signals from a Mutable<Value> to any element.
/// Returns a closure suitable for `update_raw_el`.
fn apply_zoon_style_signals<T: RawEl>(
    raw_el: T,
    vm: &Mutable<Value>,
) -> T {
    raw_el
        .style_signal("padding", vm.signal_cloned().map(|v| {
            let fields = get_fields(&v)?;
            let style = get_style_obj(fields)?;
            let padding_val = style.get("padding")?;
            match padding_val {
                Value::Number(n) => Some(format!("{}px", n.0)),
                Value::Object(padding) => {
                    let row = padding.get("row").and_then(|v| v.as_number()).unwrap_or(0.0);
                    let col = padding.get("column").and_then(|v| v.as_number()).unwrap_or(0.0);
                    let top = padding.get("top").and_then(|v| v.as_number()).unwrap_or(col);
                    let bottom = padding.get("bottom").and_then(|v| v.as_number()).unwrap_or(col);
                    let left = padding.get("left").and_then(|v| v.as_number()).unwrap_or(row);
                    let right = padding.get("right").and_then(|v| v.as_number()).unwrap_or(row);
                    Some(format!("{}px {}px {}px {}px", top, right, bottom, left))
                }
                _ => None,
            }
        }))
        .style_signal("box-shadow", vm.signal_cloned().map(|v| {
            let fields = get_fields(&v)?;
            let style = get_style_obj(fields)?;
            if let Some(Value::Tagged {
                tag,
                fields: list_fields,
            }) = style.get("shadows")
            {
                if tag.as_ref() == LIST_TAG {
                    let mut shadow_parts = Vec::new();
                    for item in list_fields.values() {
                        if let Value::Object(shadow_obj) = item {
                            let x = shadow_obj
                                .get("x")
                                .and_then(|v| v.as_number())
                                .unwrap_or(0.0);
                            let y = shadow_obj
                                .get("y")
                                .and_then(|v| v.as_number())
                                .unwrap_or(0.0);
                            let blur = shadow_obj
                                .get("blur")
                                .and_then(|v| v.as_number())
                                .unwrap_or(0.0);
                            let spread = shadow_obj
                                .get("spread")
                                .and_then(|v| v.as_number())
                                .unwrap_or(0.0);
                            let inset = shadow_obj
                                .get("direction")
                                .and_then(|v| v.as_tag())
                                .map(|t| t == "Inwards")
                                .unwrap_or(false);
                            let color_css = shadow_obj
                                .get("color")
                                .and_then(value_to_css_color)
                                .unwrap_or_else(|| "rgba(0,0,0,0.5)".to_string());
                            let inset_str = if inset { "inset " } else { "" };
                            shadow_parts.push(format!(
                                "{}{}px {}px {}px {}px {}",
                                inset_str, x, y, blur, spread, color_css
                            ));
                        }
                    }
                    if !shadow_parts.is_empty() {
                        return Some(shadow_parts.join(", "));
                    }
                }
            }
            None
        }))
        .style_signal("border-top", vm.signal_cloned().map(|v| css_border_side(&v, "top")))
        .style_signal("border-bottom", vm.signal_cloned().map(|v| css_border_side(&v, "bottom")))
        .style_signal("border-left", vm.signal_cloned().map(|v| css_border_side(&v, "left")))
        .style_signal("border-right", vm.signal_cloned().map(|v| css_border_side(&v, "right")))
        .style_signal("line-height", vm.signal_cloned().map(|v| {
            let style = get_style_obj(get_fields(&v)?)?;
            let lh = style.get("line_height")?.as_number()?;
            Some(format!("{}", lh))
        }))
        .style_signal("text-shadow", vm.signal_cloned().map(|v| {
            let style = get_style_obj(get_fields(&v)?)?;
            let shadow = match style.get("text_shadow")? {
                Value::Object(obj) => obj,
                _ => return None,
            };
            let x = shadow.get("x").and_then(|v| v.as_number()).unwrap_or(0.0);
            let y = shadow.get("y").and_then(|v| v.as_number()).unwrap_or(0.0);
            let blur = shadow.get("blur").and_then(|v| v.as_number()).unwrap_or(0.0);
            let color_css = shadow.get("color")
                .and_then(value_to_css_color)
                .unwrap_or_else(|| "rgba(0,0,0,0.5)".to_string());
            Some(format!("{}px {}px {}px {}", x, y, blur, color_css))
        }))
        .style_signal("-webkit-font-smoothing", vm.signal_cloned().map(|v| {
            let style = get_style_obj(get_fields(&v)?)?;
            let val = style.get("font_smoothing")?;
            match val.as_tag()? {
                "Antialiased" => Some("antialiased".to_string()),
                _ => None,
            }
        }))
        .style_signal("text-decoration", vm.signal_cloned().map(|v| {
            let style = get_style_obj(get_fields(&v)?)?;
            let font = match style.get("font")? {
                Value::Object(f) => f.as_ref(),
                Value::Tagged { fields, .. } => fields.as_ref(),
                _ => return None,
            };
            let line = match font.get("line")? {
                Value::Object(l) => l.as_ref(),
                _ => return None,
            };
            let strike = line.get("strikethrough").and_then(|v| v.as_bool()).unwrap_or(false);
            let underline = line.get("underline").and_then(|v| v.as_bool()).unwrap_or(false);
            match (strike, underline) {
                (true, true) => Some("line-through underline".to_string()),
                (true, false) => Some("line-through".to_string()),
                (false, true) => Some("underline".to_string()),
                (false, false) => Some("none".to_string()),
            }
        }))
}

/// CSS border-side extractor (kept for raw CSS signal use in retained tree).
fn css_border_side(value: &Value, side: &str) -> Option<String> {
    let style = get_style_obj(get_fields(value)?)?;
    let borders = match style.get("borders") {
        Some(Value::Object(b)) => b,
        _ => return None,
    };
    let side_obj = match borders.get(side) {
        Some(Value::Object(s)) => s,
        _ => return None,
    };
    let width = side_obj
        .get("width")
        .and_then(|v| v.as_number())
        .unwrap_or(1.0);
    let color_css = side_obj
        .get("color")
        .and_then(value_to_css_color)
        .unwrap_or_else(|| "currentColor".to_string());
    Some(format!("{}px solid {}", width, color_css))
}

// ═══════════════════════════════════════════════════════════════════════════
// Retained tree types
// ═══════════════════════════════════════════════════════════════════════════

/// Keyed items for O(1) per-item updates in Stripes with keyed list display.
///
/// Items are ordered by ListKey (sorted). Updates from keyed diffs
/// target individual items by key without scanning the entire list.
struct KeyedItems {
    /// Items ordered by key: (key_str, retained_node).
    items: Vec<(Arc<str>, RetainedNode)>,
    /// O(1) key → index lookup.
    key_to_index: HashMap<Arc<str>, usize>,
}

impl KeyedItems {
    fn new() -> Self {
        KeyedItems {
            items: Vec::new(),
            key_to_index: HashMap::new(),
        }
    }

    /// Apply keyed diffs to the retained items.
    fn apply(
        &mut self,
        diffs: &[KeyedDiff],
        tx: &mpsc::UnboundedSender<VecDiff<RawElOrText>>,
        handle: &DdWorkerHandle,
        stripe_link_path: &str,
    ) {
        for diff in diffs {
            match diff {
                KeyedDiff::Upsert { key, value } => {
                    let key_str = key.0.clone();
                    if let Some(&idx) = self.key_to_index.get(&key_str) {
                        // Update existing item in place (O(1) — HashMap lookup + Mutable set)
                        let child_lp = self.child_link_path(value, stripe_link_path, &key_str);
                        self.items[idx].1.update(value, handle, &child_lp);
                    } else {
                        // New item — insert at sorted position
                        let insert_pos = self.items.iter()
                            .position(|(k, _)| k.as_ref() > key_str.as_ref())
                            .unwrap_or(self.items.len());
                        let child_lp = self.child_link_path(value, stripe_link_path, &key_str);
                        let (el, node) = build_retained_node(value, handle, &child_lp);
                        // Increment indices of items shifted right by the insert
                        for (_, idx) in self.key_to_index.iter_mut() {
                            if *idx >= insert_pos { *idx += 1; }
                        }
                        self.key_to_index.insert(key_str.clone(), insert_pos);
                        self.items.insert(insert_pos, (key_str, node));
                        tx.unbounded_send(VecDiff::InsertAt {
                            index: insert_pos,
                            value: el,
                        }).ok();
                    }
                }
                KeyedDiff::Remove { key } => {
                    let key_str = key.0.clone();
                    if let Some(&idx) = self.key_to_index.get(&key_str) {
                        self.items.remove(idx);
                        self.key_to_index.remove(&key_str);
                        // Decrement indices of items shifted left by the removal
                        for (_, i) in self.key_to_index.iter_mut() {
                            if *i > idx { *i -= 1; }
                        }
                        tx.unbounded_send(VecDiff::RemoveAt { index: idx }).ok();
                    }
                }
            }
        }
    }

    fn find_index(&self, key: &str) -> Option<usize> {
        self.key_to_index.get(key).copied()
    }

    /// Extract link path from item's __link_path__ field, or construct from key.
    fn child_link_path(&self, value: &Value, stripe_link_path: &str, key: &str) -> String {
        get_fields(value)
            .and_then(|f| f.get(LINK_PATH_FIELD))
            .and_then(|v| v.as_text())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{}.items.{}", stripe_link_path, key))
    }
}

/// A retained element tree for efficient incremental DOM updates.
///
/// Built once from the initial Value. On state changes, `update()` diffs
/// old vs new Value tree and updates only changed Mutables.
pub struct RetainedTree {
    root: RetainedNode,
}

enum RetainedNode {
    /// Plain text / number / tag / bool
    Primitive {
        text: Mutable<String>,
    },
    Button {
        value: Mutable<Value>,
        link_path: Rc<RefCell<String>>,
    },
    Stripe {
        value: Mutable<Value>,
        items: Vec<RetainedNode>,
        items_tx: mpsc::UnboundedSender<VecDiff<RawElOrText>>,
        link_path: Rc<RefCell<String>>,
        /// Keyed items for O(1) updates (Some when display pipeline is active).
        keyed_items: Option<KeyedItems>,
    },
    Stack {
        value: Mutable<Value>,
        layers: Vec<RetainedNode>,
        layers_tx: mpsc::UnboundedSender<VecDiff<RawElOrText>>,
    },
    Container {
        value: Mutable<Value>,
        child: Option<Box<RetainedNode>>,
        child_tx: mpsc::UnboundedSender<VecDiff<RawElOrText>>,
    },
    TextInput {
        value: Mutable<Value>,
        link_path: Rc<RefCell<String>>,
        dom_element: Rc<RefCell<Option<web_sys::HtmlInputElement>>>,
    },
    Checkbox {
        value: Mutable<Value>,
        link_path: Rc<RefCell<String>>,
    },
    Label {
        value: Mutable<Value>,
        link_path: Rc<RefCell<String>>,
    },
    Paragraph {
        value: Mutable<Value>,
        contents: Vec<RetainedNode>,
        contents_tx: mpsc::UnboundedSender<VecDiff<RawElOrText>>,
    },
    Link {
        value: Mutable<Value>,
        link_path: Rc<RefCell<String>>,
    },
    Document {
        child: Option<Box<RetainedNode>>,
        child_tx: mpsc::UnboundedSender<VecDiff<RawElOrText>>,
    },
    Empty,
}

impl RetainedNode {
    /// Check if this retained node can handle an update from the given value
    /// without needing to be replaced.
    fn matches_value_type(&self, value: &Value) -> bool {
        match (self, value) {
            (RetainedNode::Primitive { .. }, Value::Number(_))
            | (RetainedNode::Primitive { .. }, Value::Text(_))
            | (RetainedNode::Primitive { .. }, Value::Bool(_))
            | (RetainedNode::Primitive { .. }, Value::Unit) => true,
            (RetainedNode::Primitive { .. }, Value::Tag(_)) => true,
            (RetainedNode::Button { .. }, Value::Tagged { tag, .. }) => {
                tag.as_ref() == "ElementButton"
            }
            (RetainedNode::Stripe { .. }, Value::Tagged { tag, .. }) => {
                tag.as_ref() == "ElementStripe"
            }
            (RetainedNode::Stack { .. }, Value::Tagged { tag, .. }) => {
                tag.as_ref() == "ElementStack"
            }
            (RetainedNode::Container { .. }, Value::Tagged { tag, .. }) => {
                tag.as_ref() == "ElementContainer"
            }
            (RetainedNode::TextInput { .. }, Value::Tagged { tag, .. }) => {
                tag.as_ref() == "ElementTextInput"
            }
            (RetainedNode::Checkbox { .. }, Value::Tagged { tag, .. }) => {
                tag.as_ref() == "ElementCheckbox"
            }
            (RetainedNode::Label { .. }, Value::Tagged { tag, .. }) => {
                tag.as_ref() == "ElementLabel"
            }
            (RetainedNode::Paragraph { .. }, Value::Tagged { tag, .. }) => {
                tag.as_ref() == "ElementParagraph"
            }
            (RetainedNode::Link { .. }, Value::Tagged { tag, .. }) => {
                tag.as_ref() == "ElementLink"
            }
            (RetainedNode::Document { .. }, Value::Tagged { tag, .. }) => {
                tag.as_ref() == "DocumentNew"
            }
            (RetainedNode::Empty, _) => false,
            _ => false,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Build functions — create the initial retained tree from a Value
// ═══════════════════════════════════════════════════════════════════════════

/// Build a retained tree from a document Value.
pub fn build_retained_tree(value: &Value, handle: &DdWorkerHandle) -> (RawElOrText, RetainedTree) {
    let (element, root) = build_retained_node(value, handle, "");
    (element, RetainedTree { root })
}

fn build_retained_node(
    value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    match value {
        Value::Number(_) | Value::Text(_) | Value::Tag(_) | Value::Bool(_) | Value::Unit => {
            build_retained_primitive(value)
        }
        Value::Tagged { tag, fields } => {
            build_retained_tagged(tag, fields, value, handle, link_path)
        }
        Value::Object(_) => build_retained_primitive(value),
    }
}

fn build_retained_primitive(value: &Value) -> (RawElOrText, RetainedNode) {
    let text_str = match value {
        Value::Tag(s) if s.as_ref() == "NoElement" || s.as_ref() == "SKIP" => String::new(),
        _ => value.to_display_string(),
    };
    let text_mutable = Mutable::new(text_str);
    let el = El::new().child_signal(text_mutable.signal_cloned().map(|t| {
        if t.is_empty() {
            None
        } else {
            Some(zoon::Text::with_signal(always(t)))
        }
    }));
    (el.unify(), RetainedNode::Primitive { text: text_mutable })
}

fn build_retained_tagged(
    tag: &str,
    fields: &Arc<Fields>,
    full_value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    match tag {
        "ElementButton" => build_retained_button(fields, full_value, handle, link_path),
        "ElementStripe" => build_retained_stripe(fields, full_value, handle, link_path),
        "ElementStack" => build_retained_stack(fields, full_value, handle, link_path),
        "ElementContainer" => build_retained_container(fields, full_value, handle, link_path),
        "ElementLabel" => build_retained_label(fields, full_value, handle, link_path),
        "ElementTextInput" => build_retained_text_input(fields, full_value, handle, link_path),
        "ElementCheckbox" => build_retained_checkbox(fields, full_value, handle, link_path),
        "ElementParagraph" => build_retained_paragraph(fields, full_value, handle, link_path),
        "ElementLink" => build_retained_link(fields, full_value, handle, link_path),
        "DocumentNew" => {
            let (child_tx, child_rx) = mpsc::unbounded();
            let (child_opt, initial_elements) =
                if let Some(root) = fields.get("root") {
                    let (el, child) = build_retained_node(root, handle, link_path);
                    (Some(Box::new(child)), vec![el])
                } else {
                    (None, vec![])
                };
            child_tx
                .unbounded_send(VecDiff::Replace { values: initial_elements })
                .ok();
            let el = El::new()
                .s(Width::fill())
                .s(Height::fill())
                .update_raw_el(|raw_el| {
                    raw_el.children_signal_vec(VecDiffStreamSignalVec(child_rx))
                });
            (el.unify(), RetainedNode::Document { child: child_opt, child_tx })
        }
        _ => build_retained_primitive(&Value::text(format!("{}[...]", tag))),
    }
}

fn build_retained_button(
    fields: &Fields,
    full_value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    let vm = Mutable::new(full_value.clone());
    let effective_link = extract_effective_link(fields, link_path);
    let link_ref = Rc::new(RefCell::new(effective_link.clone()));
    let hover_link = extract_hover_link_path(fields, link_path);

    let el = Button::new()
        .update_raw_el(|raw_el| raw_el.attr("role", "button"))
        .label_signal(vm.signal_cloned().map(|v| {
            get_fields(&v)
                .and_then(|f| f.get("label"))
                .map(|l| l.to_display_string())
                .unwrap_or_default()
        }))
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_font(&v))))
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_align_font(&v))))
        .s(Align::with_signal_self(vm.signal_cloned().map(|v| extract_self_align(&v))))
        .s(Width::with_signal_self(vm.signal_cloned().map(|v| extract_width(&v))))
        .s(Height::with_signal_self(vm.signal_cloned().map(|v| extract_height(&v))))
        .s(Background::new()
            .color_signal(vm.signal_cloned().map(|v| extract_bg_color(&v)))
            .url_signal(vm.signal_cloned().map(|v| extract_bg_url(&v))))
        .s(RoundedCorners::all_signal(vm.signal_cloned().map(|v| {
            get_fields(&v)
                .and_then(|f| get_style_obj(f))
                .and_then(|s| s.get("rounded_corners"))
                .and_then(|v| v.as_number())
                .map(|n| n as u32)
        })))
        .s(Transform::with_signal_self(vm.signal_cloned().map(|v| extract_transform(&v))))
        .s(Outline::with_signal_self(vm.signal_cloned().map(|v| extract_outline(&v))))
        .s(AlignContent::with_signal_self(vm.signal_cloned().map(|v| extract_align_content(&v))))
        .update_raw_el({
            let vm = vm.clone();
            move |raw_el| apply_zoon_style_signals(raw_el, &vm)
        })
        .on_press({
            let handle_ref = handle.clone_ref();
            let lp = link_ref.clone();
            move || {
                let path = lp.borrow().clone();
                if !path.is_empty() {
                    handle_ref.inject_dd_event(Event::LinkPress { link_path: path });
                }
            }
        })
        .on_hovered_change({
            let handle_ref = handle.clone_ref();
            let hover_link = hover_link.clone();
            move |hovered| {
                if let Some(ref path) = hover_link {
                    if !path.is_empty() {
                        handle_ref.inject_dd_event(Event::HoverChange { link_path: path.clone(), hovered });
                    }
                }
            }
        });

    (
        el.unify(),
        RetainedNode::Button {
            value: vm,
            link_path: link_ref,
        },
    )
}

fn build_retained_stripe(
    fields: &Fields,
    full_value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    let vm = Mutable::new(full_value.clone());
    let (items_tx, items_rx) = mpsc::unbounded();
    let effective_link = extract_effective_link(fields, link_path);
    let link_ref = Rc::new(RefCell::new(effective_link));

    // Build initial items
    let item_values = extract_sorted_list_items(fields, "items");

    // Detect keyed Stripe by matching element tag from compiler.
    let keyed_mode = if let Some(keyed_tag) = handle.keyed_stripe_element_tag() {
        let el_tag = fields.get("element")
            .and_then(|e| e.get_field("tag"))
            .and_then(|t| t.as_tag());
        el_tag == Some(keyed_tag)
    } else {
        false
    };

    let mut retained_items = Vec::new();
    let mut keyed_items_init: Option<KeyedItems> = None;

    if keyed_mode {
        // Keyed Stripe: items arrive via keyed diffs, not from the document closure.
        // The document closure provides a stub list (correct count, placeholder values)
        // for structural checks like List/is_empty(). Skip building items here.
        keyed_items_init = Some(KeyedItems::new());
    } else {
        // Non-keyed: build items from the document value.
        let mut initial_elements = Vec::new();
        for (i, item) in item_values.iter().enumerate() {
            let child_lp = format!("{}.items.{}", link_path, i);
            let (el, node) = build_retained_node(item, handle, &child_lp);
            retained_items.push(node);
            initial_elements.push(el);
        }
        items_tx
            .unbounded_send(VecDiff::Replace {
                values: initial_elements,
            })
            .ok();
    }

    let el = Stripe::new()
        .direction_signal(vm.signal_cloned().map(|v| {
            let dir = get_fields(&v)
                .and_then(|f| f.get("direction"))
                .and_then(|v| v.as_tag());
            match dir {
                Some("Row") => Direction::Row,
                _ => Direction::Column,
            }
        }))
        .s(Gap::both_signal(vm.signal_cloned().map(|v| {
            get_fields(&v)
                .and_then(|f| f.get("gap"))
                .and_then(|v| v.as_number())
                .filter(|g| *g > 0.0)
                .map(|g| g as u32)
        })))
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_font(&v))))
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_align_font(&v))))
        .s(Width::with_signal_self(vm.signal_cloned().map(|v| extract_width(&v))))
        .s(Height::with_signal_self(vm.signal_cloned().map(|v| extract_height(&v))))
        .s(Background::new()
            .color_signal(vm.signal_cloned().map(|v| extract_bg_color(&v)))
            .url_signal(vm.signal_cloned().map(|v| extract_bg_url(&v))))
        .s(RoundedCorners::all_signal(vm.signal_cloned().map(|v| {
            get_fields(&v)
                .and_then(|f| get_style_obj(f))
                .and_then(|s| s.get("rounded_corners"))
                .and_then(|v| v.as_number())
                .map(|n| n as u32)
        })))
        .s(Transform::with_signal_self(vm.signal_cloned().map(|v| extract_transform(&v))))
        .s(Outline::with_signal_self(vm.signal_cloned().map(|v| extract_outline(&v))))
        .s(AlignContent::with_signal_self(vm.signal_cloned().map(|v| extract_align_content(&v))))
        .update_raw_el({
            let vm = vm.clone();
            move |raw_el| apply_zoon_style_signals(raw_el, &vm)
        })
        .items_signal_vec(VecDiffStreamSignalVec(items_rx));

    // Wire hover handlers via raw JS (same as current approach — see MEMORY.md)
    let hover_link = extract_hover_link_path(fields, link_path);
    let el = if let Some(hover_link) = hover_link {
        let handle_hover_in = handle.clone_ref();
        let link_hover_in = hover_link.clone();
        let handle_hover_out = handle.clone_ref();
        let link_hover_out = hover_link;
        el.update_raw_el(move |raw_el| {
            raw_el.after_insert(move |el: web_sys::HtmlElement| {
                let link_in = link_hover_in.clone();
                let handle_in = handle_hover_in.clone_ref();
                let enter_closure =
                    wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
                        handle_in.inject_dd_event(Event::HoverChange {
                            link_path: link_in.clone(),
                            hovered: true,
                        });
                    });
                el.add_event_listener_with_callback(
                    "mouseenter",
                    enter_closure.as_ref().unchecked_ref(),
                )
                .ok();
                enter_closure.forget();

                let link_out = link_hover_out.clone();
                let handle_out = handle_hover_out.clone_ref();
                let leave_closure =
                    wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
                        handle_out.inject_dd_event(Event::HoverChange {
                            link_path: link_out.clone(),
                            hovered: false,
                        });
                    });
                el.add_event_listener_with_callback(
                    "mouseleave",
                    leave_closure.as_ref().unchecked_ref(),
                )
                .ok();
                leave_closure.forget();
            })
        })
    } else {
        el
    };

    (
        el.unify(),
        RetainedNode::Stripe {
            value: vm,
            items: retained_items,
            items_tx,
            link_path: link_ref,
            keyed_items: keyed_items_init,
        },
    )
}

fn build_retained_stack(
    fields: &Fields,
    full_value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    let vm = Mutable::new(full_value.clone());
    let (layers_tx, layers_rx) = mpsc::unbounded();

    let layer_values = extract_sorted_list_items(fields, "layers");
    let mut retained_layers = Vec::new();
    let mut initial_elements = Vec::new();

    for (i, layer) in layer_values.iter().enumerate() {
        let child_lp = format!("{}.layers.{}", link_path, i);
        let (el, node) = build_retained_node(layer, handle, &child_lp);
        retained_layers.push(node);
        initial_elements.push(el);
    }

    layers_tx
        .unbounded_send(VecDiff::Replace {
            values: initial_elements,
        })
        .ok();

    let el = Stack::new()
        .s(Width::with_signal_self(vm.signal_cloned().map(|v| extract_width(&v))))
        .s(Height::with_signal_self(vm.signal_cloned().map(|v| extract_height(&v))))
        .s(Background::new()
            .color_signal(vm.signal_cloned().map(|v| extract_bg_color(&v))))
        .layers_signal_vec(VecDiffStreamSignalVec(layers_rx));

    (
        el.unify(),
        RetainedNode::Stack {
            value: vm,
            layers: retained_layers,
            layers_tx,
        },
    )
}

fn build_retained_container(
    fields: &Fields,
    full_value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    let vm = Mutable::new(full_value.clone());
    let (child_tx, child_rx) = mpsc::unbounded();

    let mut retained_child = None;
    let mut initial_elements = Vec::new();

    if let Some(child_val) = fields.get("child") {
        let (el, node) = build_retained_node(child_val, handle, link_path);
        retained_child = Some(Box::new(node));
        initial_elements.push(el);
    }

    child_tx
        .unbounded_send(VecDiff::Replace {
            values: initial_elements,
        })
        .ok();

    let el = El::new()
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_font(&v))))
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_align_font(&v))))
        .s(Width::with_signal_self(vm.signal_cloned().map(|v| extract_width(&v))))
        .s(Height::with_signal_self(vm.signal_cloned().map(|v| extract_height(&v))))
        .s(Background::new()
            .color_signal(vm.signal_cloned().map(|v| extract_bg_color(&v))))
        .s(RoundedCorners::all_signal(vm.signal_cloned().map(|v| {
            get_fields(&v)
                .and_then(|f| get_style_obj(f))
                .and_then(|s| s.get("rounded_corners"))
                .and_then(|v| v.as_number())
                .map(|n| n as u32)
        })))
        .s(Transform::with_signal_self(vm.signal_cloned().map(|v| extract_transform(&v))))
        .s(Outline::with_signal_self(vm.signal_cloned().map(|v| extract_outline(&v))))
        .s(AlignContent::with_signal_self(vm.signal_cloned().map(|v| extract_align_content(&v))))
        .update_raw_el({
            let vm = vm.clone();
            let child_rx = child_rx;
            move |raw_el| {
                let raw_el = apply_zoon_style_signals(raw_el, &vm);
                raw_el.children_signal_vec(VecDiffStreamSignalVec(child_rx))
            }
        });

    (
        el.unify(),
        RetainedNode::Container {
            value: vm,
            child: retained_child,
            child_tx,
        },
    )
}

fn build_retained_label(
    fields: &Fields,
    full_value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    let vm = Mutable::new(full_value.clone());
    let effective_link = fields
        .get(LINK_PATH_FIELD)
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_else(|| link_path.to_string());
    let link_ref = Rc::new(RefCell::new(effective_link.clone()));
    let hover_link = extract_hover_link_path(fields, link_path);

    let el = Label::new()
        .label_signal(vm.signal_cloned().map(|v| {
            let fields = get_fields(&v);
            let label = fields
                .and_then(|f| f.get("label"))
                .map(|l| l.to_display_string())
                .unwrap_or_default();
            // Extract text-decoration from font.line (strikethrough/underline)
            // Must be applied to inner El because CSS text-decoration doesn't propagate to block children
            let text_decoration = fields
                .and_then(|f| get_style_obj(f))
                .and_then(|s| match s.get("font")? {
                    Value::Object(f) => Some(f.as_ref()),
                    Value::Tagged { fields, .. } => Some(fields.as_ref()),
                    _ => None,
                })
                .and_then(|f| match f.get("line")? {
                    Value::Object(l) => Some(l.as_ref()),
                    _ => None,
                })
                .map(|line| {
                    let strike = line.get("strikethrough").and_then(|v| v.as_bool()).unwrap_or(false);
                    let underline = line.get("underline").and_then(|v| v.as_bool()).unwrap_or(false);
                    match (strike, underline) {
                        (true, true) => "line-through underline",
                        (true, false) => "line-through",
                        (false, true) => "underline",
                        _ => "none",
                    }
                });
            let el = El::new().child(label);
            if let Some(td) = text_decoration {
                el.update_raw_el(|raw_el| raw_el.style("text-decoration", td))
            } else {
                el
            }
        }))
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_font(&v))))
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_align_font(&v))))
        .update_raw_el({
            let vm = vm.clone();
            move |raw_el| apply_zoon_style_signals(raw_el, &vm)
        })
        .on_double_click({
            let handle_ref = handle.clone_ref();
            let lp = link_ref.clone();
            move || {
                let path = lp.borrow().clone();
                if !path.is_empty() {
                    handle_ref.inject_dd_event(Event::DoubleClick { link_path: path });
                }
            }
        })
        .on_hovered_change({
            let handle_ref = handle.clone_ref();
            let hover_link = hover_link.clone();
            move |hovered| {
                if let Some(ref path) = hover_link {
                    if !path.is_empty() {
                        handle_ref.inject_dd_event(Event::HoverChange { link_path: path.clone(), hovered });
                    }
                }
            }
        });

    (
        el.unify(),
        RetainedNode::Label {
            value: vm,
            link_path: link_ref,
        },
    )
}

fn build_retained_text_input(
    fields: &Fields,
    full_value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    let vm = Mutable::new(full_value.clone());
    let effective_link = fields
        .get(LINK_PATH_FIELD)
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_else(|| link_path.to_string());
    let link_ref = Rc::new(RefCell::new(effective_link.clone()));
    let dom_el: Rc<RefCell<Option<web_sys::HtmlInputElement>>> = Default::default();

    let text = fields
        .get("text")
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let focus = fields
        .get("focus")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let input = TextInput::new()
        .label_hidden("text input")
        .focus_signal(vm.signal_cloned().map(|v| {
            get_fields(&v)
                .and_then(|f| f.get("focus"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        }))
        .text_signal(vm.signal_cloned().map(|v| {
            get_fields(&v)
                .and_then(|f| f.get("text"))
                .and_then(|v| v.as_text())
                .map(|s| s.to_string())
                .unwrap_or_default()
        }))
        .placeholder(Placeholder::with_signal(vm.signal_cloned().map(|v| {
            get_fields(&v)
                .and_then(|f| f.get("placeholder"))
                .and_then(|v| v.get_field("text"))
                .and_then(|v| v.as_text())
                .map(|s| s.to_string())
                .unwrap_or_default()
        })).s(Font::new().italic()))
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_font(&v))))
        .s(Width::with_signal_self(vm.signal_cloned().map(|v| extract_width(&v))))
        .s(Background::new()
            .color_signal(vm.signal_cloned().map(|v| extract_bg_color(&v))))
        .update_raw_el({
            let dom_el_ref = dom_el.clone();
            let initial_text = text.clone();
            let initial_focus = focus;
            let vm = vm.clone();
            move |raw_el| {
                let raw_el = apply_zoon_style_signals(raw_el, &vm);
                raw_el.after_insert(move |input_el: web_sys::HtmlInputElement| {
                    if initial_focus {
                        input_el.set_value(&initial_text);
                        let text_len: u32 = initial_text.len().try_into().unwrap_or(0);
                        input_el.set_selection_range(text_len, text_len).ok();
                    }
                    *dom_el_ref.borrow_mut() = Some(input_el);
                })
            }
        })
        .on_key_down_event({
            let handle_ref = handle.clone_ref();
            let lp = link_ref.clone();
            move |event| {
                let base = lp.borrow().clone();
                if !base.is_empty() {
                    let key = match event.key() {
                        Key::Enter => "Enter".to_string(),
                        Key::Escape => "Escape".to_string(),
                        Key::Other(k) => k.clone(),
                    };
                    let path = format!("{}.event.key_down", base);
                    handle_ref.inject_dd_event(Event::KeyDown { link_path: path, key });
                }
            }
        })
        .on_change({
            let handle_ref = handle.clone_ref();
            let lp = link_ref.clone();
            move |text| {
                let base = lp.borrow().clone();
                if !base.is_empty() {
                    let path = format!("{}.event.change", base);
                    handle_ref.inject_dd_event(Event::TextChange { link_path: path, text });
                }
            }
        })
        .on_blur({
            let handle_ref = handle.clone_ref();
            let lp = link_ref.clone();
            move || {
                let base = lp.borrow().clone();
                if !base.is_empty() {
                    let path = format!("{}.event.blur", base);
                    handle_ref.inject_dd_event(Event::Blur { link_path: path });
                }
            }
        })
        .update_raw_el({
            let handle_ref = handle.clone_ref();
            let lp = link_ref.clone();
            move |raw_el| {
                raw_el.event_handler(move |_: events::Focus| {
                    let base = lp.borrow().clone();
                    if !base.is_empty() {
                        let path = format!("{}.event.focus", base);
                        handle_ref.inject_dd_event(Event::Focus { link_path: path });
                    }
                })
            }
        });

    (
        input.unify(),
        RetainedNode::TextInput {
            value: vm,
            link_path: link_ref,
            dom_element: dom_el,
        },
    )
}

fn build_retained_checkbox(
    fields: &Fields,
    full_value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    let vm = Mutable::new(full_value.clone());
    let effective_link = fields
        .get(LINK_PATH_FIELD)
        .and_then(|v| v.as_text())
        .map(|s| s.to_string())
        .unwrap_or_else(|| link_path.to_string());
    let link_ref = Rc::new(RefCell::new(effective_link.clone()));

    let has_icon = fields.get("icon").is_some()
        && !matches!(fields.get("icon"), Some(Value::Tag(t)) if t.as_ref() == "NoElement");

    let el = if has_icon {
        // Icon checkbox — render as El with role="checkbox"
        let el = El::new()
            .update_raw_el(|raw_el| raw_el.attr("role", "checkbox"))
            .update_raw_el({
                let vm = vm.clone();
                move |raw_el| {
                    raw_el.attr_signal(
                        "aria-checked",
                        vm.signal_cloned().map(|v| {
                            let checked = get_fields(&v)
                                .and_then(|f| f.get("checked"))
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            Some(if checked {
                                "true".to_string()
                            } else {
                                "false".to_string()
                            })
                        }),
                    )
                }
            })
            .child_signal(vm.signal_cloned().map(move |v| {
                let icon = get_fields(&v).and_then(|f| f.get("icon"))?;
                if matches!(icon, Value::Tag(t) if t.as_ref() == "NoElement") {
                    return None;
                }
                Some(render_value_static(icon))
            }));

        let el = el
            .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_font(&v))))
            .s(Width::with_signal_self(vm.signal_cloned().map(|v| extract_width(&v))))
            .s(Height::with_signal_self(vm.signal_cloned().map(|v| extract_height(&v))))
            .s(RoundedCorners::all_signal(vm.signal_cloned().map(|v| {
                get_fields(&v)
                    .and_then(|f| get_style_obj(f))
                    .and_then(|s| s.get("rounded_corners"))
                    .and_then(|v| v.as_number())
                    .map(|n| n as u32)
            })))
            .s(Transform::with_signal_self(vm.signal_cloned().map(|v| extract_transform(&v))))
            .s(Outline::with_signal_self(vm.signal_cloned().map(|v| extract_outline(&v))))
            .s(AlignContent::with_signal_self(vm.signal_cloned().map(|v| extract_align_content(&v))))
            .update_raw_el({
                let vm = vm.clone();
                move |raw_el| apply_zoon_style_signals(raw_el, &vm)
            });

        if !effective_link.is_empty() {
            let handle_ref = handle.clone_ref();
            let lp = link_ref.clone();
            el.update_raw_el(move |raw_el| {
                raw_el.after_insert(move |el: web_sys::HtmlElement| {
                    let closure = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
                        handle_ref.inject_dd_event(Event::LinkClick {
                            link_path: lp.borrow().clone(),
                        });
                    });
                    el.add_event_listener_with_callback(
                        "click",
                        closure.as_ref().unchecked_ref(),
                    ).ok();
                    closure.forget();
                })
            }).unify()
        } else {
            el.unify()
        }
    } else {
        // Standard checkbox — Zoon typed Checkbox
        let checkbox = Checkbox::new()
            .label_hidden("checkbox")
            .checked_signal(vm.signal_cloned().map(|v| {
                get_fields(&v)
                    .and_then(|f| f.get("checked"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
            }))
            .icon(|_| El::new());

        if !effective_link.is_empty() {
            let handle_ref = handle.clone_ref();
            let lp = link_ref.clone();
            checkbox.update_raw_el(move |raw_el| {
                raw_el.after_insert(move |el: web_sys::HtmlDivElement| {
                    let closure = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
                        handle_ref.inject_dd_event(Event::LinkClick {
                            link_path: lp.borrow().clone(),
                        });
                    });
                    el.add_event_listener_with_callback(
                        "click",
                        closure.as_ref().unchecked_ref(),
                    ).ok();
                    closure.forget();
                })
            }).unify()
        } else {
            checkbox.unify()
        }
    };

    (
        el,
        RetainedNode::Checkbox {
            value: vm,
            link_path: link_ref,
        },
    )
}

fn build_retained_paragraph(
    fields: &Fields,
    full_value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    let vm = Mutable::new(full_value.clone());
    let (contents_tx, contents_rx) = mpsc::unbounded();

    let content_values = extract_sorted_list_items(fields, "contents");
    let mut retained_contents = Vec::new();
    let mut initial_elements = Vec::new();

    for (i, item) in content_values.iter().enumerate() {
        let child_lp = format!("{}.contents.{}", link_path, i);
        let (el, node) = build_retained_node(item, handle, &child_lp);
        retained_contents.push(node);
        initial_elements.push(el);
    }

    contents_tx
        .unbounded_send(VecDiff::Replace {
            values: initial_elements,
        })
        .ok();

    let el = Paragraph::new()
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_font(&v))))
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_align_font(&v))))
        .update_raw_el({
            let vm = vm.clone();
            move |raw_el| apply_zoon_style_signals(raw_el, &vm)
        })
        .contents_signal_vec(VecDiffStreamSignalVec(contents_rx));

    (
        el.unify(),
        RetainedNode::Paragraph {
            value: vm,
            contents: retained_contents,
            contents_tx,
        },
    )
}

fn build_retained_link(
    fields: &Fields,
    full_value: &Value,
    handle: &DdWorkerHandle,
    link_path: &str,
) -> (RawElOrText, RetainedNode) {
    let vm = Mutable::new(full_value.clone());
    let effective_link = extract_effective_link(fields, link_path);
    let link_ref = Rc::new(RefCell::new(effective_link));

    let el = Link::new()
        .label_signal(vm.signal_cloned().map(|v| {
            let label = get_fields(&v)
                .and_then(|f| f.get("label"))
                .map(|l| l.to_display_string())
                .unwrap_or_default();
            El::new().child(label)
        }))
        .to_signal(vm.signal_cloned().map(|v| {
            get_fields(&v)
                .and_then(|f| f.get("to"))
                .and_then(|v| v.as_text())
                .map(|s| s.to_string())
                .unwrap_or_default()
        }))
        .new_tab(NewTab::new())
        .s(Font::with_signal_self(vm.signal_cloned().map(|v| extract_font(&v))))
        .update_raw_el({
            let vm = vm.clone();
            move |raw_el| apply_zoon_style_signals(raw_el, &vm)
        });

    // Wire hover events for element.hovered LINK
    let hover_link = extract_hover_link_path(fields, link_path);
    let el = if let Some(hover_link) = hover_link {
        let handle_hover_in = handle.clone_ref();
        let link_hover_in = hover_link.clone();
        let handle_hover_out = handle.clone_ref();
        let link_hover_out = hover_link;
        el.update_raw_el(move |raw_el| {
            raw_el.after_insert(move |el: web_sys::HtmlAnchorElement| {
                let link_in = link_hover_in.clone();
                let handle_in = handle_hover_in.clone_ref();
                let enter_closure =
                    wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
                        handle_in.inject_dd_event(Event::HoverChange {
                            link_path: link_in.clone(),
                            hovered: true,
                        });
                    });
                el.add_event_listener_with_callback(
                    "mouseenter",
                    enter_closure.as_ref().unchecked_ref(),
                )
                .ok();
                enter_closure.forget();

                let link_out = link_hover_out.clone();
                let handle_out = handle_hover_out.clone_ref();
                let leave_closure =
                    wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
                        handle_out.inject_dd_event(Event::HoverChange {
                            link_path: link_out.clone(),
                            hovered: false,
                        });
                    });
                el.add_event_listener_with_callback(
                    "mouseleave",
                    leave_closure.as_ref().unchecked_ref(),
                )
                .ok();
                leave_closure.forget();
            })
        })
    } else {
        el
    };

    (el.unify(), RetainedNode::Link { value: vm, link_path: link_ref })
}

// ═══════════════════════════════════════════════════════════════════════════
// Update functions — diff old vs new Value tree
// ═══════════════════════════════════════════════════════════════════════════

impl RetainedTree {
    /// Apply keyed diffs to the keyed Stripe in this tree.
    /// Walks the tree to find the Stripe with keyed_items and applies diffs to it.
    pub fn apply_keyed_diffs(&mut self, diffs: &[KeyedDiff], handle: &DdWorkerHandle) {
        self.root.apply_keyed_diffs(diffs, handle);
    }

    /// Update the retained tree with a new document Value.
    /// Only changed elements' Mutables are updated; Zoon handles DOM diffs.
    pub fn update(&mut self, new_value: &Value, handle: &DdWorkerHandle) {
        self.root.update(new_value, handle, "");
    }
}

impl RetainedNode {
    fn update(&mut self, new_value: &Value, handle: &DdWorkerHandle, link_path: &str) {
        match self {
            RetainedNode::Primitive { text } => {
                let new_text = match new_value {
                    Value::Tag(s) if s.as_ref() == "NoElement" || s.as_ref() == "SKIP" => {
                        String::new()
                    }
                    _ => new_value.to_display_string(),
                };
                text.set_neq(new_text);
            }

            RetainedNode::Button { value, link_path: lp } => {
                value.set_neq(new_value.clone());
                if let Some(fields) = get_fields(new_value) {
                    *lp.borrow_mut() = extract_effective_link(fields, link_path);
                }
            }

            RetainedNode::Stripe {
                value,
                items,
                items_tx,
                link_path: lp,
                keyed_items,
            } => {
                value.set_neq(new_value.clone());
                if let Some(fields) = get_fields(new_value) {
                    *lp.borrow_mut() = extract_effective_link(fields, link_path);
                    // Keyed Stripes: items come via apply_keyed_diffs, skip positional diff.
                    // Non-keyed Stripes: use positional diff as before.
                    if keyed_items.is_none() {
                        let new_items = extract_sorted_list_items(fields, "items");
                        diff_children(items, items_tx, &new_items, handle, link_path, "items");
                    }
                }
            }

            RetainedNode::Stack {
                value,
                layers,
                layers_tx,
            } => {
                value.set_neq(new_value.clone());
                if let Some(fields) = get_fields(new_value) {
                    let new_layers = extract_sorted_list_items(fields, "layers");
                    diff_children(layers, layers_tx, &new_layers, handle, link_path, "layers");
                }
            }

            RetainedNode::Container {
                value,
                child,
                child_tx,
            } => {
                value.set_neq(new_value.clone());
                if let Some(fields) = get_fields(new_value) {
                    let new_child = fields.get("child");
                    match (child.as_mut(), new_child) {
                        (Some(existing), Some(new_val)) => {
                            if existing.matches_value_type(new_val) {
                                existing.update(new_val, handle, link_path);
                            } else {
                                let (el, node) =
                                    build_retained_node(new_val, handle, link_path);
                                *child = Some(Box::new(node));
                                child_tx.unbounded_send(VecDiff::RemoveAt { index: 0 }).ok();
                                child_tx
                                    .unbounded_send(VecDiff::InsertAt {
                                        index: 0,
                                        value: el,
                                    })
                                    .ok();
                            }
                        }
                        (None, Some(new_val)) => {
                            let (el, node) = build_retained_node(new_val, handle, link_path);
                            *child = Some(Box::new(node));
                            child_tx.unbounded_send(VecDiff::Push { value: el }).ok();
                        }
                        (Some(_), None) => {
                            *child = None;
                            child_tx.unbounded_send(VecDiff::Pop {}).ok();
                        }
                        (None, None) => {}
                    }
                }
            }

            RetainedNode::TextInput {
                value,
                link_path: lp,
                dom_element,
            } => {
                // Detect focus change for manual focus handling
                let old_focus = get_fields(&value.get_cloned())
                    .and_then(|f| f.get("focus"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let new_focus = get_fields(new_value)
                    .and_then(|f| f.get("focus"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let new_text = get_fields(new_value)
                    .and_then(|f| f.get("text"))
                    .and_then(|v| v.as_text())
                    .unwrap_or("")
                    .to_string();

                value.set_neq(new_value.clone());

                if let Some(fields) = get_fields(new_value) {
                    *lp.borrow_mut() = fields
                        .get(LINK_PATH_FIELD)
                        .and_then(|v| v.as_text())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| link_path.to_string());
                }

                if let Some(input_el) = dom_element.borrow().as_ref() {
                    // Always sync DOM input value to match Boon state.
                    let dom_value = input_el.value();
                    if dom_value != new_text {
                        input_el.set_value(&new_text);
                    }

                    // Handle focus change: manually focus + set cursor position
                    if !old_focus && new_focus {
                        let text_len: u32 = new_text.len().try_into().unwrap_or(0);
                        input_el.set_selection_range(text_len, text_len).ok();
                        input_el.focus().ok();
                    }
                }
            }

            RetainedNode::Checkbox {
                value,
                link_path: lp,
            } => {
                value.set_neq(new_value.clone());
                if let Some(fields) = get_fields(new_value) {
                    *lp.borrow_mut() = fields
                        .get(LINK_PATH_FIELD)
                        .and_then(|v| v.as_text())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| link_path.to_string());
                }
            }

            RetainedNode::Label {
                value,
                link_path: lp,
            } => {
                value.set_neq(new_value.clone());
                if let Some(fields) = get_fields(new_value) {
                    *lp.borrow_mut() = fields
                        .get(LINK_PATH_FIELD)
                        .and_then(|v| v.as_text())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| link_path.to_string());
                }
            }

            RetainedNode::Paragraph {
                value,
                contents,
                contents_tx,
            } => {
                value.set_neq(new_value.clone());
                if let Some(fields) = get_fields(new_value) {
                    let new_contents = extract_sorted_list_items(fields, "contents");
                    diff_children(
                        contents,
                        contents_tx,
                        &new_contents,
                        handle,
                        link_path,
                        "contents",
                    );
                }
            }

            RetainedNode::Link { value, link_path: lp } => {
                value.set_neq(new_value.clone());
                if let Some(fields) = get_fields(new_value) {
                    *lp.borrow_mut() = extract_effective_link(fields, link_path);
                }
            }

            RetainedNode::Document { child, child_tx } => {
                let new_root = get_fields(new_value).and_then(|f| f.get("root"));
                match (child.as_mut(), new_root) {
                    (Some(existing), Some(new_val)) => {
                        if existing.matches_value_type(new_val) {
                            existing.update(new_val, handle, link_path);
                        } else {
                            let (el, node) = build_retained_node(new_val, handle, link_path);
                            *child = Some(Box::new(node));
                            child_tx.unbounded_send(VecDiff::RemoveAt { index: 0 }).ok();
                            child_tx
                                .unbounded_send(VecDiff::InsertAt {
                                    index: 0,
                                    value: el,
                                })
                                .ok();
                        }
                    }
                    (None, Some(new_val)) => {
                        let (el, node) = build_retained_node(new_val, handle, link_path);
                        *child = Some(Box::new(node));
                        child_tx.unbounded_send(VecDiff::Push { value: el }).ok();
                    }
                    (Some(_), None) => {
                        *child = None;
                        child_tx.unbounded_send(VecDiff::Pop {}).ok();
                    }
                    (None, None) => {}
                }
            }

            RetainedNode::Empty => {}
        }
    }

    /// Walk the tree to find the keyed Stripe and apply diffs.
    fn apply_keyed_diffs(&mut self, diffs: &[KeyedDiff], handle: &DdWorkerHandle) {
        match self {
            RetainedNode::Stripe {
                items_tx,
                link_path: lp,
                keyed_items: Some(ki),
                ..
            } => {
                let stripe_lp = lp.borrow().clone();
                ki.apply(diffs, items_tx, handle, &stripe_lp);
            }
            RetainedNode::Document { child: Some(c), .. } => {
                c.apply_keyed_diffs(diffs, handle);
            }
            RetainedNode::Container { child: Some(c), .. } => {
                c.apply_keyed_diffs(diffs, handle);
            }
            RetainedNode::Stripe { items, keyed_items: None, .. } => {
                for item in items {
                    item.apply_keyed_diffs(diffs, handle);
                }
            }
            RetainedNode::Stack { layers, .. } => {
                for layer in layers {
                    layer.apply_keyed_diffs(diffs, handle);
                }
            }
            RetainedNode::Paragraph { contents, .. } => {
                for content in contents {
                    content.apply_keyed_diffs(diffs, handle);
                }
            }
            _ => {}
        }
    }
}

/// Positional diff for list children (Stripe items, Paragraph contents, Stack layers).
fn diff_children(
    retained: &mut Vec<RetainedNode>,
    tx: &mpsc::UnboundedSender<VecDiff<RawElOrText>>,
    new_items: &[Value],
    handle: &DdWorkerHandle,
    link_path: &str,
    field_name: &str,
) {
    let old_len = retained.len();
    let new_len = new_items.len();
    let common = old_len.min(new_len);

    for i in 0..common {
        let child_lp = format!("{}.{}.{}", link_path, field_name, i);
        if retained[i].matches_value_type(&new_items[i]) {
            retained[i].update(&new_items[i], handle, &child_lp);
        } else {
            let (el, node) = build_retained_node(&new_items[i], handle, &child_lp);
            retained[i] = node;
            tx.unbounded_send(VecDiff::RemoveAt { index: i }).ok();
            tx.unbounded_send(VecDiff::InsertAt { index: i, value: el })
                .ok();
        }
    }

    for i in old_len..new_len {
        let child_lp = format!("{}.{}.{}", link_path, field_name, i);
        let (el, node) = build_retained_node(&new_items[i], handle, &child_lp);
        retained.push(node);
        tx.unbounded_send(VecDiff::Push { value: el }).ok();
    }

    for _ in new_len..old_len {
        retained.pop();
        tx.unbounded_send(VecDiff::Pop {}).ok();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Static rendering (no event handlers, no signals)
// ═══════════════════════════════════════════════════════════════════════════

pub fn render_value_static(value: &Value) -> RawElOrText {
    match value {
        Value::Number(n) => {
            let text = if n.0 == n.0.floor() && n.0.is_finite() {
                format!("{}", n.0 as i64)
            } else {
                format!("{}", n.0)
            };
            zoon::Text::new(text).unify()
        }
        Value::Text(s) => zoon::Text::new(s.to_string()).unify(),
        Value::Tag(s) => {
            if s.as_ref() == "NoElement" {
                return El::new().unify();
            }
            zoon::Text::new(s.to_string()).unify()
        }
        Value::Bool(b) => {
            let text = if *b { "True" } else { "False" };
            zoon::Text::new(text).unify()
        }
        Value::Tagged { tag, fields } => render_tagged_static(tag, fields),
        Value::Object(_) => zoon::Text::new(value.to_display_string()).unify(),
        Value::Unit => El::new().unify(),
    }
}

fn render_tagged_static(
    tag: &str,
    fields: &Arc<Fields>,
) -> RawElOrText {
    match tag {
        "ElementButton" => {
            let label = fields
                .get("label")
                .map(|v| v.to_display_string())
                .unwrap_or_default();
            let mut el = Button::new()
                .update_raw_el(|raw_el| raw_el.attr("role", "button"))
                .label(El::new().child(label));
            if let Some(font) = extract_font_from_fields(fields) { el = el.s(font); }
            if let Some(width) = extract_width_from_fields(fields) { el = el.s(width); }
            if let Some(height) = extract_height_from_fields(fields) { el = el.s(height); }
            if let Some(padding) = extract_padding_from_fields(fields) { el = el.s(padding); }
            if let Some(background) = extract_background_from_fields(fields) { el = el.s(background); }
            if let Some(rc) = extract_rounded_corners_from_fields(fields) { el = el.s(rc); }
            if let Some(transform) = extract_transform_from_fields(fields) { el = el.s(transform); }
            if let Some(outline) = extract_outline_from_fields(fields) { el = el.s(outline); }
            if let Some(borders) = extract_borders_from_fields(fields) { el = el.s(borders); }
            if let Some(align) = extract_align_content_from_fields(fields) { el = el.s(align); }
            el.unify()
        }
        "ElementStripe" => render_stripe_static(fields),
        "ElementStack" => render_stack_static(fields),
        "ElementContainer" => render_container_static(fields),
        "ElementLabel" => {
            let label = fields
                .get("label")
                .map(|v| v.to_display_string())
                .unwrap_or_default();
            let mut el = Label::new().label(El::new().child(label));
            if let Some(font) = extract_font_from_fields(fields) { el = el.s(font); }
            if let Some(padding) = extract_padding_from_fields(fields) { el = el.s(padding); }
            el.unify()
        }
        "ElementParagraph" => {
            let mut el = Paragraph::new();
            if let Some(font) = extract_font_from_fields(fields) { el = el.s(font); }
            let rendered: Vec<RawElOrText> = extract_sorted_list_items(fields, "contents")
                .iter()
                .map(|item| render_value_static(item))
                .collect();
            let (tx, rx) = mpsc::unbounded();
            tx.unbounded_send(VecDiff::Replace { values: rendered }).ok();
            el.contents_signal_vec(VecDiffStreamSignalVec(rx)).unify()
        }
        "DocumentNew" => {
            if let Some(root) = fields.get("root") {
                render_value_static(root)
            } else {
                El::new().child("Empty document").unify()
            }
        }
        _ => zoon::Text::new(format!("{}[...]", tag)).unify(),
    }
}

fn render_stripe_static(fields: &Fields) -> RawElOrText {
    let direction = fields
        .get("direction")
        .and_then(|v| v.as_tag())
        .unwrap_or("Column");

    let mut el = Stripe::new()
        .direction(if direction == "Row" { Direction::Row } else { Direction::Column });

    if let Some(gap) = fields.get("gap").and_then(|v| v.as_number()) {
        if gap > 0.0 {
            el = el.s(Gap::both(gap as u32));
        }
    }

    if let Some(font) = extract_font_from_fields(fields) { el = el.s(font); }
    if let Some(width) = extract_width_from_fields(fields) { el = el.s(width); }
    if let Some(height) = extract_height_from_fields(fields) { el = el.s(height); }
    if let Some(padding) = extract_padding_from_fields(fields) { el = el.s(padding); }
    if let Some(background) = extract_background_from_fields(fields) { el = el.s(background); }
    if let Some(rc) = extract_rounded_corners_from_fields(fields) { el = el.s(rc); }
    if let Some(transform) = extract_transform_from_fields(fields) { el = el.s(transform); }
    if let Some(outline) = extract_outline_from_fields(fields) { el = el.s(outline); }
    if let Some(borders) = extract_borders_from_fields(fields) { el = el.s(borders); }
    if let Some(align) = extract_align_content_from_fields(fields) { el = el.s(align); }

    let rendered_items: Vec<RawElOrText> = extract_sorted_list_items(fields, "items")
        .iter()
        .map(|item| render_value_static(item))
        .collect();
    let (tx, rx) = mpsc::unbounded();
    tx.unbounded_send(VecDiff::Replace { values: rendered_items }).ok();
    el.items_signal_vec(VecDiffStreamSignalVec(rx)).unify()
}

fn render_stack_static(fields: &Fields) -> RawElOrText {
    let mut el = Stack::new();
    if let Some(width) = extract_width_from_fields(fields) { el = el.s(width); }
    if let Some(height) = extract_height_from_fields(fields) { el = el.s(height); }
    if let Some(background) = extract_background_from_fields(fields) { el = el.s(background); }

    let rendered_layers: Vec<RawElOrText> = extract_sorted_list_items(fields, "layers")
        .iter()
        .map(|item| render_value_static(item))
        .collect();
    let (tx, rx) = mpsc::unbounded();
    tx.unbounded_send(VecDiff::Replace { values: rendered_layers }).ok();
    el.layers_signal_vec(VecDiffStreamSignalVec(rx)).unify()
}

fn render_container_static(fields: &Fields) -> RawElOrText {
    let mut el = El::new();
    if let Some(font) = extract_font_from_fields(fields) { el = el.s(font); }
    if let Some(width) = extract_width_from_fields(fields) { el = el.s(width); }
    if let Some(height) = extract_height_from_fields(fields) { el = el.s(height); }
    if let Some(padding) = extract_padding_from_fields(fields) { el = el.s(padding); }
    if let Some(background) = extract_background_from_fields(fields) { el = el.s(background); }
    if let Some(rc) = extract_rounded_corners_from_fields(fields) { el = el.s(rc); }
    if let Some(transform) = extract_transform_from_fields(fields) { el = el.s(transform); }
    if let Some(outline) = extract_outline_from_fields(fields) { el = el.s(outline); }
    if let Some(borders) = extract_borders_from_fields(fields) { el = el.s(borders); }
    if let Some(align) = extract_align_content_from_fields(fields) { el = el.s(align); }

    if let Some(child) = fields.get("child") {
        el.child(render_value_static(child)).unify()
    } else {
        el.unify()
    }
}
