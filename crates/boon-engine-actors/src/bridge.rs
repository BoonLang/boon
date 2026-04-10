use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use boon_renderer_zoon::empty_text;
use boon_scene::{PhysicalSceneParams, RenderRootHandle, RenderSurface, SceneHandles};
use zoon::futures_util::stream::LocalBoxStream;
use zoon::futures_util::{StreamExt, future, select, stream};
use zoon::*;

use super::engine::{
    ActorContext, ActorHandle, ActorLoop, BRIDGE_BLUR_CAPACITY, BRIDGE_FOCUS_CAPACITY,
    BRIDGE_HOVER_CAPACITY, BRIDGE_KEY_DOWN_CAPACITY, BRIDGE_PENDING_BLUR_CAP,
    BRIDGE_PENDING_FOCUS_CAP, BRIDGE_PENDING_KEY_DOWN_CAP, BRIDGE_PRESS_EVENT_CAPACITY,
    BRIDGE_TEXT_CHANGE_CAPACITY, ConstructContext, ConstructInfo, ConstructInfoComplete,
    ConstructType, LOG_DEBUG, ListChange, NamedChannel, Number as EngineNumber, Object, ScopeId,
    Tag as EngineTag, TaggedObject, Text as EngineText, TimestampedEvent, TypedStream, Value,
    ValueIdempotencyKey, ValueMetadata, Variable, create_actor, create_constant_actor, inc_metric,
    switch_map,
};

// --- Cached ConstructInfoComplete for hot bridge paths ---
// These avoid repeated ConstructInfo::new() → complete() allocations
// for high-frequency events. Each ConstructInfo::new() allocates an
// Arc<Vec<Cow<str>>> for the id. Caching here means cloning is just
// an Arc refcount bump.

thread_local! {
    // Hover tag values (most frequent - every mouse move in/out)
    static HOVER_TAG_INFO: ConstructInfoComplete =
        ConstructInfo::new("hovered", None, "Hovered state").complete(ConstructType::Tag);

    // Change event text value (per keystroke)
    static CHANGE_EVENT_TEXT_INFO: ConstructInfoComplete =
        ConstructInfo::new("text_input::change_event::text_value", None, "change text value").complete(ConstructType::Text);

    // Key down event tag value (per keystroke)
    static KEY_DOWN_EVENT_TAG_INFO: ConstructInfoComplete =
        ConstructInfo::new("text_input::key_down_event::key_value", None, "key_down key value").complete(ConstructType::Tag);

    // Key down event text value (captured DOM text at key time)
    static KEY_DOWN_EVENT_TEXT_INFO: ConstructInfoComplete =
        ConstructInfo::new("text_input::key_down_event::text_value", None, "key_down text value").complete(ConstructType::Text);
}
use boon::parser;

/// Extract SceneContext from a ConstructContext's type-erased scene_ctx field.
fn get_scene_ctx(construct_context: &ConstructContext) -> Option<&PhysicalSceneParams> {
    construct_context
        .scene_ctx
        .as_ref()?
        .downcast_ref::<PhysicalSceneParams>()
}

async fn read_number_variable(variable: Option<Arc<Variable>>) -> Option<f64> {
    let variable = variable?;
    match variable.value_actor().current_value().await.ok()? {
        Value::Number(number, _) => Some(number.number()),
        _ => None,
    }
}

async fn derive_scene_params(scene: &SceneHandles<Arc<Variable>>) -> PhysicalSceneParams {
    let mut params = PhysicalSceneParams::default();

    if let Some(geometry_var) = &scene.geometry {
        if let Ok(Value::Object(geometry_obj, _)) = geometry_var.value_actor().current_value().await
        {
            if let Some(bevel_angle) =
                read_number_variable(geometry_obj.variable("bevel_angle")).await
            {
                params.bevel_angle = bevel_angle;
            }
        }
    }

    if let Some(lights_var) = &scene.lights {
        if let Ok(Value::List(lights, _)) = lights_var.value_actor().current_value().await {
            for (_, item) in lights.snapshot().await {
                let Ok(Value::TaggedObject(light, _)) = item.current_value().await else {
                    continue;
                };
                match light.tag() {
                    "DirectionalLight" => {
                        if let Some(intensity) =
                            read_number_variable(light.variable("intensity")).await
                        {
                            params.directional_intensity = intensity;
                        }
                        if let Some(spread) = read_number_variable(light.variable("spread")).await {
                            params.shadow_blur_per_depth = PhysicalSceneParams::DEFAULT
                                .shadow_blur_per_depth
                                * spread.clamp(0.25, 4.0);
                        }
                        if let (Some(azimuth), Some(altitude)) = (
                            read_number_variable(light.variable("azimuth")).await,
                            read_number_variable(light.variable("altitude")).await,
                        ) {
                            let azimuth_radians = azimuth.to_radians();
                            let altitude_radians = altitude.clamp(5.0, 85.0).to_radians();
                            // Interpret azimuth like a compass heading and project the shadow
                            // opposite the light direction. Keep the default scale around 45deg.
                            let altitude_factor = (1.0 / altitude_radians.tan()).clamp(0.35, 2.0);
                            params.shadow_dx_per_depth = -azimuth_radians.sin()
                                * PhysicalSceneParams::DEFAULT.shadow_dx_per_depth
                                * altitude_factor;
                            params.shadow_dy_per_depth = azimuth_radians.cos()
                                * PhysicalSceneParams::DEFAULT.shadow_dy_per_depth
                                * altitude_factor;
                        }
                    }
                    "AmbientLight" => {
                        if let Some(intensity) =
                            read_number_variable(light.variable("intensity")).await
                        {
                            params.ambient_factor = intensity.clamp(0.0, 1.0);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    params
}

fn resolve_scene_root(scene_object: &Arc<Object>) -> RenderRootHandle<Arc<Variable>> {
    RenderRootHandle::scene(
        scene_object.expect_variable("root_element").clone(),
        scene_object.variable("lights"),
        scene_object.variable("geometry"),
    )
}

fn resolve_document_root(document_object: &Arc<Object>) -> RenderRootHandle<Arc<Variable>> {
    RenderRootHandle::new(
        RenderSurface::Document,
        document_object.expect_variable("root_element").clone(),
    )
}

/// Generate physical CSS properties from a style Value in a scene context.
/// Must be called from an async context (inside stream filter_map, etc.)
/// because reading variable values requires `.current_value().await`.
///
/// Maps physical Boon properties to CSS:
/// - `depth: N` → multi-layer box-shadow
/// - `move: [closer: N]` → elevated box-shadow + subtle scale
/// - `move: [further: N]` → inset shadow + subtle scale
/// - `material.color` → background-color (when no background.color set)
/// - `material.gloss: 0-1` → specular linear-gradient overlay
/// - `material.glow` → colored outer box-shadow
/// - `rounded_corners` → border-radius
/// - `spring_range` → CSS transition
#[allow(dead_code)]
async fn physical_css_from_style_value(
    style_value: &Value,
    scene_ctx: &PhysicalSceneParams,
) -> String {
    let obj = match style_value {
        Value::Object(obj, _) => obj,
        _ => return String::new(),
    };
    let mut css = String::new();
    let mut shadows: Vec<String> = Vec::new();

    /// Helper to read a number from a variable (async).
    async fn read_number(obj: &Arc<Object>, key: &str) -> Option<f64> {
        let var = obj.variable(key)?;
        match var.value_actor().current_value().await {
            Ok(Value::Number(n, _)) => Some(n.number()),
            _ => None,
        }
    }

    // --- depth → box-shadow layers ---
    let depth = read_number(obj, "depth").await.unwrap_or(0.0);

    // --- move → elevation/recession adjustment ---
    let (elevation, is_inset) = if let Some(move_v) = obj.variable("move") {
        match move_v.value_actor().current_value().await {
            Ok(Value::Object(move_obj, _)) => {
                if let Some(closer_v) = move_obj.variable("closer") {
                    match closer_v.value_actor().current_value().await {
                        Ok(Value::Number(n, _)) => (n.number(), false),
                        _ => (0.0, false),
                    }
                } else if let Some(further_v) = move_obj.variable("further") {
                    match further_v.value_actor().current_value().await {
                        Ok(Value::Number(n, _)) => (n.number(), true),
                        _ => (0.0, false),
                    }
                } else {
                    (0.0, false)
                }
            }
            _ => (0.0, false),
        }
    } else {
        (0.0, false)
    };

    let effective_depth = depth + elevation;

    if effective_depth > 0.0 {
        let dx = effective_depth * scene_ctx.shadow_dx_per_depth;
        let dy = effective_depth * scene_ctx.shadow_dy_per_depth;
        let blur = effective_depth * scene_ctx.shadow_blur_per_depth;
        let opacity = scene_ctx.shadow_opacity();

        if is_inset {
            shadows.push(format!(
                "inset {dx:.1}px {dy:.1}px {blur:.1}px rgba(0,0,0,{opacity:.2})"
            ));
        } else {
            // Layer 1: main shadow
            shadows.push(format!(
                "{dx:.1}px {dy:.1}px {blur:.1}px rgba(0,0,0,{opacity:.2})"
            ));
            // Layer 2: softer ambient shadow
            let amb_blur = blur * 2.0;
            let amb_opacity = opacity * 0.4;
            shadows.push(format!(
                "0px {:.1}px {amb_blur:.1}px rgba(0,0,0,{amb_opacity:.2})",
                dy * 0.5
            ));
        }
    }

    // --- material properties ---
    if let Some(material_v) = obj.variable("material") {
        if let Ok(Value::Object(mat_obj, _)) = material_v.value_actor().current_value().await {
            // material.glow → colored outer shadow
            if let Some(glow_v) = mat_obj.variable("glow") {
                match glow_v.value_actor().current_value().await {
                    Ok(Value::Object(glow_obj, _)) => {
                        let color = match glow_obj.variable("color") {
                            Some(cv) => match cv.value_actor().current_value().await {
                                Ok(ref v) => value_to_css_color_async(v)
                                    .await
                                    .unwrap_or_else(|| "rgba(100,150,255,0.5)".to_string()),
                                _ => "rgba(100,150,255,0.5)".to_string(),
                            },
                            None => "rgba(100,150,255,0.5)".to_string(),
                        };
                        let intensity = read_number(&glow_obj, "intensity").await.unwrap_or(0.1);
                        let blur = intensity * 40.0;
                        let spread = intensity * 10.0;
                        shadows.push(format!("0 0 {blur:.1}px {spread:.1}px {color}"));
                    }
                    // glow: None → no glow (skip)
                    Ok(Value::Tag(ref tag, _)) if tag.tag() == "None" => {}
                    _ => {}
                }
            }

            // material.color → background-color (fallback when no background.color)
            if obj.variable("background").is_none() {
                if let Some(color_v) = mat_obj.variable("color") {
                    if let Ok(color_val) = color_v.value_actor().current_value().await {
                        if let Some(color_css) = value_to_css_color_async(&color_val).await {
                            css.push_str(&format!("background-color:{color_css};"));
                        }
                    }
                }
            }

            // material.gloss → specular gradient overlay
            if let Some(gloss_v) = mat_obj.variable("gloss") {
                if let Ok(Value::Number(n, _)) = gloss_v.value_actor().current_value().await {
                    let gloss = n.number().clamp(0.0, 1.0);
                    if gloss > 0.0 {
                        let alpha = gloss * 0.25;
                        let angle = scene_ctx.bevel_angle;
                        css.push_str(&format!(
                            "background-image:linear-gradient({angle:.0}deg,rgba(255,255,255,{alpha:.2}) 0%,transparent 50%,rgba(0,0,0,{:.2}) 100%);",
                            alpha * 0.3
                        ));
                    }
                }
            }

            // material.transparency → opacity
            if let Some(transp_v) = mat_obj.variable("transparency") {
                if let Ok(Value::Number(n, _)) = transp_v.value_actor().current_value().await {
                    let opacity = n.number().clamp(0.0, 1.0);
                    css.push_str(&format!("opacity:{opacity:.2};"));
                    // Also add backdrop-filter for glass effect
                    css.push_str("backdrop-filter:blur(12px);-webkit-backdrop-filter:blur(12px);");
                }
            }
        }
    }

    // Combine all shadows
    if !shadows.is_empty() {
        css.push_str(&format!("box-shadow:{};", shadows.join(",")));
    }

    // --- rounded_corners → border-radius ---
    if let Some(rc_v) = obj.variable("rounded_corners") {
        match rc_v.value_actor().current_value().await {
            Ok(Value::Number(n, _)) => {
                css.push_str(&format!("border-radius:{}px;", n.number()));
            }
            Ok(Value::Tag(tag, _)) if tag.tag() == "Fully" => {
                css.push_str("border-radius:9999px;");
            }
            _ => {}
        }
    }

    // --- spring_range → CSS transition ---
    if let Some(sr_v) = obj.variable("spring_range") {
        match sr_v.value_actor().current_value().await {
            Ok(Value::Object(sr_obj, _)) => {
                // [extend: N, compress: N] → use max as duration basis
                let extend = read_number(&sr_obj, "extend").await.unwrap_or(0.0);
                let compress = read_number(&sr_obj, "compress").await.unwrap_or(0.0);
                let range = extend.max(compress);
                if range > 0.0 {
                    let duration = (range * 0.04).clamp(0.08, 0.5);
                    css.push_str(&format!(
                        "transition:all {duration:.2}s cubic-bezier(0.34,1.56,0.64,1);"
                    ));
                }
            }
            Ok(Value::Number(n, _)) => {
                let duration = (n.number() * 0.15).clamp(0.05, 0.8);
                css.push_str(&format!(
                    "transition:all {duration:.2}s cubic-bezier(0.34,1.56,0.64,1);"
                ));
            }
            _ => {}
        }
    }

    // --- subtle scale for elevation ---
    if elevation > 0.0 && !is_inset {
        let scale = 1.0 + elevation * 0.001;
        css.push_str(&format!("transform:scale({scale:.4});"));
    } else if elevation > 0.0 && is_inset {
        let scale = 1.0 - elevation * 0.001;
        css.push_str(&format!("transform:scale({scale:.4});"));
    }

    css
}

/// Convert a Boon Value to a CSS color string (async version).
/// Handles Oklch tagged objects, named color tags, and plain text CSS colors.
async fn value_to_css_color_async(value: &Value) -> Option<String> {
    match value {
        Value::Text(t, _) => Some(t.text().to_string()),
        Value::TaggedObject(tagged, _) if tagged.tag() == "Oklch" => {
            // Inline Oklch extraction (avoids reconstructing Value with metadata)
            async fn get_num(tagged: &TaggedObject, name: &str, default: f64) -> f64 {
                if let Some(v) = tagged.variable(name) {
                    match v.value_actor().current_value().await {
                        Ok(Value::Number(n, _)) => n.number(),
                        _ => default,
                    }
                } else {
                    default
                }
            }
            let l = get_num(tagged, "lightness", 0.5).await;
            let c = get_num(tagged, "chroma", 0.0).await;
            let h = get_num(tagged, "hue", 0.0).await;
            let a = get_num(tagged, "alpha", 1.0).await;
            if a < 1.0 {
                Some(format!("oklch({}% {} {} / {})", l * 100.0, c, h, a))
            } else {
                Some(format!("oklch({}% {} {})", l * 100.0, c, h))
            }
        }
        Value::Tag(tag, _) => {
            let css = match tag.tag() {
                "White" => "white",
                "Black" => "black",
                "Red" => "red",
                "Green" => "green",
                "Blue" => "blue",
                "Transparent" => "transparent",
                _ => return None,
            };
            Some(css.to_string())
        }
        _ => None,
    }
}

/// Helper: create a style → property stream via switch_map.
/// Returns a stream of Values for a given property name under the style object.
fn style_property_stream(
    settings_variable: &Arc<Variable>,
    property: &'static str,
) -> LocalBoxStream<'static, Value> {
    let sv = settings_variable.clone();
    let style_stream = switch_map(sv.stream(), |value| {
        value.expect_object().expect_variable("style").stream()
    });
    switch_map(style_stream, move |value| {
        let obj = value.expect_object();
        match obj.variable(property) {
            Some(var) => var.stream().left_stream(),
            None => stream::empty().right_stream(),
        }
    })
    .boxed_local()
}

/// Helper: create a style → nested property stream (style → parent → child).
fn style_nested_property_stream(
    settings_variable: &Arc<Variable>,
    parent: &'static str,
    child: &'static str,
) -> LocalBoxStream<'static, Value> {
    let sv = settings_variable.clone();
    let parent_stream = style_property_stream(&sv, parent);
    switch_map(parent_stream, move |value| {
        let obj = value.expect_object();
        match obj.variable(child) {
            Some(var) => var.stream().left_stream(),
            None => stream::empty().right_stream(),
        }
    })
    .boxed_local()
}

/// Apply physical CSS to a raw element if a SceneContext is present.
/// This is a no-op in document mode.
///
/// Uses per-property reactive streams with Zoon's style_signal() API.
/// Each physical property (depth, material.color, etc.) gets its own
/// switch_map chain so changes propagate reactively (e.g., dark mode toggle).
fn apply_physical_css<E: RawEl>(
    raw_el: E,
    settings_variable: &Arc<Variable>,
    construct_context: &ConstructContext,
) -> E {
    let scene_ctx = match get_scene_ctx(construct_context) {
        Some(ctx) => ctx.clone(),
        None => return raw_el,
    };
    let sv = settings_variable.clone();

    // --- 1. background-color ← style.material.color ---
    let bg_color_css_stream = {
        let color_stream = style_nested_property_stream(&sv, "material", "color");
        // oklch_to_css_reactive: reads initial values atomically, then subscribes to
        // per-component changes via stream_from_now(). When dark mode toggles internal
        // Oklch components (e.g., lightness 0.96→0.08), the inner subscription fires.
        // The outer switch_map handles cases where a completely new Oklch object is assigned.
        switch_map(color_stream, |value| oklch_to_css_reactive(value)).boxed_local()
    };

    // --- 2. box-shadow ← style.depth + style.move + style.material.glow ---
    // Uses select_all + scan to merge three independent reactive sources
    #[derive(Clone)]
    enum ShadowComponent {
        Depth(f64),
        MoveCloser(f64),
        MoveFurther(f64),
        Glow(String), // pre-formatted glow shadow CSS fragment
    }

    let depth_stream: LocalBoxStream<'static, ShadowComponent> =
        style_property_stream(&sv, "depth")
            .filter_map(|v| {
                future::ready(match v {
                    Value::Number(n, _) => Some(ShadowComponent::Depth(n.number())),
                    _ => None,
                })
            })
            .boxed_local();

    let move_stream: LocalBoxStream<'static, ShadowComponent> = {
        let move_value_stream = style_property_stream(&sv, "move");
        switch_map(move_value_stream, |value| {
            let obj = value.expect_object();
            if let Some(var) = obj.variable("closer") {
                var.stream()
                    .filter_map(|v| {
                        future::ready(match v {
                            Value::Number(n, _) => Some(ShadowComponent::MoveCloser(n.number())),
                            _ => None,
                        })
                    })
                    .left_stream()
                    .left_stream()
            } else if let Some(var) = obj.variable("further") {
                var.stream()
                    .filter_map(|v| {
                        future::ready(match v {
                            Value::Number(n, _) => Some(ShadowComponent::MoveFurther(n.number())),
                            _ => None,
                        })
                    })
                    .right_stream()
                    .left_stream()
            } else {
                stream::once(future::ready(ShadowComponent::MoveCloser(0.0)))
                    .chain(stream::pending())
                    .right_stream()
            }
        })
        .boxed_local()
    };

    let glow_stream: LocalBoxStream<'static, ShadowComponent> = {
        let glow_value_stream = style_nested_property_stream(&sv, "material", "glow");
        // Use switch_map to subscribe reactively to glow sub-properties.
        // When dark mode changes glow.color's Oklch components, the inner
        // oklch_to_css_reactive subscription fires and updates the CSS.
        switch_map(glow_value_stream, |value| {
            match value {
                Value::Object(glow_obj, _) => {
                    #[derive(Clone)]
                    enum GlowComp {
                        Color(String),
                        Intensity(f64),
                    }

                    let color_sub: LocalBoxStream<'static, GlowComp> =
                        match glow_obj.variable("color") {
                            Some(cv) => switch_map(cv.stream(), |v| oklch_to_css_reactive(v))
                                .map(GlowComp::Color)
                                .boxed_local(),
                            None => stream::once(future::ready(GlowComp::Color(
                                "rgba(100,150,255,0.5)".to_string(),
                            )))
                            .chain(stream::pending())
                            .boxed_local(),
                        };

                    let intensity_sub: LocalBoxStream<'static, GlowComp> = match glow_obj
                        .variable("intensity")
                    {
                        Some(iv) => iv
                            .stream()
                            .filter_map(|v| {
                                future::ready(match v {
                                    Value::Number(n, _) => Some(GlowComp::Intensity(n.number())),
                                    _ => None,
                                })
                            })
                            .boxed_local(),
                        None => stream::once(future::ready(GlowComp::Intensity(0.1)))
                            .chain(stream::pending())
                            .boxed_local(),
                    };

                    stream::select_all([color_sub, intensity_sub])
                        .scan(
                            ("rgba(100,150,255,0.5)".to_string(), 0.1_f64),
                            |state, comp| {
                                match comp {
                                    GlowComp::Color(c) => state.0 = c,
                                    GlowComp::Intensity(i) => state.1 = i,
                                }
                                let blur = state.1 * 40.0;
                                let spread = state.1 * 10.0;
                                let css = format!("0 0 {blur:.1}px {spread:.1}px {}", state.0);
                                future::ready(Some(ShadowComponent::Glow(css)))
                            },
                        )
                        .left_stream()
                        .left_stream()
                }
                // glow: None → no glow
                Value::Tag(tag, _) if tag.tag() == "None" => {
                    stream::once(future::ready(ShadowComponent::Glow(String::new())))
                        .chain(stream::pending())
                        .right_stream()
                        .left_stream()
                }
                _ => stream::pending::<ShadowComponent>().right_stream(),
            }
        })
        .boxed_local()
    };

    let sc_shadow = scene_ctx.clone();
    let box_shadow_stream = stream::select_all([depth_stream, move_stream, glow_stream])
        .scan(
            (0.0_f64, 0.0_f64, false, String::new()),
            move |state, component| {
                match component {
                    ShadowComponent::Depth(d) => state.0 = d,
                    ShadowComponent::MoveCloser(e) => {
                        state.1 = e;
                        state.2 = false;
                    }
                    ShadowComponent::MoveFurther(e) => {
                        state.1 = e;
                        state.2 = true;
                    }
                    ShadowComponent::Glow(g) => state.3 = g,
                }
                let (depth, elevation, is_inset, ref glow) = *state;
                let effective_depth = depth + elevation;
                let mut shadows: Vec<String> = Vec::new();

                if effective_depth > 0.0 {
                    let dx = effective_depth * sc_shadow.shadow_dx_per_depth;
                    let dy = effective_depth * sc_shadow.shadow_dy_per_depth;
                    let blur = effective_depth * sc_shadow.shadow_blur_per_depth;
                    let opacity = sc_shadow.shadow_opacity();

                    if is_inset {
                        shadows.push(format!(
                            "inset {dx:.1}px {dy:.1}px {blur:.1}px rgba(0,0,0,{opacity:.2})"
                        ));
                    } else {
                        shadows.push(format!(
                            "{dx:.1}px {dy:.1}px {blur:.1}px rgba(0,0,0,{opacity:.2})"
                        ));
                        let amb_blur = blur * 2.0;
                        let amb_opacity = opacity * 0.4;
                        shadows.push(format!(
                            "0px {:.1}px {amb_blur:.1}px rgba(0,0,0,{amb_opacity:.2})",
                            dy * 0.5
                        ));
                    }
                }

                if !glow.is_empty() {
                    shadows.push(glow.clone());
                }

                let css = if shadows.is_empty() {
                    "none".to_string()
                } else {
                    shadows.join(",")
                };
                future::ready(Some(css))
            },
        )
        .boxed_local();

    // --- 3. background-image ← style.material.gloss ---
    let sc_gloss = scene_ctx.clone();
    let gloss_stream = style_nested_property_stream(&sv, "material", "gloss")
        .filter_map(move |value| {
            let angle = sc_gloss.bevel_angle;
            future::ready(match value {
                Value::Number(n, _) => {
                    let gloss = n.number().clamp(0.0, 1.0);
                    if gloss > 0.0 {
                        let alpha = gloss * 0.25;
                        Some(format!(
                            "linear-gradient({angle:.0}deg,rgba(255,255,255,{alpha:.2}) 0%,transparent 50%,rgba(0,0,0,{:.2}) 100%)",
                            alpha * 0.3
                        ))
                    } else {
                        Some("none".to_string())
                    }
                }
                _ => None,
            })
        })
        .boxed_local();

    // --- 4. border-radius ← style.rounded_corners ---
    let radius_stream = style_property_stream(&sv, "rounded_corners")
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                Value::Tag(tag, _) if tag.tag() == "Fully" => Some("9999px".to_string()),
                _ => None,
            })
        })
        .boxed_local();

    // --- 5. transition ← style.spring_range ---
    let transition_stream = style_property_stream(&sv, "spring_range")
        .filter_map(|value| async move {
            match value {
                Value::Object(sr_obj, _) => {
                    async fn read_num(obj: &Object, name: &str) -> f64 {
                        if let Some(v) = obj.variable(name) {
                            match v.value_actor().current_value().await {
                                Ok(Value::Number(n, _)) => n.number(),
                                _ => 0.0,
                            }
                        } else {
                            0.0
                        }
                    }
                    let extend = read_num(&sr_obj, "extend").await;
                    let compress = read_num(&sr_obj, "compress").await;
                    let range = extend.max(compress);
                    if range > 0.0 {
                        let duration = (range * 0.04).clamp(0.08, 0.5);
                        Some(format!("all {duration:.2}s cubic-bezier(0.34,1.56,0.64,1)"))
                    } else {
                        None
                    }
                }
                Value::Number(n, _) => {
                    let duration = (n.number() * 0.15).clamp(0.05, 0.8);
                    Some(format!("all {duration:.2}s cubic-bezier(0.34,1.56,0.64,1)"))
                }
                _ => None,
            }
        })
        .boxed_local();

    // --- 6. opacity + backdrop-filter ← style.material.transparency ---
    let transparency_signal = signal::from_stream(
        style_nested_property_stream(&sv, "material", "transparency")
            .filter_map(|value| {
                future::ready(match value {
                    Value::Number(n, _) => Some(n.number().clamp(0.0, 1.0)),
                    _ => None,
                })
            })
            .boxed_local(),
    )
    .broadcast();
    let opacity_signal = transparency_signal.signal_ref(|opt| opt.map(|v| format!("{v:.2}")));
    let backdrop_signal =
        transparency_signal.signal_ref(|opt| opt.map(|_| "blur(12px)".to_string()));

    // --- 7. transform: scale ← style.move elevation ---
    let scale_stream = {
        let move_value_stream = style_property_stream(&sv, "move");
        switch_map(move_value_stream, |value| {
            let obj = value.expect_object();
            if let Some(var) = obj.variable("closer") {
                var.stream()
                    .filter_map(|v| {
                        future::ready(match v {
                            Value::Number(n, _) => {
                                let elev = n.number();
                                if elev > 0.0 {
                                    Some(format!("scale({:.4})", 1.0 + elev * 0.001))
                                } else {
                                    Some("none".to_string())
                                }
                            }
                            _ => None,
                        })
                    })
                    .left_stream()
                    .left_stream()
            } else if let Some(var) = obj.variable("further") {
                var.stream()
                    .filter_map(|v| {
                        future::ready(match v {
                            Value::Number(n, _) => {
                                let elev = n.number();
                                if elev > 0.0 {
                                    Some(format!("scale({:.4})", 1.0 - elev * 0.001))
                                } else {
                                    Some("none".to_string())
                                }
                            }
                            _ => None,
                        })
                    })
                    .right_stream()
                    .left_stream()
            } else {
                stream::pending::<String>().right_stream()
            }
        })
        .boxed_local()
    };

    // Apply all physical CSS via Zoon's style_signal (reactive, no raw DOM setProperty)
    raw_el
        .style_signal("background-color", signal::from_stream(bg_color_css_stream))
        .style_signal("box-shadow", signal::from_stream(box_shadow_stream))
        .style_signal("background-image", signal::from_stream(gloss_stream))
        .style_signal("border-radius", signal::from_stream(radius_stream))
        .style_signal("transition", signal::from_stream(transition_stream))
        .style_signal("opacity", opacity_signal)
        .style_signal("backdrop-filter", backdrop_signal)
        .style_signal("transform", signal::from_stream(scale_stream))
}

/// Convert a Boon tag name to a Zoon Tag.
/// Maps common semantic HTML tags and falls back to Custom for unknown tags.
/// NOTE: Currently unused - ready for when HTML tag support is implemented.
#[allow(dead_code)]
fn boon_tag_to_zoon_tag(tag_name: &str) -> zoon::Tag<'static> {
    match tag_name {
        "Header" => zoon::Tag::Header,
        "Footer" => zoon::Tag::Footer,
        "Section" => zoon::Tag::Section,
        "Article" => zoon::Tag::Article,
        "Aside" => zoon::Tag::Aside,
        "Main" => zoon::Tag::Main,
        "Nav" => zoon::Tag::Nav,
        "H1" => zoon::Tag::H1,
        "H2" => zoon::Tag::H2,
        "H3" => zoon::Tag::H3,
        "H4" => zoon::Tag::H4,
        "H5" => zoon::Tag::H5,
        "H6" => zoon::Tag::H6,
        // List elements not in Zoon's Tag enum - use Custom
        "Ul" => zoon::Tag::Custom("ul"),
        "Ol" => zoon::Tag::Custom("ol"),
        "Li" => zoon::Tag::Custom("li"),
        // Fallback for any other tag
        other => {
            let tag_lower = other.to_lowercase();
            // Leak the string to get 'static lifetime - acceptable for small set of tags
            zoon::Tag::Custom(Box::leak(tag_lower.into_boxed_str()))
        }
    }
}

/// Log unexpected type in debug mode. Call this in filter_map when receiving an unexpected type.
/// This helps catch bugs where type mismatches would otherwise be silently swallowed.
/// In release mode, this is a no-op.
#[allow(dead_code)]
fn log_unexpected_type(context: &str, expected: &str, got: &Value) {
    #[cfg(debug_assertions)]
    {
        let type_name = match got {
            Value::Tag(_, _) => "Tag",
            Value::TaggedObject(_, _) => "TaggedObject",
            Value::Object(_, _) => "Object",
            Value::Text(_, _) => "Text",
            Value::Number(_, _) => "Number",
            Value::List(_, _) => "List",
            Value::Flushed(_, _) => "Flushed",
        };
        zoon::eprintln!(
            "[TYPE_MISMATCH] {}: expected {}, got {}",
            context,
            expected,
            type_name
        );
    }
    // In release mode, do nothing - this maintains current behavior
    #[cfg(not(debug_assertions))]
    let _ = (context, expected, got);
}

pub fn object_with_document_to_element_signal(
    root_object: Arc<Object>,
    construct_context: ConstructContext,
) -> impl Signal<Item = Option<RawElOrText>> {
    let render_root = select_root_variable(&root_object);
    let root_actor = render_root.root.clone().value_actor();

    // CRITICAL: Use switch_map (not flat_map) because the inner stream is infinite.
    // When example is switched, the document changes and we MUST switch to the new
    // root_element stream. flat_map would stay subscribed to the old one forever.
    let element_stream = switch_map(root_actor.clone().stream(), move |value| {
        let resolved_root = if render_root.is_scene() {
            resolve_scene_root(&value.expect_object())
        } else {
            resolve_document_root(&value.expect_object())
        };
        let root_element_var = resolved_root.root.clone();
        stream::once({
            let scene = resolved_root.scene.clone();
            async move {
                let scene_ctx: Option<Rc<dyn std::any::Any>> = if let Some(scene) = scene {
                    Some(Rc::new(derive_scene_params(&scene).await))
                } else {
                    None
                };
                (root_element_var, scene_ctx)
            }
        })
        .flat_map(|(root_element_var, scene_ctx)| {
            root_element_var
                .value_actor()
                .clone()
                .stream()
                .map(move |v| (v, scene_ctx.clone()))
        })
    })
    .map(move |(value, scene_ctx)| {
        let mut ctx = construct_context.clone();
        ctx.scene_ctx = scene_ctx;
        value_to_element(value, ctx)
    })
    .boxed_local();

    signal::from_stream(element_stream)
}

fn select_root_variable(root_object: &Arc<Object>) -> RenderRootHandle<Arc<Variable>> {
    if let Some(scene_var) = root_object.variable("scene") {
        RenderRootHandle::new(RenderSurface::Scene, scene_var.clone())
    } else {
        RenderRootHandle::new(
            RenderSurface::Document,
            root_object.expect_variable("document").clone(),
        )
    }
}

fn value_to_element(value: Value, construct_context: ConstructContext) -> RawElOrText {
    match value {
        Value::Text(text, _) => zoon::Text::new(text.text()).unify(),
        Value::Number(number, _) => zoon::Text::new(number.number()).unify(),
        Value::Tag(tag, _) => {
            // Handle special tags like NoElement
            match tag.tag() {
                "NoElement" => El::new().unify(),        // Empty element
                other => zoon::Text::new(other).unify(), // Render tag as text
            }
        }
        Value::TaggedObject(tagged_object, _) => match tagged_object.tag() {
            "ElementContainer" => element_container(tagged_object, construct_context).unify(),
            "ElementStripe" => element_stripe(tagged_object, construct_context).unify(),
            "ElementStack" => element_stack(tagged_object, construct_context).unify(),
            "ElementButton" => element_button(tagged_object, construct_context).unify(),
            "ElementTextInput" => element_text_input(tagged_object, construct_context).unify(),
            "ElementCheckbox" => element_checkbox(tagged_object, construct_context).unify(),
            "ElementSlider" => element_slider(tagged_object, construct_context).unify(),
            "ElementSelect" => element_select(tagged_object, construct_context).unify(),
            "ElementSvg" => element_svg(tagged_object, construct_context).unify(),
            "ElementSvgCircle" => element_svg_circle(tagged_object, construct_context).unify(),
            "ElementLabel" => element_label(tagged_object, construct_context).unify(),
            "ElementParagraph" => element_paragraph(tagged_object, construct_context).unify(),
            "ElementLink" => element_link(tagged_object, construct_context).unify(),
            "ElementText" => element_text(tagged_object, construct_context).unify(),
            "ElementBlock" => element_block(tagged_object, construct_context).unify(),
            other => panic!("Element cannot be created from the tagged object with tag '{other}'"),
        },
        Value::Flushed(inner, _) => {
            // Unwrap Flushed and recursively handle the inner value
            value_to_element(*inner, construct_context)
        }
        Value::Object(_obj, _) => {
            // Transient reactive state — an Object (e.g. a style object) can briefly
            // reach value_to_element before the full tagged element constructs.
            // Render as empty; the correct element will replace it on the next update.
            El::new().unify()
        }
        Value::List(_list, _) => {
            // List can't be rendered as a single element - render as debug info
            zoon::eprintln!("Warning: List value passed to element context - rendering as empty");
            El::new().unify()
        }
    }
}

fn actor_current_or_future_stream(actor: ActorHandle) -> LocalBoxStream<'static, Value> {
    let actor_for_initial = actor.clone();
    let initial = stream::once(async move { actor_for_initial.value().await.ok() })
        .filter_map(|value| async move { value });
    stream::select(initial, actor.stream_from_now())
        .scan(None::<EmissionIdentity>, |last_seen, value| {
            let current = value.emission_identity();
            let should_emit = last_seen.as_ref() != Some(&current);
            *last_seen = Some(current);
            future::ready(Some(if should_emit { Some(value) } else { None }))
        })
        .filter_map(future::ready)
        .boxed_local()
}

fn variable_current_or_future_stream(variable: Arc<Variable>) -> LocalBoxStream<'static, Value> {
    actor_current_or_future_stream(variable.value_actor())
}

/// Create a reactive visible signal from a settings variable.
/// Returns a signal that emits `Option<bool>` for the `visible` style property.
fn visible_signal_from_settings(
    settings_variable: Arc<Variable>,
) -> impl Signal<Item = Option<bool>> + 'static {
    signal::from_stream({
        let style_stream = switch_map(settings_variable.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("visible") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Tag(tag, _) => Some(tag.tag() == "True"),
                Value::Number(n, _) => Some(n.number() != 0.0),
                _ => None,
            })
        })
    })
}

/// Create a reactive disabled signal from a settings variable.
/// Returns a signal that emits `Option<&str>` for the HTML `disabled` attribute.
/// Some("") means disabled, None means enabled (attribute removed).
fn disabled_signal_from_settings(
    settings_variable: Arc<Variable>,
) -> impl Signal<Item = Option<&'static str>> + 'static {
    signal::from_stream({
        let style_stream = switch_map(settings_variable.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("disabled") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Tag(tag, _) => Some(tag.tag() == "True"),
                _ => None,
            })
        })
    })
    .map(|opt| match opt {
        Some(true) => Some(""),
        _ => None,
    })
}

fn element_container(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let is_scene = construct_context.scene_ctx.is_some();
    let settings_variable = tagged_object.expect_variable("settings");
    let sv_physical = settings_variable.clone();
    let ctx_physical = construct_context.clone();

    // Use switch_map (not flat_map) because child stream is infinite.
    // When example switches, we must re-subscribe to the new child element.
    let child_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("child").stream()
    })
    .map({
        let _construct_context = construct_context.clone();
        move |value| value_to_element(value, construct_context.clone())
    });

    // Create style streams
    let sv2 = tagged_object.expect_variable("settings");
    let sv3 = tagged_object.expect_variable("settings");
    let sv4 = tagged_object.expect_variable("settings");
    let sv5 = tagged_object.expect_variable("settings");
    let sv6 = tagged_object.expect_variable("settings");
    let sv7 = tagged_object.expect_variable("settings");
    let sv_font_size = tagged_object.expect_variable("settings");
    let sv_font_color = tagged_object.expect_variable("settings");
    let sv_font_weight = tagged_object.expect_variable("settings");
    let sv_align = tagged_object.expect_variable("settings");
    let sv_bg_url = tagged_object.expect_variable("settings");
    let sv_size = tagged_object.expect_variable("settings");
    let sv_visible = tagged_object.expect_variable("settings");
    let visible_sig = visible_signal_from_settings(sv_visible);

    // Padding with directional support - produces tuple (top, right, bottom, left) as u32
    // Uses Zoon's Padding::*_signal APIs for typed styling
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let padding_tuple_signal = signal::from_stream({
        let style_stream = switch_map(sv7.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("padding") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => {
                    let all = n.number() as u32;
                    Some((all, all, all, all))
                }
                Value::Object(obj, _) => {
                    let top = if let Some(v) = obj.variable("top") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("column") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let right = if let Some(v) = obj.variable("right") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("row") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let bottom = if let Some(v) = obj.variable("bottom") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("column") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let left = if let Some(v) = obj.variable("left") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("row") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    Some((top, right, bottom, left))
                }
                _ => None,
            }
        })
        .boxed_local()
    })
    .broadcast();
    // Derive individual padding signals from the broadcasted tuple
    let padding_top_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(t, _, _, _)| t));
    let padding_right_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, r, _, _)| r));
    let padding_bottom_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, _, b, _)| b));
    let padding_left_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, _, _, l)| l));

    // Font size - produces u32 for typed Font API
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_size.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = if let Value::Number(n, _) = value {
                Some(n.number() as u32)
            } else {
                None
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Font color
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let font_color_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_color.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(font_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    // Font weight - produces FontWeight typed values
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let font_weight_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_weight.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("weight") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => match tag.tag() {
                    "Hairline" => Some(FontWeight::Hairline),
                    "ExtraLight" | "UltraLight" => Some(FontWeight::ExtraLight),
                    "Light" => Some(FontWeight::Light),
                    "Regular" | "Normal" => Some(FontWeight::Regular),
                    "Medium" => Some(FontWeight::Medium),
                    "SemiBold" | "DemiBold" => Some(FontWeight::SemiBold),
                    "Bold" => Some(FontWeight::Bold),
                    "ExtraBold" | "UltraBold" => Some(FontWeight::ExtraBold),
                    "Black" | "Heavy" => Some(FontWeight::Heavy),
                    "ExtraHeavy" => Some(FontWeight::ExtraHeavy),
                    _ => None,
                },
                Value::Number(n, _) => Some(FontWeight::Number(n.number() as u32)),
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Align (row: Center -> text-align via Font typed API)
    // Produces Font values for Font::with_signal_self() - no FontAlignment enum needed
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let align_font_signal = signal::from_stream({
        let style_stream = switch_map(sv_align.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let align_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("align") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(align_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("row") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result: Option<Font<'static>> = match value {
                Value::Tag(tag, _) => match tag.tag() {
                    "Center" => Some(Font::new().center()),
                    "Left" | "Start" => Some(Font::new().left()),
                    "Right" | "End" => Some(Font::new().right()),
                    "Justify" => Some(Font::new().justify()),
                    _ => None,
                },
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Width signal - produces u32 for typed Width API
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let width_signal = signal::from_stream({
        let style_stream = switch_map(sv2.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("width") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
    });

    // Height signal - produces u32 for typed Height API
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let height_signal = signal::from_stream({
        let style_stream = switch_map(sv3.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("height") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
    });

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    // In scene mode, physical CSS handles background-color via material.color — skip typed API
    let background_signal = signal::from_stream(if is_scene {
        stream::pending::<String>().boxed_local()
    } else {
        let style_stream = switch_map(sv4.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let bg_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("background") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(bg_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    // Background image URL - produces raw URL string for typed Background::url_signal API
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let background_image_signal = signal::from_stream({
        let style_stream = switch_map(sv_bg_url.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let bg_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("background") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(bg_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("url") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Text(text, _) => Some(text.text().to_string()),
                _ => None,
            })
        })
    });

    // Size (shorthand for width + height) - produces u32 for typed Width/Height API
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let size_signal = signal::from_stream({
        let style_stream = switch_map(sv_size.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
    });

    // Border radius - produces u32 for typed RoundedCorners API
    // In scene mode, physical CSS handles border-radius via rounded_corners — skip typed API
    let border_radius_signal = signal::from_stream(if is_scene {
        stream::pending::<u32>().boxed_local()
    } else {
        let style_stream = switch_map(sv5.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("rounded_corners") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
        .boxed_local()
    });

    // Transform: move_right, move_down, and rotate - produces Transform typed values
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let transform_signal = signal::from_stream({
        let style_stream = switch_map(sv6.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("transform") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            let obj = value.expect_object();
            let move_right = if let Some(v) = obj.variable("move_right") {
                match v.value_actor().current_value().await {
                    Ok(Value::Number(n, _)) => n.number(),
                    _ => 0.0,
                }
            } else {
                0.0
            };
            let move_down = if let Some(v) = obj.variable("move_down") {
                match v.value_actor().current_value().await {
                    Ok(Value::Number(n, _)) => n.number(),
                    _ => 0.0,
                }
            } else {
                0.0
            };
            let rotate = if let Some(v) = obj.variable("rotate") {
                match v.value_actor().current_value().await {
                    Ok(Value::Number(n, _)) => n.number(),
                    _ => 0.0,
                }
            } else {
                0.0
            };
            // Build Transform using typed API
            let has_transform = move_right != 0.0 || move_down != 0.0 || rotate != 0.0;
            if has_transform {
                let mut transform = Transform::new();
                if move_right != 0.0 {
                    transform = transform.move_right(move_right);
                }
                if move_down != 0.0 {
                    transform = transform.move_down(move_down);
                }
                if rotate != 0.0 {
                    transform = transform.rotate(rotate);
                }
                Some(transform)
            } else {
                None
            }
        })
        .boxed_local()
    });

    // Size signal for height (duplicate for separate signal) - produces u32
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_size2 = tagged_object.expect_variable("settings");
    let size_for_height_signal = signal::from_stream({
        let style_stream = switch_map(sv_size2.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
    });

    // Use Stripe (flex container by design) with typed styles
    // display: flex, flex-direction: column are built into Stripe
    // align-items: center is handled by Align::center_x()
    // background-size: contain and background-repeat: no-repeat are global in MoonZoon's basic.css
    //
    // TODO: Implement HTML tag support (element.tag property)
    // Currently element: [tag: Header] is IGNORED because:
    // 1. Stripe::with_tag(tag) must be called at compile time, but tag is known at runtime
    // 2. Stripe::new() and Stripe::with_tag() return different generic types
    // Solutions:
    //   a) Use Box<dyn Element> for dynamic dispatch (performance cost)
    //   b) Add async element construction phase to read tag before construction
    //   c) Request Zoon API: Stripe::new().tag_signal(tag_signal) for runtime tag changes
    // The helper function boon_tag_to_zoon_tag() is already implemented and ready.
    Stripe::new()
        .direction(Direction::Column)
        .s(Width::exact_signal(width_signal))
        .s(Width::exact_signal(size_signal)) // size overrides width
        .s(Height::exact_signal(height_signal))
        .s(Height::exact_signal(size_for_height_signal)) // size overrides height
        .s(Background::new()
            .color_signal(background_signal)
            .url_signal(background_image_signal))
        .s(RoundedCorners::all_signal(border_radius_signal))
        .s(Transform::with_signal_self(transform_signal))
        .s(Font::new()
            .size_signal(font_size_signal)
            .color_signal(font_color_signal)
            .weight_signal(font_weight_signal))
        .s(Padding::new()
            .top_signal(padding_top_signal)
            .right_signal(padding_right_signal)
            .bottom_signal(padding_bottom_signal)
            .left_signal(padding_left_signal))
        .s(Font::with_signal_self(align_font_signal))
        .s(Visible::with_signal(visible_sig))
        .item_signal(signal::from_stream(child_stream))
        .update_raw_el(move |raw_el| apply_physical_css(raw_el, &sv_physical, &ctx_physical))
        .after_remove(move |_| {
            drop(tagged_object);
        })
}

fn element_stripe(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let is_scene = construct_context.scene_ctx.is_some();
    let sv_physical = tagged_object.expect_variable("settings");
    let ctx_physical = construct_context.clone();

    // TimestampedEvent captures Lamport time at DOM callback for consistent ordering
    let (hovered_sender, mut hovered_receiver) =
        NamedChannel::<TimestampedEvent<bool>>::new("element.hovered", BRIDGE_HOVER_CAPACITY);

    // Set up hovered link if element field exists with hovered property
    // Access element through settings, like other properties (style, direction, etc.)
    let sv_element_for_hover = tagged_object.expect_variable("settings");
    // CRITICAL: Use switch_map (not flat_map) because element variable stream is infinite.
    let hovered_stream = switch_map(sv_element_for_hover.stream(), |value| {
        // Get element from settings object if it exists
        let obj = value.expect_object();
        match obj.variable("element") {
            Some(var) => var.stream().left_stream(),
            None => stream::empty().right_stream(),
        }
    })
    .filter_map(|value| {
        let obj = value.expect_object();
        future::ready(obj.variable("hovered"))
    })
    .map(|variable| variable.expect_link_value_sender())
    .chain(stream::pending());

    let hovered_handler_loop = ActorLoop::new({
        let _construct_context = construct_context.clone();
        async move {
            let mut hovered_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut hovered_stream = hovered_stream.fuse();
            let mut last_hover_state: Option<bool> = None;
            loop {
                select! {
                    new_sender = hovered_stream.next() => {
                        if let Some(sender) = new_sender {
                            // Send initial hover state (false) when link is established
                            let initial_hover_value = EngineTag::new_value_cached(
                                HOVER_TAG_INFO.with(|info| info.clone()),
                                ValueIdempotencyKey::new(),
                                "False",
                            );
                            sender.send_or_drop(initial_hover_value);
                            last_hover_state = Some(false);
                            hovered_link_value_sender = Some(sender);
                        }
                    }
                    event = hovered_receiver.select_next_some() => {
                        if last_hover_state == Some(event.data) {
                            inc_metric!(HOVER_EVENTS_DEDUPED);
                            continue;
                        }
                        if let Some(sender) = hovered_link_value_sender.as_ref() {
                            inc_metric!(HOVER_EVENTS_EMITTED);
                            last_hover_state = Some(event.data);
                            let hover_tag = if event.data { "True" } else { "False" };
                            let event_value = EngineTag::new_value_cached_with_lamport_time(
                                HOVER_TAG_INFO.with(|info| info.clone()),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                hover_tag,
                            );
                            sender.send_or_drop(event_value);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    // CRITICAL: Use switch_map (not flat_map) because variable streams are infinite.
    // These are Arc-wrapped, so when we call `expect_variable()`, we get an Arc<Variable>
    // that stays alive independently of the parent Object. switch_map keeps the Variable
    // alive for its subscription lifetime.

    let direction_stream = switch_map(settings_variable.clone().stream(), |value| {
        let object = value.expect_object();
        // object.expect_variable returns Arc<Variable> which is kept alive by switch_map
        object.expect_variable("direction").stream()
    })
    .map(|direction| match direction.expect_tag().tag() {
        "Column" => Direction::Column,
        "Row" => Direction::Row,
        other => panic!(
            "Invalid Stripe element direction value: Found: '{other}', Expected: 'Column' or 'Row'"
        ),
    });

    // Gap - produces u32 for typed Gap API
    let gap_stream = switch_map(settings_variable.clone().stream(), |value| {
        let object = value.expect_object();
        object.expect_variable("gap").stream()
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Number(n, _) => Some(n.number() as u32),
            _ => None,
        })
    });

    // Style property streams for element_stripe
    let sv_visible = tagged_object.expect_variable("settings");
    let visible_sig = visible_signal_from_settings(sv_visible);

    // Width - produces typed Width values with optional min/max constraints
    // Supports: Fill | number | [sizing: Fill, minimum: X, maximum: Y]
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_width = tagged_object.expect_variable("settings");
    let width_typed_signal = signal::from_stream({
        let style_stream = switch_map(sv_width.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("width") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => Some(Width::exact(n.number() as u32)),
                Value::Tag(tag, _) if tag.tag() == "Fill" => Some(Width::fill()),
                Value::Object(obj, _) => {
                    // Handle [sizing: Fill, minimum: X, maximum: Y]
                    // Parse sizing (Fill or exact value)
                    let base_width = if let Some(v) = obj.variable("sizing") {
                        match v.value_actor().current_value().await {
                            Ok(Value::Tag(tag, _)) if tag.tag() == "Fill" => Some(Width::fill()),
                            Ok(Value::Number(n, _)) => Some(Width::exact(n.number() as u32)),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    let mut width = base_width?;

                    // Apply minimum constraint
                    if let Some(v) = obj.variable("minimum") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            width = width.min(n.number() as u32);
                        }
                    }

                    // Apply maximum constraint
                    if let Some(v) = obj.variable("maximum") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            width = width.max(n.number() as u32);
                        }
                    }

                    Some(width)
                }
                _ => None,
            }
        })
        .boxed_local()
    });

    // Height - produces typed Height values with optional min constraint (supports Screen -> 100vh)
    // Supports: Fill | number | [sizing: Fill, minimum: Screen | number]
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_height = tagged_object.expect_variable("settings");
    let height_typed_signal = signal::from_stream({
        let style_stream = switch_map(sv_height.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("height") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => Some(Height::exact(n.number() as u32)),
                Value::Tag(tag, _) if tag.tag() == "Fill" => Some(Height::fill()),
                Value::Object(obj, _) => {
                    // Parse sizing (Fill or exact value)
                    let base_height = if let Some(v) = obj.variable("sizing") {
                        match v.value_actor().current_value().await {
                            Ok(Value::Tag(tag, _)) if tag.tag() == "Fill" => Some(Height::fill()),
                            Ok(Value::Number(n, _)) => Some(Height::exact(n.number() as u32)),
                            _ => None,
                        }
                    } else {
                        None
                    };

                    let mut height = base_height?;

                    // Apply minimum constraint (supports Screen for 100vh and pixel values)
                    if let Some(v) = obj.variable("minimum") {
                        match v.value_actor().current_value().await {
                            Ok(Value::Tag(tag, _)) if tag.tag() == "Screen" => {
                                height = height.min_screen();
                            }
                            Ok(Value::Number(n, _)) => {
                                height = height.min(n.number() as u32);
                            }
                            _ => {}
                        }
                    }

                    Some(height)
                }
                _ => None,
            }
        })
        .boxed_local()
    });

    // Background color
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    // In scene mode, physical CSS handles background-color via material.color — skip typed API
    let sv_bg = tagged_object.expect_variable("settings");
    let background_signal = signal::from_stream(if is_scene {
        stream::pending::<String>().boxed_local()
    } else {
        let style_stream = switch_map(sv_bg.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let bg_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("background") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(bg_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    // Padding (directional: [top, column, left, right, row, bottom]) - produces tuple (top, right, bottom, left) as u32
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_padding = tagged_object.expect_variable("settings");
    let padding_tuple_signal = signal::from_stream({
        let style_stream = switch_map(sv_padding.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("padding") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => {
                    let all = n.number() as u32;
                    Some((all, all, all, all))
                }
                Value::Object(obj, _) => {
                    async fn get_num(obj: &Object, name: &str) -> u32 {
                        if let Some(v) = obj.variable(name) {
                            match v.value_actor().current_value().await {
                                Ok(Value::Number(n, _)) => n.number() as u32,
                                _ => 0,
                            }
                        } else {
                            0
                        }
                    }
                    let top = get_num(&obj, "top").await;
                    let bottom = get_num(&obj, "bottom").await;
                    let left = get_num(&obj, "left").await;
                    let right = get_num(&obj, "right").await;
                    let column = get_num(&obj, "column").await;
                    let row = get_num(&obj, "row").await;

                    let final_top = if top > 0 { top } else { column };
                    let final_bottom = if bottom > 0 { bottom } else { column };
                    let final_left = if left > 0 { left } else { row };
                    let final_right = if right > 0 { right } else { row };

                    Some((final_top, final_right, final_bottom, final_left))
                }
                _ => None,
            }
        })
        .boxed_local()
    })
    .broadcast();
    // Derive individual padding signals from the broadcasted tuple
    let padding_top_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(t, _, _, _)| t));
    let padding_right_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, r, _, _)| r));
    let padding_bottom_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, _, b, _)| b));
    let padding_left_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, _, _, l)| l));

    // Shadows (box-shadow from LIST of shadow objects) - produces Vec<Shadow> for typed Shadows API
    // In scene mode, physical CSS handles box-shadow via depth/move/glow — skip typed API
    let sv_shadows = tagged_object.expect_variable("settings");
    let shadows_typed_signal = signal::from_stream(if is_scene {
        stream::pending::<Vec<Shadow>>().boxed_local()
    } else {
        let style_stream = switch_map(sv_shadows.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("shadows") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            if let Value::List(list, _) = &value {
                let mut shadows: Vec<Shadow> = Vec::new();
                // Get all items from the list (snapshot returns (ItemId, ActorHandle) pairs)
                let snapshot = list.snapshot().await;
                for (_item_id, actor) in snapshot {
                    let item = actor.current_value().await;
                    if let Ok(Value::Object(obj, _)) = item {
                        async fn get_num(obj: &Object, name: &str) -> f64 {
                            if let Some(v) = obj.variable(name) {
                                match v.value_actor().current_value().await {
                                    Ok(Value::Number(n, _)) => n.number(),
                                    _ => 0.0,
                                }
                            } else {
                                0.0
                            }
                        }
                        let x = get_num(&obj, "x").await as i32;
                        let y = get_num(&obj, "y").await as i32;
                        let blur = get_num(&obj, "blur").await as u32;
                        let spread = get_num(&obj, "spread").await as i32;

                        // Check for inset (direction: Inwards)
                        let inset = if let Some(v) = obj.variable("direction") {
                            match v.value_actor().current_value().await {
                                Ok(Value::Tag(tag, _)) if tag.tag() == "Inwards" => true,
                                _ => false,
                            }
                        } else {
                            false
                        };

                        // Get color using typed API
                        let color: Option<Color> = if let Some(v) = obj.variable("color") {
                            if let Ok(color_value) = v.value_actor().current_value().await {
                                oklch_to_color(color_value).await
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        // Build typed Shadow
                        let mut shadow = Shadow::new().x(x).y(y).blur(blur).spread(spread);
                        if inset {
                            shadow = shadow.inner();
                        }
                        if let Some(c) = color {
                            shadow = shadow.color(c);
                        }
                        shadows.push(shadow);
                    }
                }
                if !shadows.is_empty() {
                    Some(shadows)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .boxed_local()
    });

    // Font size - produces u32 for typed Font API (cascading to children)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_size = tagged_object.expect_variable("settings");
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_size.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
    });

    // Font color
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let sv_font_color = tagged_object.expect_variable("settings");
    let font_color_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_color.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(font_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    // Font weight - produces FontWeight for typed Font API
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_weight = tagged_object.expect_variable("settings");
    let font_weight_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_weight.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("weight") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => match tag.tag() {
                    "Hairline" => Some(FontWeight::Hairline),
                    "ExtraLight" | "UltraLight" => Some(FontWeight::ExtraLight),
                    "Light" => Some(FontWeight::Light),
                    "Regular" | "Normal" => Some(FontWeight::Regular),
                    "Medium" => Some(FontWeight::Medium),
                    "SemiBold" | "DemiBold" => Some(FontWeight::SemiBold),
                    "Bold" => Some(FontWeight::Bold),
                    "ExtraBold" | "UltraBold" => Some(FontWeight::ExtraBold),
                    "Black" | "Heavy" => Some(FontWeight::Heavy),
                    "ExtraHeavy" => Some(FontWeight::ExtraHeavy),
                    _ => None,
                },
                Value::Number(n, _) => Some(FontWeight::Number(n.number() as u32)),
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Font family - produces Vec<FontFamily> for typed Font API
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_family = tagged_object.expect_variable("settings");
    let font_family_typed_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_family.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("family") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            if let Value::List(list, _) = &value {
                // Get all items from list (snapshot returns (ItemId, ActorHandle) pairs)
                let snapshot = list.snapshot().await;
                let mut families: Vec<FontFamily<'static>> = Vec::new();
                for (_item_id, actor) in snapshot {
                    if let Ok(item) = actor.current_value().await {
                        match item {
                            Value::Text(t, _) => {
                                // Custom font name - leak to get 'static lifetime
                                let name: &'static str =
                                    Box::leak(t.text().to_string().into_boxed_str());
                                families.push(FontFamily::new(name));
                            }
                            Value::Tag(tag, _) => match tag.tag() {
                                "SansSerif" => families.push(FontFamily::SansSerif),
                                "Serif" => families.push(FontFamily::Serif),
                                "Monospace" => families.push(FontFamily::Monospace),
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                }
                if !families.is_empty() {
                    Some(families)
                } else {
                    None
                }
            } else {
                None
            }
        })
        .boxed_local()
    });

    // Font align - produces Font values for typed API (text-align)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_align = tagged_object.expect_variable("settings");
    let font_align_font_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_align.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("align") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result: Option<Font<'static>> = match value {
                Value::Tag(tag, _) => match tag.tag() {
                    "Center" => Some(Font::new().center()),
                    "Left" => Some(Font::new().left()),
                    "Right" => Some(Font::new().right()),
                    "Justify" => Some(Font::new().justify()),
                    _ => None,
                },
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Scrollbars - produces CSS overflow value ("auto" when True, "visible" otherwise)
    let sv_scrollbars = tagged_object.expect_variable("settings");
    let scrollbars_css_signal = signal::from_stream({
        let style_stream = switch_map(sv_scrollbars.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("scrollbars") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Tag(tag, _) if tag.tag() == "True" => Some("auto"),
                _ => None,
            })
        })
        .boxed_local()
    });

    // Borders (supports [top: [color: Oklch[...]]]) - produces Border for typed Borders API
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_borders = tagged_object.expect_variable("settings");
    let border_top_typed_signal = signal::from_stream({
        let style_stream = switch_map(sv_borders.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let borders_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("borders") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        let top_stream = switch_map(borders_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("top") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(top_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("color") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            if let Some(color) = oklch_to_color(value).await {
                // Create typed Border with width 3, solid style, and the color
                Some(Border::new().width(3).solid().color(color))
            } else {
                None
            }
        })
        .boxed_local()
    });

    // Outline signal - handles both NoOutline tag and Object with color/thickness/style fields.
    // CRITICAL: Uses nested switch_map (not flat_map) because all variable streams are infinite.
    let sv_outline = tagged_object.expect_variable("settings");
    let outline_value_stream = {
        let style_stream = switch_map(sv_outline.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("outline") {
                Some(var) => var.value_actor().clone().stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
    };
    let outline_signal = signal::from_stream(switch_map(outline_value_stream, |value| {
        match &value {
            Value::Tag(tag, _) if tag.tag() == "NoOutline" => {
                // Return None to remove outline
                stream::once(future::ready(None::<zoon::Outline>))
                    .chain(stream::pending())
                    .boxed_local()
            }
            Value::Object(obj, _) => {
                let obj = obj.clone();
                stream::once(async move {
                    // Parse thickness (default: 1)
                    let thickness = if let Some(thickness_var) = obj.variable("thickness") {
                        match thickness_var.value_actor().value().await {
                            Ok(Value::Number(n, _)) => n.number() as u32,
                            _ => 1,
                        }
                    } else {
                        1
                    };

                    // Parse side: Inner or Outer (default: Outer)
                    let is_inner = if let Some(side_var) = obj.variable("side") {
                        match side_var.value_actor().value().await {
                            Ok(Value::Tag(tag, _)) => tag.tag() == "Inner",
                            _ => false,
                        }
                    } else {
                        false
                    };

                    // Parse line_style: solid (default), dashed, dotted
                    let line_style = if let Some(style_var) = obj.variable("line_style") {
                        match style_var.value_actor().value().await {
                            Ok(Value::Tag(tag, _)) => match tag.tag() {
                                "Dashed" => "dashed",
                                "Dotted" => "dotted",
                                _ => "solid",
                            },
                            _ => "solid",
                        }
                    } else {
                        "solid"
                    };

                    // Parse color (required)
                    if let Some(color_var) = obj.variable("color") {
                        if let Ok(color_value) = color_var.value_actor().value().await {
                            if let Some(css_color) = oklch_to_css(color_value).await {
                                // Build typed Outline value
                                let mut outline = if is_inner {
                                    zoon::Outline::inner()
                                } else {
                                    zoon::Outline::outer()
                                };
                                outline = outline.width(thickness).color(css_color);
                                outline = match line_style {
                                    "dashed" => outline.dashed(),
                                    "dotted" => outline.dotted(),
                                    _ => outline.solid(),
                                };
                                return Some(outline);
                            }
                        }
                    }
                    None
                })
                .chain(stream::pending())
                .boxed_local()
            }
            _ => stream::pending::<Option<zoon::Outline>>().boxed_local(),
        }
    }));

    // AlignContent (row: horizontal content alignment, column: vertical content alignment)
    // Uses Zoon's AlignContent API which controls how CONTAINER aligns its CHILDREN
    // (unlike Align which positions elements within their parent)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.

    // Horizontal alignment signal (align.row) - produces Option<HorizontalAlignment>
    #[derive(Clone, Copy, Debug)]
    enum HorizontalContentAlignment {
        Center,
        Left,
        Right,
    }

    let sv_align_row = tagged_object.expect_variable("settings");
    let horizontal_content_align_signal = signal::from_stream({
        let style_stream = switch_map(sv_align_row.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let align_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("align") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(align_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("row") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => match tag.tag() {
                    "Center" => Some(HorizontalContentAlignment::Center),
                    "Start" => Some(HorizontalContentAlignment::Left),
                    "End" => Some(HorizontalContentAlignment::Right),
                    _ => None,
                },
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    })
    .broadcast();

    // Vertical alignment signal (align.column) - produces Option<VerticalContentAlignment>
    #[derive(Clone, Copy, Debug)]
    enum VerticalContentAlignment {
        Center,
        Top,
        Bottom,
    }

    let sv_align_col = tagged_object.expect_variable("settings");
    let vertical_content_align_signal = signal::from_stream({
        let style_stream = switch_map(sv_align_col.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let align_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("align") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(align_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("column") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => match tag.tag() {
                    "Center" => Some(VerticalContentAlignment::Center),
                    "Start" => Some(VerticalContentAlignment::Top),
                    "End" => Some(VerticalContentAlignment::Bottom),
                    _ => None,
                },
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    })
    .broadcast();

    // Combined content alignment signal - combines horizontal and vertical into AlignContent values
    // Uses map_ref! to combine both signals when either changes
    let combined_content_align_signal = map_ref! {
        let h_align = horizontal_content_align_signal.signal(),
        let v_align = vertical_content_align_signal.signal() =>
        {
            let mut align = AlignContent::new();
            // Apply horizontal content alignment
            if let Some(h) = h_align {
                align = match h {
                    HorizontalContentAlignment::Center => align.center_x(),
                    HorizontalContentAlignment::Left => align.left(),
                    HorizontalContentAlignment::Right => align.right(),
                };
            }
            // Apply vertical content alignment
            if let Some(v) = v_align {
                align = match v {
                    VerticalContentAlignment::Center => align.center_y(),
                    VerticalContentAlignment::Top => align.top(),
                    VerticalContentAlignment::Bottom => align.bottom(),
                };
            }
            align
        }
    };

    // Use switch_map for items stream - critical for proper re-rendering when example switches
    let items_vec_diff_stream = switch_map(
        switch_map(settings_variable.stream(), |value| {
            let object = value.expect_object();
            object.expect_variable("items").stream()
        }),
        |value| {
            // value.expect_list() returns Arc<List> which is kept alive by the stream
            list_change_to_vec_diff_stream(value.expect_list().stream())
        },
    );

    // Raw CSS properties (no Zoon typed equivalent) — line-height, font-smoothing, text-shadow
    let sv_line_height = tagged_object.expect_variable("settings");
    let line_height_css_signal = signal::from_stream({
        let style_stream = switch_map(sv_line_height.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("line_height") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(format!("{}", n.number())),
                _ => None,
            })
        })
        .boxed_local()
    });

    let sv_font_smoothing = tagged_object.expect_variable("settings");
    let font_smoothing_css_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_smoothing.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font_smoothing") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match &value {
                Value::Tag(tag, _) if tag.tag() == "Antialiased" => Some("antialiased".to_string()),
                _ => None,
            })
        })
        .boxed_local()
    });

    let sv_text_shadow = tagged_object.expect_variable("settings");
    let text_shadow_css_signal = signal::from_stream({
        let style_stream = switch_map(sv_text_shadow.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("text_shadow") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            if let Value::Object(obj, _) = &value {
                async fn get_num(obj: &Object, name: &str) -> f64 {
                    if let Some(v) = obj.variable(name) {
                        match v.value_actor().current_value().await {
                            Ok(Value::Number(n, _)) => n.number(),
                            _ => 0.0,
                        }
                    } else {
                        0.0
                    }
                }
                let x = get_num(obj, "x").await;
                let y = get_num(obj, "y").await;
                let blur = get_num(obj, "blur").await;
                let color_css = if let Some(color_var) = obj.variable("color") {
                    match color_var.value_actor().current_value().await {
                        Ok(Value::TaggedObject(tagged, _)) if tagged.tag() == "Oklch" => {
                            async fn get_oklch(
                                tagged: &TaggedObject,
                                name: &str,
                                default: f64,
                            ) -> f64 {
                                if let Some(v) = tagged.variable(name) {
                                    match v.value_actor().value().await {
                                        Ok(Value::Number(n, _)) => n.number(),
                                        _ => default,
                                    }
                                } else {
                                    default
                                }
                            }
                            let l = get_oklch(&tagged, "lightness", 0.5).await;
                            let c = get_oklch(&tagged, "chroma", 0.0).await;
                            let h = get_oklch(&tagged, "hue", 0.0).await;
                            let a = get_oklch(&tagged, "alpha", 1.0).await;
                            if a < 1.0 {
                                format!("oklch({}% {} {} / {})", l * 100.0, c, h, a)
                            } else {
                                format!("oklch({}% {} {})", l * 100.0, c, h)
                            }
                        }
                        _ => "rgba(0,0,0,0.5)".to_string(),
                    }
                } else {
                    "rgba(0,0,0,0.5)".to_string()
                };
                Some(format!("{}px {}px {}px {}", x, y, blur, color_css))
            } else {
                None
            }
        })
        .boxed_local()
    });

    Stripe::new()
        .direction_signal(signal::from_stream(direction_stream).map(Option::unwrap_or_default))
        .items_signal_vec(VecDiffStreamSignalVec(items_vec_diff_stream).map_signal(
            move |value_actor| {
                signal::from_stream(actor_current_or_future_stream(value_actor).map({
                    let construct_context = construct_context.clone();
                    move |value| value_to_element(value, construct_context.clone())
                }))
            },
        ))
        .on_hovered_change(move |is_hovered| {
            // Capture Lamport time NOW at DOM callback, before channel
            hovered_sender.send_or_drop(TimestampedEvent::now(is_hovered));
        })
        // Typed styles
        .s(Gap::both_signal(signal::from_stream(gap_stream)))
        .s(Background::new().color_signal(background_signal))
        .s(Font::new()
            .size_signal(font_size_signal)
            .color_signal(font_color_signal)
            .weight_signal(font_weight_signal)
            .family_signal(font_family_typed_signal.map(|opt| opt.unwrap_or_default())))
        .s(Padding::new()
            .top_signal(padding_top_signal)
            .right_signal(padding_right_signal)
            .bottom_signal(padding_bottom_signal)
            .left_signal(padding_left_signal))
        .s(Font::with_signal_self(font_align_font_signal))
        .s(Width::with_signal_self(width_typed_signal))
        .s(Height::with_signal_self(height_typed_signal))
        .s(Shadows::with_signal(
            shadows_typed_signal.map(|opt| opt.unwrap_or_default()),
        ))
        .s(Borders::new().top_signal(border_top_typed_signal))
        .s(Outline::with_signal_self(
            outline_signal.map(|opt| opt.flatten()),
        ))
        .s(AlignContent::with_signal_self(
            combined_content_align_signal,
        ))
        .s(Visible::with_signal(visible_sig))
        // Raw CSS properties without Zoon typed equivalents
        .update_raw_el(move |raw_el| {
            raw_el
                .style_signal("overflow", scrollbars_css_signal)
                .style_signal("line-height", line_height_css_signal)
                .style_signal("text-shadow", text_shadow_css_signal)
                // Font-smoothing via raw DOM API — dominator panics on unsupported CSS properties
                .update_dom_builder(move |dom_builder| {
                    let element: web_sys::Element = dom_builder.__internal_element().into();
                    dom_builder.future(font_smoothing_css_signal.for_each(move |value| {
                        let style = element.unchecked_ref::<web_sys::HtmlElement>().style();
                        match value {
                            Some(v) => {
                                let _ = style.set_property("-webkit-font-smoothing", &v);
                                let _ = style.set_property("-moz-osx-font-smoothing", "grayscale");
                            }
                            None => {
                                let _ = style.remove_property("-webkit-font-smoothing");
                                let _ = style.remove_property("-moz-osx-font-smoothing");
                            }
                        }
                        async {}
                    }))
                })
        })
        .update_raw_el(move |raw_el| apply_physical_css(raw_el, &sv_physical, &ctx_physical))
        // Keep tagged_object alive for the lifetime of this element
        .after_remove(move |_| {
            drop(tagged_object);
            drop(hovered_handler_loop);
        })
}

fn element_stack(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let is_scene = construct_context.scene_ctx.is_some();
    let settings_variable = tagged_object.expect_variable("settings");
    let sv_physical = settings_variable.clone();
    let ctx_physical = construct_context.clone();

    // NOTE: Arc-wrapped Objects/Variables stay alive through the stream chain.
    // No Mutex needed - expect_variable returns Arc<Variable>, expect_list returns Arc<List>.

    // Use switch_map for layers stream - critical for proper re-rendering when example switches
    let layers_vec_diff_stream = switch_map(
        switch_map(settings_variable.clone().stream(), |value| {
            let object = value.expect_object();
            object.expect_variable("layers").stream()
        }),
        |value| list_change_to_vec_diff_stream(value.expect_list().stream()),
    );

    // Create individual style streams directly from settings
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let settings_variable_2 = tagged_object.expect_variable("settings");
    let settings_variable_3 = tagged_object.expect_variable("settings");
    let settings_variable_4 = tagged_object.expect_variable("settings");
    let sv_visible = tagged_object.expect_variable("settings");
    let visible_sig = visible_signal_from_settings(sv_visible);

    // Width - produces u32 for typed Width API
    let width_signal = signal::from_stream({
        let style_stream = switch_map(settings_variable_2.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("width") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
    });

    // Height - produces u32 for typed Height API
    let height_signal = signal::from_stream({
        let style_stream = switch_map(settings_variable_3.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("height") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
    });

    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    // In scene mode, physical CSS handles background-color via material.color — skip typed API
    let background_signal = signal::from_stream(if is_scene {
        stream::pending::<String>().boxed_local()
    } else {
        let style_stream = switch_map(settings_variable_4.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let bg_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("background") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(bg_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    Stack::new()
        .s(Width::exact_signal(width_signal))
        .s(Height::exact_signal(height_signal))
        .s(Background::new().color_signal(background_signal))
        .s(Visible::with_signal(visible_sig))
        .layers_signal_vec(VecDiffStreamSignalVec(layers_vec_diff_stream).map_signal(
            move |value_actor| {
                signal::from_stream(actor_current_or_future_stream(value_actor).map({
                    let construct_context = construct_context.clone();
                    move |value| value_to_element(value, construct_context.clone())
                }))
            },
        ))
        .update_raw_el(move |raw_el| apply_physical_css(raw_el, &sv_physical, &ctx_physical))
        // Keep tagged_object alive for the lifetime of this element
        .after_remove(move |_| {
            drop(tagged_object);
        })
}

/// Convert color value to CSS color string
/// Handles both Oklch[...] tagged objects and plain color tags like White, Black, etc.
async fn oklch_to_css(value: Value) -> Option<String> {
    match value {
        Value::TaggedObject(tagged, _) => {
            if tagged.tag() == "Oklch" {
                // Helper to extract number from Variable (waits for first value if needed)
                async fn get_num(tagged: &TaggedObject, name: &str, default: f64) -> f64 {
                    if let Some(v) = tagged.variable(name) {
                        // Use value() which checks version first, then streams if needed
                        match v.value_actor().value().await {
                            Ok(Value::Number(n, _)) => n.number(),
                            _ => default,
                        }
                    } else {
                        default
                    }
                }

                let lightness = get_num(&tagged, "lightness", 0.5).await; // 0.5 = visible gray as fallback
                let chroma = get_num(&tagged, "chroma", 0.0).await;
                let hue = get_num(&tagged, "hue", 0.0).await;
                let alpha = get_num(&tagged, "alpha", 1.0).await;

                // oklch(lightness chroma hue / alpha)
                // Lightness is 0-1, needs to be percentage
                let css = if alpha < 1.0 {
                    format!(
                        "oklch({}% {} {} / {})",
                        lightness * 100.0,
                        chroma,
                        hue,
                        alpha
                    )
                } else {
                    format!("oklch({}% {} {})", lightness * 100.0, chroma, hue)
                };
                return Some(css);
            }
            None
        }
        Value::Tag(tag, _) => {
            // Handle named CSS colors
            let color = match tag.tag() {
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
            Some(color.to_string())
        }
        _ => None,
    }
}

/// Convert a Boon color value to a Zoon Color.
/// Supports Oklch tagged objects and named color tags.
/// Unlike oklch_to_css which returns CSS strings, this returns typed Zoon Color values
/// for use with typed style APIs like Shadow::color(), Border::color(), etc.
async fn oklch_to_color(value: Value) -> Option<Color> {
    match value {
        Value::TaggedObject(tagged, _) if tagged.tag() == "Oklch" => {
            // Helper to extract number from Variable (waits for first value if needed)
            async fn get_num(tagged: &TaggedObject, name: &str, default: f64) -> f64 {
                if let Some(v) = tagged.variable(name) {
                    match v.value_actor().value().await {
                        Ok(Value::Number(n, _)) => n.number(),
                        _ => default,
                    }
                } else {
                    default
                }
            }

            let lightness = get_num(&tagged, "lightness", 0.5).await;
            let chroma = get_num(&tagged, "chroma", 0.0).await;
            let hue = get_num(&tagged, "hue", 0.0).await;
            let alpha = get_num(&tagged, "alpha", 1.0).await;

            // Create Zoon Color using oklch() builder
            Some(oklch().l(lightness).c(chroma).h(hue).a(alpha).into_color())
        }
        Value::Tag(tag, _) => {
            // Handle named CSS colors by parsing them as CSS strings
            let css_color = match tag.tag() {
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
            // Parse the CSS color string into a Zoon Color
            Some(css_color.into_color())
        }
        _ => None,
    }
}

/// Create a reactive stream that emits CSS color strings whenever Oklch components change.
/// This fixes the bug where Oklch internal variables (lightness, chroma, hue) weren't subscribed to.
/// When any Oklch component (lightness, chroma, hue, alpha) changes, a new CSS string is emitted.
fn oklch_to_css_stream(value: Value) -> LocalBoxStream<'static, String> {
    match value {
        Value::TaggedObject(tagged, _) if tagged.tag() == "Oklch" => {
            // Create streams for each component, with defaults for missing variables
            // Use enum to identify which component is emitting
            #[derive(Clone, Copy)]
            enum Component {
                Lightness,
                Chroma,
                Hue,
                Alpha,
            }

            let lightness_stream: LocalBoxStream<'static, (Component, f64)> =
                if let Some(v) = tagged.variable("lightness") {
                    v.stream()
                        .filter_map(|val| {
                            future::ready(match &val {
                                Value::Number(n, _) => Some((Component::Lightness, n.number())),
                                _ => None,
                            })
                        })
                        .boxed_local()
                } else {
                    stream::once(future::ready((Component::Lightness, 0.5)))
                        .chain(stream::pending())
                        .boxed_local()
                };

            let chroma_stream: LocalBoxStream<'static, (Component, f64)> =
                if let Some(v) = tagged.variable("chroma") {
                    v.stream()
                        .filter_map(|val| {
                            future::ready(match val {
                                Value::Number(n, _) => Some((Component::Chroma, n.number())),
                                _ => None,
                            })
                        })
                        .boxed_local()
                } else {
                    stream::once(future::ready((Component::Chroma, 0.0)))
                        .chain(stream::pending())
                        .boxed_local()
                };

            let hue_stream: LocalBoxStream<'static, (Component, f64)> =
                if let Some(v) = tagged.variable("hue") {
                    v.stream()
                        .filter_map(|val| {
                            future::ready(match val {
                                Value::Number(n, _) => Some((Component::Hue, n.number())),
                                _ => None,
                            })
                        })
                        .boxed_local()
                } else {
                    stream::once(future::ready((Component::Hue, 0.0)))
                        .chain(stream::pending())
                        .boxed_local()
                };

            let alpha_stream: LocalBoxStream<'static, (Component, f64)> =
                if let Some(v) = tagged.variable("alpha") {
                    v.stream()
                        .filter_map(|val| {
                            future::ready(match val {
                                Value::Number(n, _) => Some((Component::Alpha, n.number())),
                                _ => None,
                            })
                        })
                        .boxed_local()
                } else {
                    stream::once(future::ready((Component::Alpha, 1.0)))
                        .chain(stream::pending())
                        .boxed_local()
                };

            // Combine all streams - emit new CSS whenever any component changes
            // Use scan to maintain state of all components
            stream::select_all([lightness_stream, chroma_stream, hue_stream, alpha_stream])
                .scan((0.5, 0.0, 0.0, 1.0), |state, (component, value)| {
                    match component {
                        Component::Lightness => state.0 = value,
                        Component::Chroma => state.1 = value,
                        Component::Hue => state.2 = value,
                        Component::Alpha => state.3 = value,
                    }
                    let (l, c, h, a) = *state;
                    let css = if a < 1.0 {
                        format!("oklch({}% {} {} / {})", l * 100.0, c, h, a)
                    } else {
                        format!("oklch({}% {} {})", l * 100.0, c, h)
                    };
                    future::ready(Some(css))
                })
                .boxed_local()
        }
        Value::Tag(tag, _) => {
            // Handle named CSS colors - return constant infinite stream
            let color = match tag.tag() {
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
                _ => return stream::empty().boxed_local(),
            };
            stream::once(future::ready(color.to_string()))
                .chain(stream::pending())
                .boxed_local()
        }
        _ => stream::empty().boxed_local(),
    }
}

/// Hybrid reactive Oklch-to-CSS conversion.
///
/// 1. Reads all 4 Oklch components atomically via `current_value().await` → correct initial CSS
/// 2. Subscribes to each component via `stream_from_now()` → only future updates, no replay
/// 3. `select_all + scan` initialized with the correct values from step 1
///
/// This avoids both problems:
/// - `oklch_to_css_stream`: race condition from `scan` starting at wrong defaults (0.5, 0, 0, 1.0)
/// - `oklch_to_css_oneshot`: not reactive (reads once, then goes pending)
fn oklch_to_css_reactive(value: Value) -> LocalBoxStream<'static, String> {
    match value {
        Value::TaggedObject(tagged, _) if tagged.tag() == "Oklch" => {
            stream::once(async move {
                // Step 1: Read current values atomically
                async fn read_f64(tagged: &TaggedObject, name: &str, default: f64) -> f64 {
                    match tagged.variable(name) {
                        Some(v) => match v.value_actor().current_value().await {
                            Ok(Value::Number(n, _)) => n.number(),
                            _ => default,
                        },
                        None => default,
                    }
                }
                let l = read_f64(&tagged, "lightness", 0.5).await;
                let c = read_f64(&tagged, "chroma", 0.0).await;
                let h = read_f64(&tagged, "hue", 0.0).await;
                let a = read_f64(&tagged, "alpha", 1.0).await;

                let initial_css = format_oklch(l, c, h, a);

                // Step 2: Subscribe to future changes via stream_from_now()
                #[derive(Clone, Copy)]
                enum Comp {
                    L,
                    C,
                    H,
                    A,
                }

                fn comp_stream(
                    tagged: &TaggedObject,
                    name: &str,
                    comp: Comp,
                ) -> LocalBoxStream<'static, (Comp, f64)> {
                    match tagged.variable(name) {
                        Some(v) => v
                            .stream_from_now()
                            .filter_map(move |val| {
                                future::ready(match val {
                                    Value::Number(n, _) => Some((comp, n.number())),
                                    _ => None,
                                })
                            })
                            .boxed_local(),
                        None => stream::pending().boxed_local(),
                    }
                }

                let updates = stream::select_all([
                    comp_stream(&tagged, "lightness", Comp::L),
                    comp_stream(&tagged, "chroma", Comp::C),
                    comp_stream(&tagged, "hue", Comp::H),
                    comp_stream(&tagged, "alpha", Comp::A),
                ])
                .scan((l, c, h, a), move |state, (comp, value)| {
                    match comp {
                        Comp::L => state.0 = value,
                        Comp::C => state.1 = value,
                        Comp::H => state.2 = value,
                        Comp::A => state.3 = value,
                    }
                    future::ready(Some(format_oklch(state.0, state.1, state.2, state.3)))
                });

                // Step 3: Chain initial + reactive updates
                stream::once(future::ready(initial_css)).chain(updates)
            })
            .flatten()
            .boxed_local()
        }
        Value::Tag(tag, _) => {
            let color = match tag.tag() {
                "White" => "white",
                "Black" => "black",
                "Red" => "red",
                "Green" => "green",
                "Blue" => "blue",
                "Transparent" => "transparent",
                _ => return stream::empty().boxed_local(),
            };
            stream::once(future::ready(color.to_string()))
                .chain(stream::pending())
                .boxed_local()
        }
        _ => stream::empty().boxed_local(),
    }
}

fn format_oklch(l: f64, c: f64, h: f64, a: f64) -> String {
    if a < 1.0 {
        format!("oklch({}% {} {} / {})", l * 100.0, c, h, a)
    } else {
        format!("oklch({}% {} {})", l * 100.0, c, h)
    }
}

fn element_button(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let is_scene = construct_context.scene_ctx.is_some();
    let sv_physical = tagged_object.expect_variable("settings");
    let ctx_physical = construct_context.clone();
    let disabled_sig = disabled_signal_from_settings(tagged_object.expect_variable("settings"));

    // TimestampedEvent captures Lamport time at DOM callback for consistent ordering
    let (press_event_sender, mut press_event_receiver) = NamedChannel::<TimestampedEvent<()>>::new(
        "button.press_event",
        BRIDGE_PRESS_EVENT_CAPACITY,
    );
    let (hovered_sender, mut hovered_receiver) =
        NamedChannel::<TimestampedEvent<bool>>::new("button.hovered", BRIDGE_HOVER_CAPACITY);

    let element_variable = tagged_object.expect_variable("element");

    // Set up press event handler - use same subscription pattern as text_input
    // Chain with pending() to prevent stream termination, which would cause busy-polling
    // in the select! loop (fused stream returns Ready(None) immediately when exhausted)
    // Use switch_map (not flat_map) because variable.stream() is infinite.
    // When element is recreated, switch_map cancels old subscription and re-subscribes to new one.
    let mut press_stream = switch_map(
        variable_current_or_future_stream(element_variable.clone())
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    )
    .filter_map(|value| future::ready(value.expect_object().variable("press")))
    .map(|variable| variable.expect_link_value_sender())
    .chain(stream::pending())
    .fuse();

    // Set up hovered link if element field exists with hovered property
    // Chain with pending() to prevent stream termination (same as press_stream)
    let hovered_stream = variable_current_or_future_stream(element_variable.clone())
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending());

    let event_handler_loop = ActorLoop::new({
        let construct_context_for_events = construct_context.clone();
        async move {
            let mut press_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut hovered_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut press_event_object_value_version = 0u64;
            let mut hovered_stream = hovered_stream.fuse();
            let mut last_hover_state: Option<bool> = None;
            loop {
                select! {
                    new_press_link_value_sender = press_stream.next() => {
                        if let Some(new_press_link_value_sender) = new_press_link_value_sender {
                            press_link_value_sender = Some(new_press_link_value_sender);
                        }
                    }
                    new_sender = hovered_stream.next() => {
                        if let Some(sender) = new_sender {
                            // Send initial hover state (false) when link is established
                            let initial_hover_value = EngineTag::new_value_cached(
                                HOVER_TAG_INFO.with(|info| info.clone()),
                                ValueIdempotencyKey::new(),
                                "False",
                            );
                            sender.send_or_drop(initial_hover_value);
                            last_hover_state = Some(false);
                            hovered_link_value_sender = Some(sender);
                        }
                    }
                    event = press_event_receiver.select_next_some() => {
                        if let Some(press_link_value_sender) = press_link_value_sender.as_ref() {
                            let press_event_object_value = Object::new_value_with_lamport_time(
                                ConstructInfo::new(format!("bridge::element_button::press_event, version: {press_event_object_value_version}"), None, "Button press event"),
                                construct_context_for_events.clone(),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                [],
                            );
                            press_event_object_value_version += 1;
                            press_link_value_sender.send_or_drop(press_event_object_value);
                        }
                    }
                    event = hovered_receiver.select_next_some() => {
                        if last_hover_state == Some(event.data) {
                            inc_metric!(HOVER_EVENTS_DEDUPED);
                            continue;
                        }
                        if let Some(sender) = hovered_link_value_sender.as_ref() {
                            inc_metric!(HOVER_EVENTS_EMITTED);
                            last_hover_state = Some(event.data);
                            let hover_tag = if event.data { "True" } else { "False" };
                            let event_value = EngineTag::new_value_cached_with_lamport_time(
                                HOVER_TAG_INFO.with(|info| info.clone()),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                hover_tag,
                            );
                            sender.send_or_drop(event_value);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");
    let sv_visible = tagged_object.expect_variable("settings");
    let visible_sig = visible_signal_from_settings(sv_visible);

    // CRITICAL: Use switch_map (not flat_map) because label variable stream is infinite.
    // When settings change, switch_map cancels the old label subscription.
    let label_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("label").stream()
    })
    .map({
        let _construct_context = construct_context.clone();
        move |value| value_to_element(value, construct_context.clone())
    });

    // Font size signal
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_size = settings_variable.clone();
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_size.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = if let Value::Number(n, _) = value {
                Some(n.number() as u32)
            } else {
                None
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Font color signal
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let sv_font_color = settings_variable.clone();
    let font_color_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_color.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(font_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    // Padding signal - produces tuple (top, right, bottom, left) as u32
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_padding = settings_variable.clone();
    let padding_tuple_signal = signal::from_stream({
        let style_stream = switch_map(sv_padding.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("padding") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => {
                    let all = n.number() as u32;
                    Some((all, all, all, all))
                }
                Value::Object(obj, _) => {
                    let top = if let Some(v) = obj.variable("top") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("column") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let right = if let Some(v) = obj.variable("right") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("row") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let bottom = if let Some(v) = obj.variable("bottom") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("column") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let left = if let Some(v) = obj.variable("left") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("row") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    Some((top, right, bottom, left))
                }
                _ => None,
            }
        })
        .boxed_local()
    })
    .broadcast();
    // Derive individual padding signals from the broadcasted tuple
    let padding_top_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(t, _, _, _)| t));
    let padding_right_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, r, _, _)| r));
    let padding_bottom_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, _, b, _)| b));
    let padding_left_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, _, _, l)| l));

    // Size (width) signal
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_size_width = settings_variable.clone();
    let size_width_signal = signal::from_stream({
        let style_stream = switch_map(sv_size_width.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = if let Value::Number(n, _) = value {
                Some(n.number() as u32)
            } else {
                None
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Size (height) signal
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_size_height = settings_variable.clone();
    let size_height_signal = signal::from_stream({
        let style_stream = switch_map(sv_size_height.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = if let Value::Number(n, _) = value {
                Some(n.number() as u32)
            } else {
                None
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Rounded corners signal
    // In scene mode, physical CSS handles border-radius via rounded_corners — skip typed API
    let sv_rounded = settings_variable.clone();
    let rounded_signal = signal::from_stream(if is_scene {
        stream::pending::<u32>().boxed_local()
    } else {
        let style_stream = switch_map(sv_rounded.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("rounded_corners") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = if let Value::Number(n, _) = value {
                Some(n.number() as u32)
            } else {
                None
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Transform signal (move_left, move_down, rotate -> translate, rotate)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_transform = settings_variable.clone();
    let transform_signal = signal::from_stream({
        let style_stream = switch_map(sv_transform.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("transform") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            if let Value::Object(obj, _) = value {
                let move_left = if let Some(v) = obj.variable("move_left") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        n.number()
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                let move_down = if let Some(v) = obj.variable("move_down") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        n.number()
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                let move_up = if let Some(v) = obj.variable("move_up") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        n.number()
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                let move_right = if let Some(v) = obj.variable("move_right") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        n.number()
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                let rotate = if let Some(v) = obj.variable("rotate") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        n.number()
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };

                // Build typed Transform value
                let mut transform = zoon::Transform::new();
                if move_left != 0.0 {
                    transform = transform.move_left(move_left);
                }
                if move_right != 0.0 {
                    transform = transform.move_right(move_right);
                }
                if move_up != 0.0 {
                    transform = transform.move_up(move_up);
                }
                if move_down != 0.0 {
                    transform = transform.move_down(move_down);
                }
                if rotate != 0.0 {
                    transform = transform.rotate(rotate);
                }
                // Return None if no transformations were applied
                if move_left == 0.0
                    && move_right == 0.0
                    && move_up == 0.0
                    && move_down == 0.0
                    && rotate == 0.0
                {
                    None
                } else {
                    Some(transform)
                }
            } else {
                None
            }
        })
        .boxed_local()
    });

    // Font align - produces Font values for typed API (text-align)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_align = settings_variable.clone();
    let font_align_font_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_align.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("align") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result: Option<Font<'static>> = match value {
                Value::Tag(tag, _) => match tag.tag() {
                    "Center" => Some(Font::new().center()),
                    "Left" => Some(Font::new().left()),
                    "Right" => Some(Font::new().right()),
                    "Justify" => Some(Font::new().justify()),
                    _ => None,
                },
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Align signal - self-alignment within parent container
    let sv_align = settings_variable.clone();
    let align_signal = signal::from_stream({
        let style_stream = switch_map(sv_align.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("align") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .map(|value| match &value {
            Value::Tag(tag, _) => {
                let mut align = zoon::Align::new();
                match tag.tag() {
                    "Right" => {
                        align = align.right();
                    }
                    "Left" => {
                        align = align.left();
                    }
                    "Top" => {
                        align = align.top();
                    }
                    "Bottom" => {
                        align = align.bottom();
                    }
                    "Center" => {
                        align = align.center_x().center_y();
                    }
                    _ => {}
                }
                Some(align)
            }
            _ => None,
        })
        .boxed_local()
    });

    // Background color signal
    // In scene mode, physical CSS handles background-color via material.color — skip typed API
    let sv_background = settings_variable.clone();
    let background_signal = signal::from_stream(if is_scene {
        stream::pending::<String>().boxed_local()
    } else {
        let style_stream = switch_map(sv_background.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let bg_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("background") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(bg_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    // Outline signal - handles both NoOutline tag and Object with color/thickness/style fields.
    // CRITICAL: Uses nested switch_map (not flat_map) because all variable streams are infinite.
    // The innermost switch_map handles pending() streams from NoOutline tag.
    let sv_outline = settings_variable.clone();
    let button_id = Arc::as_ptr(&sv_outline) as usize;
    let outline_value_stream = {
        let style_stream = switch_map(sv_outline.stream(), move |value| {
            if LOG_DEBUG {
                zoon::println!(
                    "[OUTLINE_DEBUG btn={:x}] sv_outline emitted, getting style variable",
                    button_id
                );
            }
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, move |value| {
            if LOG_DEBUG {
                zoon::println!(
                    "[OUTLINE_DEBUG btn={:x}] style_stream emitted, getting outline variable",
                    button_id
                );
            }
            let obj = value.expect_object();
            match obj.variable("outline") {
                Some(var) => {
                    if LOG_DEBUG {
                        zoon::println!(
                            "[OUTLINE_DEBUG btn={:x}] Got outline variable, subscribing to its value_actor",
                            button_id
                        );
                    }
                    var.value_actor()
                        .clone()
                        .stream()
                        .map(move |v| {
                            if LOG_DEBUG {
                                zoon::println!(
                                    "[OUTLINE_DEBUG btn={:x}] outline value_actor emitted: {}",
                                    button_id,
                                    match &v {
                                        Value::Tag(t, _) => format!("Tag({})", t.tag()),
                                        Value::Object(_, _) => "Object".to_string(),
                                        _ => "other".to_string(),
                                    }
                                );
                            }
                            v
                        })
                        .left_stream()
                }
                None => stream::empty().right_stream(),
            }
        })
    };
    let outline_signal = signal::from_stream(switch_map(outline_value_stream, |value| {
        match &value {
            Value::Tag(tag, _) if tag.tag() == "NoOutline" => {
                // Return None to remove outline
                stream::once(future::ready(None::<zoon::Outline>))
                    .chain(stream::pending())
                    .boxed_local()
            }
            Value::Object(obj, _) => {
                let obj = obj.clone();
                stream::once(async move {
                        // Parse thickness (default: 1)
                        let thickness = if let Some(thickness_var) = obj.variable("thickness") {
                            match thickness_var.value_actor().value().await {
                                Ok(Value::Number(n, _)) => n.number() as u32,
                                _ => 1,
                            }
                        } else {
                            1
                        };

                        // Parse side: Inner or Outer (default: Outer)
                        let is_inner = if let Some(side_var) = obj.variable("side") {
                            match side_var.value_actor().value().await {
                                Ok(Value::Tag(tag, _)) => tag.tag() == "Inner",
                                _ => false,
                            }
                        } else {
                            false
                        };

                        // Parse line_style: solid (default), dashed, dotted
                        let line_style = if let Some(style_var) = obj.variable("line_style") {
                            match style_var.value_actor().value().await {
                                Ok(Value::Tag(tag, _)) => match tag.tag() {
                                    "Dashed" => "dashed",
                                    "Dotted" => "dotted",
                                    _ => "solid",
                                },
                                _ => "solid",
                            }
                        } else {
                            "solid"
                        };

                        // Parse color (required)
                        if let Some(color_var) = obj.variable("color") {
                            if let Ok(color_value) = color_var.value_actor().value().await {
                                if let Some(css_color) = oklch_to_css(color_value).await {
                                    if LOG_DEBUG { zoon::println!("[OUTLINE] Generated typed Outline: width={}, style={}, color={}, inner={}", thickness, line_style, css_color, is_inner); }
                                    // Build typed Outline value
                                    let mut outline = if is_inner {
                                        zoon::Outline::inner()
                                    } else {
                                        zoon::Outline::outer()
                                    };
                                    outline = outline.width(thickness).color(css_color);
                                    outline = match line_style {
                                        "dashed" => outline.dashed(),
                                        "dotted" => outline.dotted(),
                                        _ => outline.solid(),
                                    };
                                    return Some(outline);
                                } else {
                                    zoon::eprintln!("[OUTLINE] oklch_to_css returned None for color");
                                }
                            } else {
                                zoon::eprintln!("[OUTLINE] Failed to get color value from actor");
                            }
                        } else {
                            zoon::eprintln!("[OUTLINE] No 'color' variable in outline object");
                        }
                        None
                    })
                    .chain(stream::pending())
                    .boxed_local()
            }
            other => {
                log_unexpected_type("button outline", "Object or NoOutline tag", other);
                stream::pending::<Option<zoon::Outline>>().boxed_local()
            }
        }
    }));

    Button::new()
        .label_signal(signal::from_stream(label_stream).map(|label| {
            if let Some(label) = label {
                label
            } else {
                empty_text()
            }
        }))
        .on_click(move || {
            // Capture Lamport time NOW at DOM callback, before channel
            press_event_sender.send_or_drop(TimestampedEvent::now(()));
        })
        .on_hovered_change(move |is_hovered| {
            // Capture Lamport time NOW at DOM callback, before channel
            hovered_sender.send_or_drop(TimestampedEvent::now(is_hovered));
        })
        .s(Width::exact_signal(size_width_signal))
        .s(Height::exact_signal(size_height_signal))
        .s(RoundedCorners::all_signal(rounded_signal))
        .s(Transform::with_signal_self(transform_signal))
        .s(Outline::with_signal_self(
            outline_signal.map(|opt| opt.flatten()),
        ))
        .s(Background::new().color_signal(background_signal))
        .s(Font::new()
            .size_signal(font_size_signal)
            .color_signal(font_color_signal))
        .s(Padding::new()
            .top_signal(padding_top_signal)
            .right_signal(padding_right_signal)
            .bottom_signal(padding_bottom_signal)
            .left_signal(padding_left_signal))
        .s(Font::with_signal_self(font_align_font_signal))
        .s(Align::with_signal_self(
            align_signal.map(|opt| opt.flatten()),
        ))
        .s(Visible::with_signal(visible_sig))
        .update_raw_el(move |raw_el| {
            apply_physical_css(
                raw_el.attr_signal("disabled", disabled_sig),
                &sv_physical,
                &ctx_physical,
            )
        })
        .after_remove(move |_| {
            drop(event_handler_loop);
            drop(tagged_object);
        })
}

fn element_text_input(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let is_scene = construct_context.scene_ctx.is_some();
    let sv_physical = tagged_object.expect_variable("settings");
    let ctx_physical = construct_context.clone();
    let disabled_sig = disabled_signal_from_settings(tagged_object.expect_variable("settings"));

    #[derive(Debug, Clone)]
    struct KeyDownPayload {
        key: String,
        text: String,
    }

    enum TextInputDomEvent {
        Input(TimestampedEvent<String>),
        Change(TimestampedEvent<String>),
        KeyDown(TimestampedEvent<KeyDownPayload>),
    }

    if LOG_DEBUG {
        zoon::println!("[EVENT:TextInput:v2] element_text_input CALLED - creating new TextInput");
    }
    // Separate channels for each event type.
    // TimestampedEvent captures Lamport time at DOM callback, ensuring correct ordering
    // even when select! processes events out of order.
    let (dom_event_sender, mut dom_event_receiver) = NamedChannel::<TextInputDomEvent>::new(
        "text_input.dom_event",
        BRIDGE_KEY_DOWN_CAPACITY.max(BRIDGE_TEXT_CHANGE_CAPACITY),
    );
    let (blur_event_sender, mut blur_event_receiver) =
        NamedChannel::<TimestampedEvent<()>>::new("text_input.blur", BRIDGE_BLUR_CAPACITY);
    let (focus_event_sender, mut focus_event_receiver) =
        NamedChannel::<TimestampedEvent<()>>::new("text_input.focus", BRIDGE_FOCUS_CAPACITY);
    let dom_input_el: Rc<RefCell<Option<web_sys::HtmlInputElement>>> = Default::default();
    let suppress_next_blur: Rc<RefCell<bool>> = Default::default();

    let element_variable = tagged_object.expect_variable("element");

    // Set up event handlers - create separate subscriptions for each event type
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    // CRITICAL: Use switch_map (not flat_map) because variable.stream() is infinite.
    // When element is recreated during example switching, switch_map cancels old subscription
    // and re-subscribes to the new element's event streams, preventing stale LINK bugs.
    let mut change_stream = switch_map(
        variable_current_or_future_stream(element_variable.clone())
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    )
    .filter_map(|value| future::ready(value.expect_object().variable("change")))
    .map(move |variable| variable.expect_link_value_sender())
    .chain(stream::pending())
    .fuse();

    let mut input_stream = switch_map(
        variable_current_or_future_stream(element_variable.clone())
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    )
    .filter_map(|value| future::ready(value.expect_object().variable("input")))
    .map(move |variable| variable.expect_link_value_sender())
    .chain(stream::pending())
    .fuse();

    let mut key_down_stream = switch_map(
        variable_current_or_future_stream(element_variable.clone())
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    )
    .filter_map(|value| future::ready(value.expect_object().variable("key_down")))
    .map(move |variable| variable.expect_link_value_sender())
    .chain(stream::pending())
    .fuse();

    let mut blur_stream = switch_map(
        variable_current_or_future_stream(element_variable.clone())
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    )
    .filter_map(|value| future::ready(value.expect_object().variable("blur")))
    .map(move |variable| variable.expect_link_value_sender())
    .chain(stream::pending())
    .fuse();

    let mut focus_stream = switch_map(
        variable_current_or_future_stream(element_variable.clone())
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    )
    .filter_map(|value| future::ready(value.expect_object().variable("focus")))
    .map(move |variable| variable.expect_link_value_sender())
    .chain(stream::pending())
    .fuse();

    // Helper to create change event value with captured Lamport timestamp
    fn create_change_event_value(
        construct_context: &ConstructContext,
        text: String,
        lamport_time: u64,
        scope_id: ScopeId,
    ) -> Value {
        inc_metric!(CHANGE_EVENTS_CONSTRUCTED);
        // C1: Use cached ConstructInfoComplete for the inner text value
        // to avoid ConstructInfo::new() allocation on every keystroke
        let text_value = EngineText::new_value_cached_with_lamport_time(
            CHANGE_EVENT_TEXT_INFO.with(|info| info.clone()),
            ValueIdempotencyKey::new(),
            lamport_time,
            text,
        );
        Object::new_value_with_lamport_time(
            ConstructInfo::new("text_input::change_event", None, "TextInput change event"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            lamport_time,
            [Variable::new_arc(
                ConstructInfo::new("text_input::change_event::text", None, "change text"),
                construct_context.clone(),
                "text",
                create_constant_actor(
                    ConstructInfo::new(
                        "text_input::change_event::text_actor",
                        None,
                        "change text actor",
                    ),
                    parser::PersistenceId::new(),
                    text_value,
                    scope_id,
                ),
                parser::PersistenceId::default(),
                parser::Scope::Root,
            )],
        )
    }

    fn create_input_event_value(
        construct_context: &ConstructContext,
        text: String,
        lamport_time: u64,
        scope_id: ScopeId,
    ) -> Value {
        inc_metric!(CHANGE_EVENTS_CONSTRUCTED);
        let text_value = EngineText::new_value_cached_with_lamport_time(
            CHANGE_EVENT_TEXT_INFO.with(|info| info.clone()),
            ValueIdempotencyKey::new(),
            lamport_time,
            text,
        );
        Object::new_value_with_lamport_time(
            ConstructInfo::new("text_input::input_event", None, "TextInput input event"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            lamport_time,
            [Variable::new_arc(
                ConstructInfo::new("text_input::input_event::text", None, "input text"),
                construct_context.clone(),
                "text",
                create_constant_actor(
                    ConstructInfo::new(
                        "text_input::input_event::text_actor",
                        None,
                        "input text actor",
                    ),
                    parser::PersistenceId::new(),
                    text_value,
                    scope_id,
                ),
                parser::PersistenceId::default(),
                parser::Scope::Root,
            )],
        )
    }

    // Helper to create key_down event value with captured Lamport timestamp
    fn create_key_down_event_value(
        construct_context: &ConstructContext,
        payload: KeyDownPayload,
        lamport_time: u64,
        scope_id: ScopeId,
    ) -> Value {
        inc_metric!(KEYDOWN_EVENTS_CONSTRUCTED);
        // C1: Use cached ConstructInfoComplete for the inner tag value
        let tag_value = EngineTag::new_value_cached_with_lamport_time(
            KEY_DOWN_EVENT_TAG_INFO.with(|info| info.clone()),
            ValueIdempotencyKey::new(),
            lamport_time,
            payload.key,
        );
        let text_value = EngineText::new_value_cached_with_lamport_time(
            KEY_DOWN_EVENT_TEXT_INFO.with(|info| info.clone()),
            ValueIdempotencyKey::new(),
            lamport_time,
            payload.text,
        );
        Object::new_value_with_lamport_time(
            ConstructInfo::new(
                "text_input::key_down_event",
                None,
                "TextInput key_down event",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            lamport_time,
            [
                Variable::new_arc(
                    ConstructInfo::new("text_input::key_down_event::key", None, "key_down key"),
                    construct_context.clone(),
                    "key",
                    create_constant_actor(
                        ConstructInfo::new(
                            "text_input::key_down_event::key_actor",
                            None,
                            "key_down key actor",
                        ),
                        parser::PersistenceId::new(),
                        tag_value,
                        scope_id,
                    ),
                    parser::PersistenceId::default(),
                    parser::Scope::Root,
                ),
                Variable::new_arc(
                    ConstructInfo::new("text_input::key_down_event::text", None, "key_down text"),
                    construct_context.clone(),
                    "text",
                    create_constant_actor(
                        ConstructInfo::new(
                            "text_input::key_down_event::text_actor",
                            None,
                            "key_down text actor",
                        ),
                        parser::PersistenceId::new(),
                        text_value,
                        scope_id,
                    ),
                    parser::PersistenceId::default(),
                    parser::Scope::Root,
                ),
            ],
        )
    }

    let event_handler_loop = ActorLoop::new({
        let construct_context_for_events = construct_context.clone();
        async move {
            if LOG_DEBUG {
                zoon::println!("[EVENT:TextInput] Event handler loop STARTED");
            }
            let scope_id = construct_context
                .bridge_scope_id
                .expect("Bug: bridge_scope_id not set for text_input event handler");
            let mut input_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut change_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut key_down_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut blur_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut focus_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut last_input_text: Option<String> = None;
            let mut last_change_text: Option<String> = None;

            // RACE CONDITION FIX: Buffer events that arrive before sender is ready.
            // When switching examples quickly, DOM events can arrive before the Boon
            // LINK subscription is established. Without buffering, these events are lost.
            // Text events use Option (keep-latest) since only the final text matters.
            // Key/blur/focus use bounded Vecs since ordering matters.
            let mut pending_input_event: Option<TimestampedEvent<String>> = None;
            let mut pending_change_event: Option<TimestampedEvent<String>> = None;
            let mut pending_key_down_events: Vec<TimestampedEvent<KeyDownPayload>> = Vec::new();
            let mut pending_blur_events: Vec<TimestampedEvent<()>> = Vec::new();
            let mut pending_focus_events: Vec<TimestampedEvent<()>> = Vec::new();

            loop {
                select! {
                    // These branches get the Boon-side senders for each event type
                    result = input_stream.next() => {
                        if let Some(sender) = result {
                            if LOG_DEBUG { zoon::println!("[EVENT:TextInput] input_link_value_sender READY"); }
                            if let Some(buffered_event) = pending_input_event.take() {
                                if LOG_DEBUG { zoon::println!("[EVENT:TextInput] Flushing buffered input event: lamport={}", buffered_event.lamport_time); }
                                sender.send_or_drop(create_input_event_value(&construct_context, buffered_event.data, buffered_event.lamport_time, scope_id));
                            }
                            input_link_value_sender = Some(sender);
                        }
                    }
                    result = change_stream.next() => {
                        if let Some(sender) = result {
                            if LOG_DEBUG { zoon::println!("[EVENT:TextInput] change_link_value_sender READY"); }
                            // Flush the latest buffered change event (only most recent matters)
                            if let Some(buffered_event) = pending_change_event.take() {
                                if LOG_DEBUG { zoon::println!("[EVENT:TextInput] Flushing buffered change event: lamport={}", buffered_event.lamport_time); }
                                sender.send_or_drop(create_change_event_value(&construct_context, buffered_event.data, buffered_event.lamport_time, scope_id));
                            }
                            change_link_value_sender = Some(sender);
                        }
                    }
                    result = key_down_stream.next() => {
                        if let Some(sender) = result {
                            if LOG_DEBUG { zoon::println!("[EVENT:TextInput] key_down_link_value_sender READY"); }
                            // Flush any buffered events first
                            for buffered_event in pending_key_down_events.drain(..) {
                                if LOG_DEBUG { zoon::println!("[EVENT:TextInput] Flushing buffered key_down event: key='{}', lamport={}", buffered_event.data.key, buffered_event.lamport_time); }
                                let event_value = create_key_down_event_value(&construct_context, buffered_event.data, buffered_event.lamport_time, scope_id);
                                let _ = sender.try_send(event_value);
                            }
                            key_down_link_value_sender = Some(sender);
                        }
                    }
                    result = blur_stream.next() => {
                        if let Some(sender) = result {
                            if LOG_DEBUG { zoon::println!("[EVENT:TextInput] blur_link_value_sender READY"); }
                            // Flush any buffered events first
                            for buffered_event in pending_blur_events.drain(..) {
                                if LOG_DEBUG { zoon::println!("[EVENT:TextInput] Flushing buffered blur event: lamport={}", buffered_event.lamport_time); }
                                let event_value = Object::new_value_with_lamport_time(
                                    ConstructInfo::new("text_input::blur_event", None, "TextInput blur event"),
                                    construct_context_for_events.clone(),
                                    ValueIdempotencyKey::new(),
                                    buffered_event.lamport_time,
                                    [],
                                );
                                sender.send_or_drop(event_value);
                            }
                            blur_link_value_sender = Some(sender);
                        }
                    }
                    result = focus_stream.next() => {
                        if let Some(sender) = result {
                            if LOG_DEBUG { zoon::println!("[EVENT:TextInput] focus_link_value_sender READY"); }
                            // Flush any buffered events first
                            for buffered_event in pending_focus_events.drain(..) {
                                if LOG_DEBUG { zoon::println!("[EVENT:TextInput] Flushing buffered focus event: lamport={}", buffered_event.lamport_time); }
                                let event_value = Object::new_value_with_lamport_time(
                                    ConstructInfo::new("text_input::focus_event", None, "TextInput focus event"),
                                    construct_context.clone(),
                                    ValueIdempotencyKey::new(),
                                    buffered_event.lamport_time,
                                    [],
                                );
                                sender.send_or_drop(event_value);
                            }
                            focus_link_value_sender = Some(sender);
                        }
                    }
                    event = focus_event_receiver.select_next_some() => {
                        if let Some(sender) = focus_link_value_sender.as_ref() {
                            let event_value = Object::new_value_with_lamport_time(
                                ConstructInfo::new("text_input::focus_event", None, "TextInput focus event"),
                                construct_context_for_events.clone(),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                [],
                            );
                            sender.send_or_drop(event_value);
                        } else {
                            if LOG_DEBUG { zoon::println!("[EVENT:TextInput] Buffering focus event (sender not ready)"); }
                            if pending_focus_events.len() < BRIDGE_PENDING_FOCUS_CAP {
                                pending_focus_events.push(event);
                            }
                        }
                    }
                    // TimestampedEvent carries Lamport time captured at DOM callback
                    // This ensures correct ordering even when select! processes events out of order
                    event = dom_event_receiver.select_next_some() => {
                        match event {
                            TextInputDomEvent::Input(event) => {
                                if LOG_DEBUG {
                                    zoon::println!("[EVENT:TextInput] LOOP received input: text='{}', lamport={}, sender_ready={}",
                                        if event.data.len() > 50 { format!("{}...", &event.data[..50]) } else { event.data.clone() },
                                        event.lamport_time,
                                        input_link_value_sender.is_some());
                                }
                                if last_input_text.as_ref() == Some(&event.data) {
                                    inc_metric!(CHANGE_EVENTS_DEDUPED);
                                    continue;
                                }
                                last_input_text = Some(event.data.clone());
                                if let Some(sender) = input_link_value_sender.as_ref() {
                                    sender.send_or_drop(create_input_event_value(&construct_context, event.data, event.lamport_time, scope_id));
                                } else {
                                    if LOG_DEBUG { zoon::println!("[EVENT:TextInput] Buffering input event (sender not ready)"); }
                                    pending_input_event = Some(event);
                                }
                            }
                            TextInputDomEvent::Change(event) => {
                                if LOG_DEBUG {
                                    zoon::println!("[EVENT:TextInput] LOOP received change: text='{}', lamport={}, sender_ready={}",
                                        if event.data.len() > 50 { format!("{}...", &event.data[..50]) } else { event.data.clone() },
                                        event.lamport_time,
                                        change_link_value_sender.is_some());
                                }
                                // Dedup: skip if text hasn't changed since last emission
                                if last_change_text.as_ref() == Some(&event.data) {
                                    inc_metric!(CHANGE_EVENTS_DEDUPED);
                                    continue;
                                }
                                last_change_text = Some(event.data.clone());
                                if let Some(sender) = change_link_value_sender.as_ref() {
                                    sender.send_or_drop(create_change_event_value(&construct_context, event.data, event.lamport_time, scope_id));
                                } else {
                                    // Buffer latest event until sender is ready (keep-latest, lossy)
                                    if LOG_DEBUG { zoon::println!("[EVENT:TextInput] Buffering change event (sender not ready)"); }
                                    pending_change_event = Some(event);
                                }
                            }
                            TextInputDomEvent::KeyDown(event) => {
                                if let Some(sender) = key_down_link_value_sender.as_ref() {
                                    let event_value = create_key_down_event_value(&construct_context, event.data, event.lamport_time, scope_id);
                                    let _ = sender.try_send(event_value);
                                } else {
                                    // Buffer event until sender is ready (bounded)
                                    if pending_key_down_events.len() < BRIDGE_PENDING_KEY_DOWN_CAP {
                                        pending_key_down_events.push(event);
                                    }
                                }
                            }
                        }
                    }
                    event = blur_event_receiver.select_next_some() => {
                        let blur_lamport = event.lamport_time;
                        if LOG_DEBUG { zoon::println!("[EVENT:TextInput] LOOP received blur: lamport={}, sender_ready={}", blur_lamport, blur_link_value_sender.is_some()); }
                        if let Some(sender) = blur_link_value_sender.as_ref() {
                            let event_value = Object::new_value_with_lamport_time(
                                ConstructInfo::new("text_input::blur_event", None, "TextInput blur event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                blur_lamport,
                                [],
                            );
                            sender.send_or_drop(event_value);
                        } else {
                            // Buffer event until sender is ready (bounded)
                            if LOG_DEBUG { zoon::println!("[EVENT:TextInput] Buffering blur event (sender not ready)"); }
                            if pending_blur_events.len() < BRIDGE_PENDING_BLUR_CAP {
                                pending_blur_events.push(event);
                            }
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");
    let sv_visible = tagged_object.expect_variable("settings");
    let visible_sig = visible_signal_from_settings(sv_visible);

    // CRITICAL: Use switch_map (not flat_map) because text variable stream is infinite.
    let text_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("text").stream()
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Text(text, _) => Some(text.text().to_string()),
            _ => None,
        })
    });

    // Placeholder text stream - extract actual text from placeholder object
    // CRITICAL: Use nested switch_map (not flat_map) because all variable streams are infinite.
    let placeholder_text_stream = switch_map(settings_variable.clone().stream(), |value| {
        value
            .expect_object()
            .expect_variable("placeholder")
            .stream()
    });
    let placeholder_text_stream = switch_map(placeholder_text_stream, |value| match value {
        Value::Object(obj, _) => match obj.variable("text") {
            Some(var) => var.stream().left_stream(),
            None => stream::empty().right_stream(),
        },
        _ => stream::empty().right_stream(),
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Text(text, _) => Some(text.text().to_string()),
            _ => None,
        })
    });

    // Placeholder signal for TextInput
    let placeholder_signal = signal::from_stream(placeholder_text_stream);

    // Placeholder font color signal: placeholder → style → font → color → oklch CSS
    let sv_ph_color = tagged_object.expect_variable("settings");
    let placeholder_color_signal = signal::from_stream({
        let ph_stream = switch_map(sv_ph_color.stream(), |value| {
            value
                .expect_object()
                .expect_variable("placeholder")
                .stream()
        });
        let style_stream = switch_map(ph_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("style") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(font_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    // Width signal from style
    // Width signal - produces Width values for typed API (supports Fill and pixel values)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_width = tagged_object.expect_variable("settings");
    let width_typed_signal = signal::from_stream({
        let style_stream = switch_map(sv_width.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("width") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(Width::exact(n.number() as u32)),
                Value::Tag(tag, _) if tag.tag() == "Fill" => Some(Width::fill()),
                _ => None,
            })
        })
    });

    // Padding signal from style - produces tuple (top, right, bottom, left) as u32
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_padding = tagged_object.expect_variable("settings");
    let padding_tuple_signal = signal::from_stream({
        let style_stream = switch_map(sv_padding.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("padding") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => {
                    let all = n.number() as u32;
                    Some((all, all, all, all))
                }
                Value::Object(obj, _) => {
                    // Handle directional padding: [top, column, left, right, row, bottom]
                    async fn get_num(obj: &Object, name: &str) -> u32 {
                        if let Some(v) = obj.variable(name) {
                            match v.value_actor().current_value().await {
                                Ok(Value::Number(n, _)) => n.number() as u32,
                                _ => 0,
                            }
                        } else {
                            0
                        }
                    }
                    let top = get_num(&obj, "top").await;
                    let bottom = get_num(&obj, "bottom").await;
                    let left = get_num(&obj, "left").await;
                    let right = get_num(&obj, "right").await;
                    let column = get_num(&obj, "column").await;
                    let row = get_num(&obj, "row").await;

                    // column applies to top/bottom, row applies to left/right
                    let final_top = if top > 0 { top } else { column };
                    let final_bottom = if bottom > 0 { bottom } else { column };
                    let final_left = if left > 0 { left } else { row };
                    let final_right = if right > 0 { right } else { row };

                    Some((final_top, final_right, final_bottom, final_left))
                }
                _ => None,
            }
        })
        .boxed_local()
    })
    .broadcast();
    // Derive individual padding signals from the broadcasted tuple
    let padding_top_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(t, _, _, _)| t));
    let padding_right_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, r, _, _)| r));
    let padding_bottom_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, _, b, _)| b));
    let padding_left_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, _, _, l)| l));

    // Font size signal from style
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_size = tagged_object.expect_variable("settings");
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_size.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
    });

    // Font color signal from style
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let sv_font_color = tagged_object.expect_variable("settings");
    let font_color_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_color.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(font_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    // Background color signal from style
    // In scene mode, physical CSS handles background-color via material.color — skip typed API
    let sv_bg_color = tagged_object.expect_variable("settings");
    let background_color_signal = signal::from_stream(if is_scene {
        stream::pending::<String>().boxed_local()
    } else {
        let style_stream = switch_map(sv_bg_color.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let bg_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("background") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(bg_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    // Focus signal - use Mutable with stream updates
    // Start with false, stream will set to true if focus: True is specified
    let focus_mutable = Mutable::new(false);
    let focus_signal = focus_mutable.signal();

    // Update the mutable when the stream emits
    // CRITICAL: Use switch_map (not flat_map) because focus variable stream is infinite.
    let focus_stream = switch_map(
        variable_current_or_future_stream(settings_variable),
        |value| {
            let focus_variable = value.expect_object().expect_variable("focus");
            variable_current_or_future_stream(focus_variable)
        },
    )
    .filter_map(|value| {
        future::ready(match value {
            Value::Tag(tag, _) => Some(tag.tag() == "True"),
            _ => None,
        })
    });

    // Task to update focus from stream - must be kept alive
    let focus_loop = ActorLoop::new({
        let focus_mutable = focus_mutable.clone();
        async move {
            futures_util::pin_mut!(focus_stream);
            while let Some(focus) = focus_stream.next().await {
                focus_mutable.set_neq(focus);
            }
        }
    });

    TextInput::new()
        .label_hidden("text input")
        .text_signal(signal::from_stream(text_stream).map(|t| t.unwrap_or_default()))
        .placeholder(
            Placeholder::with_signal(placeholder_signal.map(|t| t.unwrap_or_default()))
                .s(Font::new().color_signal(placeholder_color_signal)),
        )
        .update_raw_el({
            let sender = dom_event_sender.clone();
            let dom_input_el_ref = dom_input_el.clone();
            move |raw_el| {
                raw_el
                    .after_insert(move |input_el: web_sys::HtmlInputElement| {
                        *dom_input_el_ref.borrow_mut() = Some(input_el);
                    })
                    .event_handler(move |event: events::Input| {
                        let target: web_sys::HtmlInputElement =
                            event.target().unwrap().unchecked_into();
                        let event = TimestampedEvent::now(target.value());
                        if LOG_DEBUG {
                            zoon::println!(
                                "[EVENT:TextInput] on_input fired: text='{}', lamport={}",
                                if event.data.len() > 50 {
                                    format!("{}...", &event.data[..50])
                                } else {
                                    event.data.clone()
                                },
                                event.lamport_time
                            );
                        }
                        sender.send_or_drop(TextInputDomEvent::Input(event));
                    })
            }
        })
        .on_change({
            let sender = dom_event_sender.clone();
            move |text| {
                // Capture Lamport time NOW at DOM callback, before channel
                let event = TimestampedEvent::now(text);
                if LOG_DEBUG {
                    zoon::println!(
                        "[EVENT:TextInput] on_change fired: text='{}', lamport={}",
                        if event.data.len() > 50 {
                            format!("{}...", &event.data[..50])
                        } else {
                            event.data.clone()
                        },
                        event.lamport_time
                    );
                }
                sender.send_or_drop(TextInputDomEvent::Change(event));
            }
        })
        .on_key_down_event({
            let sender = dom_event_sender.clone();
            let dom_input_el_ref = dom_input_el.clone();
            let suppress_next_blur = suppress_next_blur.clone();
            move |event| {
                let target_input = match &event.raw_event {
                    zoon::RawKeyboardEvent::KeyDown(raw_event) => raw_event
                        .target()
                        .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok()),
                };
                let key_name = match event.key() {
                    Key::Enter => "Enter".to_string(),
                    Key::Escape => "Escape".to_string(),
                    Key::Other(k) => k.clone(),
                };
                let current_text = target_input
                    .as_ref()
                    .map(|input_el| input_el.value())
                    .or_else(|| {
                        dom_input_el_ref
                            .borrow()
                            .as_ref()
                            .map(|input_el| input_el.value())
                    })
                    .unwrap_or_default();
                let is_commit_key = matches!(key_name.as_str(), "Enter" | "Escape");
                if is_commit_key {
                    *suppress_next_blur.borrow_mut() = true;
                }

                if is_commit_key {
                    let ts_event = TimestampedEvent::now(KeyDownPayload {
                        key: key_name,
                        text: current_text,
                    });
                    if LOG_DEBUG {
                        zoon::println!(
                            "[EVENT:TextInput] on_key_down (commit) key='{}', lamport={}",
                            ts_event.data.key,
                            ts_event.lamport_time
                        );
                    }
                    sender.send_or_drop(TextInputDomEvent::KeyDown(ts_event));
                } else {
                    // Capture Lamport time NOW at DOM callback, before channel
                    let ts_event = TimestampedEvent::now(KeyDownPayload {
                        key: key_name,
                        text: current_text,
                    });
                    if LOG_DEBUG {
                        zoon::println!(
                            "[EVENT:TextInput] on_key_down fired: key='{}', lamport={}",
                            ts_event.data.key,
                            ts_event.lamport_time
                        );
                    }
                    sender.send_or_drop(TextInputDomEvent::KeyDown(ts_event));
                }
            }
        })
        .on_blur({
            let sender = blur_event_sender.clone();
            let suppress_next_blur = suppress_next_blur.clone();
            move || {
                if *suppress_next_blur.borrow() {
                    *suppress_next_blur.borrow_mut() = false;
                    return;
                }
                // Capture Lamport time NOW at DOM callback, before channel
                let event = TimestampedEvent::now(());
                if LOG_DEBUG {
                    zoon::println!(
                        "[EVENT:TextInput] on_blur fired: lamport={}",
                        event.lamport_time
                    );
                }
                sender.send_or_drop(event);
            }
        })
        .update_raw_el({
            let sender = focus_event_sender.clone();
            move |raw_el| {
                raw_el.event_handler(move |_: events::Focus| {
                    let event = TimestampedEvent::now(());
                    if LOG_DEBUG {
                        zoon::println!(
                            "[EVENT:TextInput] on_focus fired: lamport={}",
                            event.lamport_time
                        );
                    }
                    sender.send_or_drop(event);
                })
            }
        })
        .focus_signal(focus_signal)
        .s(Background::new().color_signal(background_color_signal))
        .s(Font::new()
            .size_signal(font_size_signal)
            .color_signal(font_color_signal))
        .s(Padding::new()
            .top_signal(padding_top_signal)
            .right_signal(padding_right_signal)
            .bottom_signal(padding_bottom_signal)
            .left_signal(padding_left_signal))
        .s(Width::with_signal_self(width_typed_signal))
        .s(Visible::with_signal(visible_sig))
        .update_raw_el(move |raw_el| {
            apply_physical_css(
                raw_el.attr_signal("disabled", disabled_sig),
                &sv_physical,
                &ctx_physical,
            )
        })
        .after_remove(move |_| {
            if LOG_DEBUG {
                zoon::println!("[EVENT:TextInput] Element REMOVED - dropping event handlers");
            }
            drop(event_handler_loop);
            drop(focus_loop);
        })
}

fn element_checkbox(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let sv_physical = tagged_object.expect_variable("settings");
    let ctx_physical = construct_context.clone();

    // TimestampedEvent captures Lamport time at DOM callback for consistent ordering
    let (click_event_sender, mut click_event_receiver) =
        NamedChannel::<TimestampedEvent<()>>::new("checkbox.click", BRIDGE_PRESS_EVENT_CAPACITY);

    let element_variable = tagged_object.expect_variable("element");

    // Use switch_map (not flat_map) because variable.stream() is infinite.
    // When element is recreated, switch_map cancels old subscription and re-subscribes to new one.
    let event_stream = switch_map(
        variable_current_or_future_stream(element_variable)
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    );

    // Get the click Variable (not just the sender) so we can access its registry_key
    let click_var_stream =
        event_stream.filter_map(|value| future::ready(value.expect_object().variable("click")));

    // Map to sender - the Variable stream already handles uniqueness via persistence_id + scope
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    let click_sender_stream =
        click_var_stream.map(move |variable| variable.expect_link_value_sender());

    let mut click_sender_stream = click_sender_stream.chain(stream::pending()).fuse();

    let event_handler_loop = ActorLoop::new({
        let construct_context_for_events = construct_context.clone();
        async move {
            let mut click_link_value_sender: Option<NamedChannel<Value>> = None;
            // Buffer for clicks that arrive before LINK sender is ready
            let mut pending_clicks: usize = 0;

            loop {
                select! {
                    result = click_sender_stream.next() => {
                        if let Some(sender) = result {
                            click_link_value_sender = Some(sender.clone());

                            // Send any pending clicks that were buffered
                            for _ in 0..pending_clicks {
                                let event_value = Object::new_value(
                                    ConstructInfo::new("checkbox::click_event", None, "Checkbox click event"),
                                    construct_context_for_events.clone(),
                                    ValueIdempotencyKey::new(),
                                    [],
                                );
                                sender.send_or_drop(event_value);
                            }
                            pending_clicks = 0;
                        }
                    }
                    event = click_event_receiver.select_next_some() => {
                        if let Some(sender) = click_link_value_sender.as_ref() {
                            let event_value = Object::new_value_with_lamport_time(
                                ConstructInfo::new("checkbox::click_event", None, "Checkbox click event"),
                                construct_context_for_events.clone(),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                [],
                            );
                            sender.send_or_drop(event_value);
                        } else {
                            // Buffer the click to send when sender becomes available
                            // Note: Buffered clicks use fresh timestamps when processed (edge case)
                            pending_clicks += 1;
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");
    let sv_padding = tagged_object.expect_variable("settings");
    let sv_visible = tagged_object.expect_variable("settings");
    let visible_sig = visible_signal_from_settings(sv_visible);

    // Size support for checkbox element (reads style.size → CSS width+height)
    let sv_size = tagged_object.expect_variable("settings");
    let size_signal = signal::from_stream({
        let style_stream = switch_map(sv_size.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            })
        })
        .boxed_local()
    })
    .broadcast();
    let width_signal = size_signal.signal_cloned();
    let height_signal = size_signal.signal_cloned();

    // CRITICAL: Use switch_map (not flat_map) because variable streams are infinite.
    let checked_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("checked").stream()
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Tag(tag, _) => Some(tag.tag() == "True"),
            _ => None,
        })
    });

    // CRITICAL: Use switch_map (not flat_map) because icon variable stream is infinite.
    let icon_stream = switch_map(settings_variable.stream(), |value| {
        value.expect_object().expect_variable("icon").stream()
    })
    .map(move |value| value_to_element(value, construct_context.clone()));

    // Padding support for checkbox element
    let padding_tuple_signal = signal::from_stream({
        let style_stream = switch_map(sv_padding.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("padding") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => {
                    let all = n.number() as u32;
                    Some((all, all, all, all))
                }
                Value::Object(obj, _) => {
                    let top = if let Some(v) = obj.variable("top") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("column") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let right = if let Some(v) = obj.variable("right") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("row") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let bottom = if let Some(v) = obj.variable("bottom") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("column") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let left = if let Some(v) = obj.variable("left") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else if let Some(v) = obj.variable("row") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number() as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    Some((top, right, bottom, left))
                }
                _ => None,
            }
        })
        .boxed_local()
    })
    .broadcast();
    let padding_top_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(t, _, _, _)| t));
    let padding_right_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, r, _, _)| r));
    let padding_bottom_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, _, b, _)| b));
    let padding_left_signal = padding_tuple_signal.signal_ref(|opt| opt.map(|(_, _, _, l)| l));

    Checkbox::new()
        .label_hidden("checkbox")
        .checked_signal(signal::from_stream(checked_stream).map(|c| c.unwrap_or(false)))
        .icon(move |_checked_mutable| El::new().child_signal(signal::from_stream(icon_stream)))
        .s(Visible::with_signal(visible_sig))
        .s(Padding::new()
            .top_signal(padding_top_signal)
            .right_signal(padding_right_signal)
            .bottom_signal(padding_bottom_signal)
            .left_signal(padding_left_signal))
        .update_raw_el(|raw_el| {
            raw_el
                .style_signal("width", width_signal)
                .style_signal("height", height_signal)
        })
        .on_click({
            let sender = click_event_sender.clone();
            move || {
                // Capture Lamport time NOW at DOM callback, before channel
                sender.send_or_drop(TimestampedEvent::now(()));
            }
        })
        .update_raw_el(move |raw_el| apply_physical_css(raw_el, &sv_physical, &ctx_physical))
        .after_remove(move |_| drop(event_handler_loop))
}

fn element_slider(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let sv_physical = tagged_object.expect_variable("settings");
    let ctx_physical = construct_context.clone();

    // Channel capacity constant - slider "input" events fire frequently during drag
    const SLIDER_INPUT_CAPACITY: usize = 64;

    let (input_event_sender, mut input_event_receiver) =
        NamedChannel::<TimestampedEvent<f64>>::new("slider.input", SLIDER_INPUT_CAPACITY);

    let element_variable = tagged_object.expect_variable("element");

    // Use switch_map (not flat_map) because variable.stream() is infinite
    let event_stream = switch_map(
        variable_current_or_future_stream(element_variable)
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    );

    let change_var_stream =
        event_stream.filter_map(|value| future::ready(value.expect_object().variable("change")));

    let change_sender_stream =
        change_var_stream.map(move |variable| variable.expect_link_value_sender());

    let mut change_sender_stream = change_sender_stream.chain(stream::pending()).fuse();

    let event_handler_loop = ActorLoop::new({
        let _construct_context = construct_context.clone();
        async move {
            let scope_id = construct_context
                .bridge_scope_id
                .expect("Bug: bridge_scope_id not set for slider event handler");
            let mut change_link_value_sender: Option<NamedChannel<Value>> = None;

            loop {
                select! {
                    result = change_sender_stream.next() => {
                        if let Some(sender) = result {
                            change_link_value_sender = Some(sender);
                        }
                    }
                    event = input_event_receiver.select_next_some() => {
                        if let Some(sender) = change_link_value_sender.as_ref() {
                            let number_value = super::engine::Number::new_value(
                                ConstructInfo::new("slider::change_number", None, "Slider change number"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                event.data,
                            );
                            let event_value = Object::new_value_with_lamport_time(
                                ConstructInfo::new("slider::change_event", None, "Slider change event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                [Variable::new_arc(
                                    ConstructInfo::new("slider::change_value", None, "Slider change value"),
                                    construct_context.clone(),
                                    "value",
                                    create_actor(
                                        ConstructInfo::new("slider::change_value_actor", None, "Slider change value actor"),
                                        ActorContext::default(),
                                        TypedStream::infinite(
                                            stream::once(future::ready(number_value)).chain(stream::pending()),
                                        ),
                                        parser::PersistenceId::new(),
                                        scope_id,
                                    ),
                                    parser::PersistenceId::default(),
                                    parser::Scope::Root,
                                )],
                            );
                            sender.send_or_drop(event_value);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    // Reactive value signal for the slider's current position
    let value_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("value").stream()
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Number(n, _) => Some(n.number().to_string()),
            _ => None,
        })
    });
    let value_signal = signal::from_stream(value_stream);

    // Reactive min signal
    let min_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("min").stream()
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Number(n, _) => Some(n.number().to_string()),
            _ => None,
        })
    });
    let min_signal = signal::from_stream(min_stream);

    // Reactive max signal
    let max_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("max").stream()
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Number(n, _) => Some(n.number().to_string()),
            _ => None,
        })
    });
    let max_signal = signal::from_stream(max_stream);

    // Reactive step signal
    let step_stream = switch_map(settings_variable.stream(), |value| {
        value.expect_object().expect_variable("step").stream()
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Number(n, _) => Some(n.number().to_string()),
            _ => None,
        })
    });
    let step_signal = signal::from_stream(step_stream);

    // Width signal from style
    let sv_width = tagged_object.expect_variable("settings");
    let width_signal = signal::from_stream({
        let style_stream = switch_map(sv_width.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("width") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                Value::Tag(tag, _) if tag.tag() == "Fill" => Some("100%".to_string()),
                _ => None,
            })
        })
        .boxed_local()
    });

    apply_physical_css(
        RawHtmlEl::new("input")
            .attr("type", "range")
            .attr_signal("value", value_signal)
            .attr_signal("min", min_signal)
            .attr_signal("max", max_signal)
            .attr_signal("step", step_signal)
            .style_signal("width", width_signal)
            .event_handler(move |event: events::Input| {
                let target: web_sys::HtmlInputElement = event.target().unwrap().unchecked_into();
                if let Ok(value) = target.value().parse::<f64>() {
                    input_event_sender.send_or_drop(TimestampedEvent::now(value));
                }
            })
            .after_remove(move |_| drop(event_handler_loop)),
        &sv_physical,
        &ctx_physical,
    )
}

fn element_select(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let sv_physical = tagged_object.expect_variable("settings");
    let ctx_physical = construct_context.clone();
    let disabled_sig = disabled_signal_from_settings(tagged_object.expect_variable("settings"));

    let (change_event_sender, mut change_event_receiver) =
        NamedChannel::<TimestampedEvent<String>>::new("select.change", BRIDGE_TEXT_CHANGE_CAPACITY);

    let element_variable = tagged_object.expect_variable("element");

    // Use switch_map (not flat_map) because variable.stream() is infinite
    let event_stream = switch_map(
        variable_current_or_future_stream(element_variable)
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    );

    let change_var_stream =
        event_stream.filter_map(|value| future::ready(value.expect_object().variable("change")));

    let change_sender_stream =
        change_var_stream.map(move |variable| variable.expect_link_value_sender());

    let mut change_sender_stream = change_sender_stream.chain(stream::pending()).fuse();

    let event_handler_loop = ActorLoop::new({
        let _construct_context = construct_context.clone();
        async move {
            let scope_id = construct_context
                .bridge_scope_id
                .expect("Bug: bridge_scope_id not set for select event handler");
            let mut change_link_value_sender: Option<NamedChannel<Value>> = None;

            loop {
                select! {
                    result = change_sender_stream.next() => {
                        if let Some(sender) = result {
                            change_link_value_sender = Some(sender);
                        }
                    }
                    event = change_event_receiver.select_next_some() => {
                        if let Some(sender) = change_link_value_sender.as_ref() {
                            let text_value = super::engine::Text::new_value(
                                ConstructInfo::new("select::change_text", None, "Select change text"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                event.data.clone(),
                            );
                            let event_value = Object::new_value_with_lamport_time(
                                ConstructInfo::new("select::change_event", None, "Select change event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                [Variable::new_arc(
                                    ConstructInfo::new("select::change_value", None, "Select change value"),
                                    construct_context.clone(),
                                    "value",
                                    create_actor(
                                        ConstructInfo::new("select::change_value_actor", None, "Select change value actor"),
                                        ActorContext::default(),
                                        TypedStream::infinite(
                                            stream::once(future::ready(text_value)).chain(stream::pending()),
                                        ),
                                        parser::PersistenceId::new(),
                                        scope_id,
                                    ),
                                    parser::PersistenceId::default(),
                                    parser::Scope::Root,
                                )],
                            );
                            sender.send_or_drop(event_value);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    // Reactive selected value signal
    let selected_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("selected").stream()
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Text(t, _) => Some(t.text().to_string()),
            _ => None,
        })
    });
    let selected_signal = signal::from_stream(selected_stream);

    // Reactive options signal — extract list of (value, label) pairs
    // Options arrive as a List of Objects with "value" and "label" variables
    let options_stream = switch_map(settings_variable.stream(), |value| {
        value.expect_object().expect_variable("options").stream()
    })
    .then(move |value| async move {
        match value {
            Value::List(list, _) => {
                let snapshot = list.snapshot().await;
                let mut pairs = Vec::new();
                for (_item_id, actor) in snapshot {
                    if let Ok(Value::Object(obj, _)) = actor.current_value().await {
                        let val = if let Some(var) = obj.variable("value") {
                            if let Ok(Value::Text(t, _)) = var.value_actor().current_value().await {
                                t.text().to_string()
                            } else {
                                String::new()
                            }
                        } else {
                            String::new()
                        };
                        let label = if let Some(var) = obj.variable("label") {
                            if let Ok(Value::Text(t, _)) = var.value_actor().current_value().await {
                                t.text().to_string()
                            } else {
                                val.clone()
                            }
                        } else {
                            val.clone()
                        };
                        pairs.push((val, label));
                    }
                }
                pairs
            }
            _ => Vec::new(),
        }
    });
    let options_signal = signal::from_stream(options_stream.boxed_local());

    // Width signal from style
    let sv_width = tagged_object.expect_variable("settings");
    let width_signal = signal::from_stream({
        let style_stream = switch_map(sv_width.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("width") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                Value::Tag(tag, _) if tag.tag() == "Fill" => Some("100%".to_string()),
                _ => None,
            })
        })
        .boxed_local()
    });

    // Build the <select> element with reactive options and selected value
    // inner_markup_signal rebuilds <option> HTML whenever the options list changes
    apply_physical_css(
        RawHtmlEl::new("select")
            .style_signal("width", width_signal)
            .attr_signal("value", selected_signal)
            .attr_signal("disabled", disabled_sig)
            .inner_markup_signal(options_signal.map(|opt| {
                let pairs = opt.unwrap_or_default();
                let mut html = String::new();
                for (val, label) in pairs {
                    html.push_str(&format!("<option value=\"{}\">{}</option>", val, label));
                }
                html
            }))
            .event_handler(move |event: events::Input| {
                let target: web_sys::HtmlInputElement = event.target().unwrap().unchecked_into();
                let value = target.value();
                change_event_sender.send_or_drop(TimestampedEvent::now(value));
            })
            .after_remove(move |_| drop(event_handler_loop)),
        &sv_physical,
        &ctx_physical,
    )
}

fn element_svg(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let settings_variable = tagged_object.expect_variable("settings");

    // Click event with coordinates
    let (click_sender, mut click_receiver) =
        NamedChannel::<TimestampedEvent<(f64, f64)>>::new("svg.click", BRIDGE_PRESS_EVENT_CAPACITY);

    let element_variable = tagged_object.expect_variable("element");
    let mut click_stream = switch_map(
        variable_current_or_future_stream(element_variable.clone())
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    )
    .filter_map(|value| future::ready(value.expect_object().variable("click")))
    .map(move |variable| variable.expect_link_value_sender())
    .chain(stream::pending())
    .fuse();

    let event_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let scope_id = construct_context
                .bridge_scope_id
                .expect("Bug: bridge_scope_id not set for svg event handler");
            let mut click_link_sender: Option<NamedChannel<Value>> = None;
            loop {
                select! {
                    link = click_stream.next() => {
                        if let Some(link) = link {
                            click_link_sender = Some(link);
                        }
                    }
                    event = click_receiver.next() => {
                        if let Some(event) = event {
                            if let Some(ref sender) = click_link_sender {
                                let (x, y) = event.data;
                                let x_value = Value::Number(
                                    EngineNumber::new_arc(
                                        ConstructInfo::new("svg::click_x_num", None, "svg click x number"),
                                        construct_context.clone(),
                                        x,
                                    ),
                                    ValueMetadata::with_lamport_time(ValueIdempotencyKey::new(), event.lamport_time),
                                );
                                let y_value = Value::Number(
                                    EngineNumber::new_arc(
                                        ConstructInfo::new("svg::click_y_num", None, "svg click y number"),
                                        construct_context.clone(),
                                        y,
                                    ),
                                    ValueMetadata::with_lamport_time(ValueIdempotencyKey::new(), event.lamport_time),
                                );
                                let event_value = Object::new_value_with_lamport_time(
                                    ConstructInfo::new("svg::click_event", None, "svg click event [x, y]"),
                                    construct_context.clone(),
                                    ValueIdempotencyKey::new(),
                                    event.lamport_time,
                                    [
                                        Variable::new_arc(
                                            ConstructInfo::new("svg::click_x", None, "svg click x"),
                                            construct_context.clone(),
                                            "x",
                                            create_actor(
                                                ConstructInfo::new("svg::click_x_actor", None, "svg click x actor"),
                                                ActorContext::default(),
                                                TypedStream::infinite(
                                                    stream::once(future::ready(x_value)).chain(stream::pending()),
                                                ),
                                                parser::PersistenceId::new(),
                                                scope_id,
                                            ),
                                            parser::PersistenceId::default(),
                                            parser::Scope::Root,
                                        ),
                                        Variable::new_arc(
                                            ConstructInfo::new("svg::click_y", None, "svg click y"),
                                            construct_context.clone(),
                                            "y",
                                            create_actor(
                                                ConstructInfo::new("svg::click_y_actor", None, "svg click y actor"),
                                                ActorContext::default(),
                                                TypedStream::infinite(
                                                    stream::once(future::ready(y_value)).chain(stream::pending()),
                                                ),
                                                parser::PersistenceId::new(),
                                                scope_id,
                                            ),
                                            parser::PersistenceId::default(),
                                            parser::Scope::Root,
                                        ),
                                    ],
                                );
                                sender.send_or_drop(event_value);
                            }
                        }
                    }
                }
            }
        }
    });

    // Width/height signals from style
    let width_signal = signal::from_stream({
        let style_stream = switch_map(settings_variable.clone().stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("width") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number().to_string()),
                _ => None,
            })
        })
    });

    let height_signal = signal::from_stream({
        let style_stream = switch_map(settings_variable.clone().stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("height") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number().to_string()),
                _ => None,
            })
        })
    });

    // Background style (CSS background on SVG element)
    let background_signal = signal::from_stream({
        let style_stream = switch_map(settings_variable.clone().stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("background") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Text(t, _) => Some(t.text().to_string()),
                _ => None,
            })
        })
    });

    // Children — render list items as SVG child elements
    let children_vec_diff_stream = switch_map(
        switch_map(settings_variable.stream(), |value| {
            value.expect_object().expect_variable("children").stream()
        }),
        |value| list_change_to_vec_diff_stream(value.expect_list().stream()),
    );

    RawSvgEl::new("svg")
        .attr_signal("width", width_signal)
        .attr_signal("height", height_signal)
        .style("overflow", "visible")
        .style_signal("background", background_signal)
        .event_handler(move |event: events::Click| {
            let x = f64::from(event.offset_x());
            let y = f64::from(event.offset_y());
            click_sender.send_or_drop(TimestampedEvent::now((x, y)));
        })
        .children_signal_vec(VecDiffStreamSignalVec(children_vec_diff_stream).map_signal(
            move |value_actor| {
                signal::from_stream(value_actor.stream().map({
                    let construct_context = construct_context.clone();
                    move |value| value_to_element(value, construct_context.clone())
                }))
            },
        ))
        .after_remove(move |_| drop(event_handler_loop))
}

fn element_svg_circle(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let settings_variable = tagged_object.expect_variable("settings");

    // Click event with coordinates
    let (click_sender, mut click_receiver) = NamedChannel::<TimestampedEvent<(f64, f64)>>::new(
        "svg_circle.click",
        BRIDGE_PRESS_EVENT_CAPACITY,
    );

    let element_variable = tagged_object.expect_variable("element");
    let mut click_stream = switch_map(
        variable_current_or_future_stream(element_variable.clone())
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable_current_or_future_stream(variable),
    )
    .filter_map(|value| future::ready(value.expect_object().variable("click")))
    .map(move |variable| variable.expect_link_value_sender())
    .chain(stream::pending())
    .fuse();

    let event_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let scope_id = construct_context
                .bridge_scope_id
                .expect("Bug: bridge_scope_id not set for svg_circle event handler");
            let mut click_link_sender: Option<NamedChannel<Value>> = None;
            loop {
                select! {
                    link = click_stream.next() => {
                        if let Some(link) = link {
                            click_link_sender = Some(link);
                        }
                    }
                    event = click_receiver.next() => {
                        if let Some(event) = event {
                            if let Some(ref sender) = click_link_sender {
                                let (x, y) = event.data;
                                let x_value = Value::Number(
                                    EngineNumber::new_arc(
                                        ConstructInfo::new("svg_circle::click_x_num", None, "svg_circle click x number"),
                                        construct_context.clone(),
                                        x,
                                    ),
                                    ValueMetadata::with_lamport_time(ValueIdempotencyKey::new(), event.lamport_time),
                                );
                                let y_value = Value::Number(
                                    EngineNumber::new_arc(
                                        ConstructInfo::new("svg_circle::click_y_num", None, "svg_circle click y number"),
                                        construct_context.clone(),
                                        y,
                                    ),
                                    ValueMetadata::with_lamport_time(ValueIdempotencyKey::new(), event.lamport_time),
                                );
                                let event_value = Object::new_value_with_lamport_time(
                                    ConstructInfo::new("svg_circle::click_event", None, "svg_circle click event [x, y]"),
                                    construct_context.clone(),
                                    ValueIdempotencyKey::new(),
                                    event.lamport_time,
                                    [
                                        Variable::new_arc(
                                            ConstructInfo::new("svg_circle::click_x", None, "svg_circle click x"),
                                            construct_context.clone(),
                                            "x",
                                            create_actor(
                                                ConstructInfo::new("svg_circle::click_x_actor", None, "svg_circle click x actor"),
                                                ActorContext::default(),
                                                TypedStream::infinite(
                                                    stream::once(future::ready(x_value)).chain(stream::pending()),
                                                ),
                                                parser::PersistenceId::new(),
                                                scope_id,
                                            ),
                                            parser::PersistenceId::default(),
                                            parser::Scope::Root,
                                        ),
                                        Variable::new_arc(
                                            ConstructInfo::new("svg_circle::click_y", None, "svg_circle click y"),
                                            construct_context.clone(),
                                            "y",
                                            create_actor(
                                                ConstructInfo::new("svg_circle::click_y_actor", None, "svg_circle click y actor"),
                                                ActorContext::default(),
                                                TypedStream::infinite(
                                                    stream::once(future::ready(y_value)).chain(stream::pending()),
                                                ),
                                                parser::PersistenceId::new(),
                                                scope_id,
                                            ),
                                            parser::PersistenceId::default(),
                                            parser::Scope::Root,
                                        ),
                                    ],
                                );
                                sender.send_or_drop(event_value);
                            }
                        }
                    }
                }
            }
        }
    });

    // Reactive SVG attributes: cx, cy, r
    let cx_signal = signal::from_stream(
        switch_map(settings_variable.clone().stream(), |value| {
            value.expect_object().expect_variable("cx").stream()
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number().to_string()),
                _ => None,
            })
        }),
    );

    let cy_signal = signal::from_stream(
        switch_map(settings_variable.clone().stream(), |value| {
            value.expect_object().expect_variable("cy").stream()
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number().to_string()),
                _ => None,
            })
        }),
    );

    let r_signal = signal::from_stream(
        switch_map(settings_variable.clone().stream(), |value| {
            value.expect_object().expect_variable("r").stream()
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number().to_string()),
                _ => None,
            })
        }),
    );

    // Style: fill, stroke, stroke_width
    let fill_signal = signal::from_stream({
        let style_stream = switch_map(settings_variable.clone().stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("fill") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Text(t, _) => Some(t.text().to_string()),
                _ => None,
            })
        })
    });

    let stroke_signal = signal::from_stream({
        let style_stream = switch_map(settings_variable.clone().stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("stroke") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Text(t, _) => Some(t.text().to_string()),
                _ => None,
            })
        })
    });

    let stroke_width_signal = signal::from_stream({
        let style_stream = switch_map(settings_variable.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("stroke_width") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number().to_string()),
                _ => None,
            })
        })
    });

    RawSvgEl::new("circle")
        .attr_signal("cx", cx_signal)
        .attr_signal("cy", cy_signal)
        .attr_signal("r", r_signal)
        .attr_signal("fill", fill_signal)
        .attr_signal("stroke", stroke_signal)
        .attr_signal("stroke-width", stroke_width_signal)
        .style("cursor", "pointer")
        .event_handler(move |event: events::Click| {
            let x = f64::from(event.offset_x());
            let y = f64::from(event.offset_y());
            click_sender.send_or_drop(TimestampedEvent::now((x, y)));
        })
        .after_remove(move |_| drop(event_handler_loop))
}

fn element_label(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let sv_physical = tagged_object.expect_variable("settings");
    let ctx_physical = construct_context.clone();

    // TimestampedEvent captures Lamport time at DOM callback for consistent ordering
    let (double_click_sender, mut double_click_receiver) =
        NamedChannel::<TimestampedEvent<()>>::new(
            "double_click.event",
            BRIDGE_PRESS_EVENT_CAPACITY,
        );
    let (_hovered_sender, _hovered_receiver) =
        NamedChannel::<TimestampedEvent<bool>>::new("double_click.hovered", BRIDGE_HOVER_CAPACITY);

    let element_variable = tagged_object.expect_variable("element");

    // Set up hovered link
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    let hovered_stream = element_variable.clone();
    let hovered_stream = variable_current_or_future_stream(hovered_stream)
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending());

    // Set up double_click event
    // Use switch_map (not flat_map) because variable.stream() is infinite.
    // When element is recreated, switch_map cancels old subscription and re-subscribes to new one.
    let event_stream = switch_map(
        variable_current_or_future_stream(element_variable).filter_map(move |value| {
            let obj = value.expect_object();
            future::ready(obj.variable("event"))
        }),
        move |variable| variable_current_or_future_stream(variable),
    );

    // Chain with pending() to prevent stream termination causing busy-polling in select!
    let mut double_click_stream = event_stream
        .filter_map(move |value| {
            let obj = value.expect_object();
            future::ready(obj.variable("double_click"))
        })
        .map(move |variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    let event_handler_loop = ActorLoop::new({
        let construct_context_for_events = construct_context.clone();
        async move {
            let mut double_click_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut _hovered_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut hovered_stream = hovered_stream.fuse();
            loop {
                select! {
                    new_sender = double_click_stream.next() => {
                        if let Some(sender) = new_sender {
                            double_click_link_value_sender = Some(sender);
                        }
                    }
                    new_sender = hovered_stream.next() => {
                        if let Some(sender) = new_sender {
                            // Send initial hover state (false) when link is established
                            let initial_hover_value = EngineTag::new_value_cached(
                                HOVER_TAG_INFO.with(|info| info.clone()),
                                ValueIdempotencyKey::new(),
                                "False",
                            );
                            sender.send_or_drop(initial_hover_value);
                            _hovered_link_value_sender = Some(sender);
                        }
                    }
                    event = double_click_receiver.select_next_some() => {
                        if let Some(sender) = double_click_link_value_sender.as_ref() {
                            let event_value = Object::new_value_with_lamport_time(
                                ConstructInfo::new("label::double_click_event", None, "Label double_click event"),
                                construct_context_for_events.clone(),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                [],
                            );
                            sender.send_or_drop(event_value);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    // CRITICAL: Use switch_map (not flat_map) because label variable stream is infinite.
    // When settings change, switch_map cancels the old label subscription.
    let label_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("label").stream()
    })
    .map({
        let _construct_context = construct_context.clone();
        move |value| value_to_element(value, construct_context.clone())
    });

    // Create style streams
    let sv_visible = tagged_object.expect_variable("settings");
    let visible_sig = visible_signal_from_settings(sv_visible);
    let sv2 = tagged_object.expect_variable("settings");
    let sv3 = tagged_object.expect_variable("settings");
    let sv4 = tagged_object.expect_variable("settings");
    let sv5 = tagged_object.expect_variable("settings");

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // Produces u32 for Padding::all_signal (uniform padding from simple number)
    let padding_all_signal = signal::from_stream({
        let style_stream = switch_map(sv2.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("padding") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
    });

    // Font weight - produces FontWeight typed values
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_weight = tagged_object.expect_variable("settings");
    let font_weight_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_weight.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("weight") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => match tag.tag() {
                    "Hairline" => Some(FontWeight::Hairline),
                    "ExtraLight" | "UltraLight" => Some(FontWeight::ExtraLight),
                    "Light" => Some(FontWeight::Light),
                    "Regular" | "Normal" => Some(FontWeight::Regular),
                    "Medium" => Some(FontWeight::Medium),
                    "SemiBold" | "DemiBold" => Some(FontWeight::SemiBold),
                    "Bold" => Some(FontWeight::Bold),
                    "ExtraBold" | "UltraBold" => Some(FontWeight::ExtraBold),
                    "Black" | "Heavy" => Some(FontWeight::Heavy),
                    "ExtraHeavy" => Some(FontWeight::ExtraHeavy),
                    _ => None,
                },
                Value::Number(n, _) => Some(FontWeight::Number(n.number() as u32)),
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let font_color_signal = signal::from_stream({
        let style_stream = switch_map(sv3.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(font_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(sv4.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
    });

    // Strikethrough signal (font.line.strikethrough) - produces bool for FontLine::strike_signal()
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let strikethrough_bool_signal = signal::from_stream({
        let style_stream = switch_map(sv5.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        let line_stream = switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("line") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(line_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("strikethrough") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => match tag.tag() {
                    "True" => Some(true),
                    "False" => Some(false),
                    _ => None,
                },
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Width signal (supports Fill or exact number)
    let sv_width = tagged_object.expect_variable("settings");
    let width_typed_signal = signal::from_stream({
        let style_stream = switch_map(sv_width.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("width") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = match value {
                Value::Number(n, _) => Some(Width::exact(n.number() as u32)),
                Value::Tag(tag, _) if tag.tag() == "Fill" => Some(Width::fill()),
                _ => None,
            };
            future::ready(result)
        })
    });

    Label::new()
        .s(Width::with_signal_self(width_typed_signal))
        .s(Font::new()
            .size_signal(font_size_signal)
            .color_signal(font_color_signal)
            .weight_signal(font_weight_signal)
            .line(
                FontLine::new()
                    .strike_signal(strikethrough_bool_signal.map(|opt| opt.unwrap_or(false))),
            ))
        .s(Padding::all_signal(padding_all_signal))
        .label_signal(signal::from_stream(label_stream).map(|l| l.unwrap_or_else(empty_text)))
        .on_double_click({
            let sender = double_click_sender.clone();
            move || {
                // Capture Lamport time NOW at DOM callback, before channel
                sender.send_or_drop(TimestampedEvent::now(()));
            }
        })
        .s(Visible::with_signal(visible_sig))
        .update_raw_el(move |raw_el| apply_physical_css(raw_el, &sv_physical, &ctx_physical))
        .after_remove(move |_| drop(event_handler_loop))
}

fn element_paragraph(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let settings_variable = tagged_object.expect_variable("settings");
    let sv_physical = settings_variable.clone();
    let ctx_physical = construct_context.clone();
    let sv_visible = tagged_object.expect_variable("settings");
    let visible_sig = visible_signal_from_settings(sv_visible);

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let contents_stream = switch_map(settings_variable.stream(), |value| {
        value.expect_object().expect_variable("contents").stream()
    });
    let contents_vec_diff_stream = switch_map(contents_stream, |value| {
        list_change_to_vec_diff_stream(value.expect_list().stream())
    });

    Paragraph::new()
        // white-space: pre-wrap is already global in MoonZoon's basic.css
        .contents_signal_vec(VecDiffStreamSignalVec(contents_vec_diff_stream).map_signal(
            move |value_actor| {
                signal::from_stream(actor_current_or_future_stream(value_actor).map({
                    let construct_context = construct_context.clone();
                    move |value| value_to_element(value, construct_context.clone())
                }))
            },
        ))
        .s(Visible::with_signal(visible_sig))
        .update_raw_el(move |raw_el| apply_physical_css(raw_el, &sv_physical, &ctx_physical))
}

fn element_link(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let sv_physical = tagged_object.expect_variable("settings");
    let ctx_physical = construct_context.clone();

    // TimestampedEvent captures Lamport time at DOM callback for consistent ordering
    let (hovered_sender, mut hovered_receiver) =
        NamedChannel::<TimestampedEvent<bool>>::new("link.hovered", BRIDGE_HOVER_CAPACITY);

    let element_variable = tagged_object.expect_variable("element");

    // Set up hovered handler
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    let mut hovered_stream = variable_current_or_future_stream(element_variable)
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    let event_handler_loop = ActorLoop::new({
        let _construct_context = construct_context.clone();
        async move {
            let mut hovered_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut last_hover_state: Option<bool> = None;

            loop {
                select! {
                    sender = hovered_stream.select_next_some() => {
                        // Send initial hover state (false) when link is established
                        let initial_hover_value = EngineTag::new_value_cached(
                            HOVER_TAG_INFO.with(|info| info.clone()),
                            ValueIdempotencyKey::new(),
                            "False",
                        );
                        sender.send_or_drop(initial_hover_value);
                        last_hover_state = Some(false);
                        hovered_link_value_sender = Some(sender);
                    }
                    event = hovered_receiver.select_next_some() => {
                        if last_hover_state == Some(event.data) {
                            inc_metric!(HOVER_EVENTS_DEDUPED);
                            continue;
                        }
                        if let Some(sender) = hovered_link_value_sender.as_ref() {
                            inc_metric!(HOVER_EVENTS_EMITTED);
                            last_hover_state = Some(event.data);
                            let hover_tag = if event.data { "True" } else { "False" };
                            let event_value = EngineTag::new_value_cached_with_lamport_time(
                                HOVER_TAG_INFO.with(|info| info.clone()),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                hover_tag,
                            );
                            sender.send_or_drop(event_value);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");
    let sv_visible = tagged_object.expect_variable("settings");
    let visible_sig = visible_signal_from_settings(sv_visible);

    // CRITICAL: Use switch_map (not flat_map) because label variable stream is infinite.
    // When settings change, switch_map cancels the old label subscription.
    let label_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("label").stream()
    })
    .map({
        let construct_context = construct_context.clone();
        move |value| value_to_element(value, construct_context.clone())
    });

    let sv_to = settings_variable.clone();
    // CRITICAL: Use switch_map (not flat_map) because 'to' variable stream is infinite.
    let to_stream = switch_map(sv_to.stream(), |value| {
        value.expect_object().expect_variable("to").stream()
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Text(text, _) => Some(text.text().to_string()),
            _ => None,
        })
    });

    // Underline signal (font.line.underline) - produces bool for FontLine::underline_signal()
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_underline = settings_variable.clone();
    let underline_bool_signal = signal::from_stream({
        let style_stream = switch_map(sv_underline.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        let line_stream = switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("line") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(line_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("underline") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => match tag.tag() {
                    "True" => Some(true),
                    "False" => Some(false),
                    _ => None,
                },
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    Link::new()
        .label_signal(signal::from_stream(label_stream).map(|l| l.unwrap_or_else(empty_text)))
        .to_signal(signal::from_stream(to_stream).map(|t| t.unwrap_or_default()))
        .new_tab(NewTab::new())
        .on_hovered_change(move |is_hovered| {
            // Capture Lamport time NOW at DOM callback, before channel
            hovered_sender.send_or_drop(TimestampedEvent::now(is_hovered));
        })
        .s(Font::new().line(
            FontLine::new().underline_signal(underline_bool_signal.map(|opt| opt.unwrap_or(false))),
        ))
        .s(Visible::with_signal(visible_sig))
        .update_raw_el(move |raw_el| apply_physical_css(raw_el, &sv_physical, &ctx_physical))
        .after_remove(move |_| {
            drop(event_handler_loop);
            drop(tagged_object);
        })
}

/// Element/text - renders styled text content.
/// Structure: ElementText[element?, settings[style, text]]
fn element_text(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let sv_physical = tagged_object.expect_variable("settings");
    let ctx_physical = construct_context.clone();
    let settings_variable = tagged_object.expect_variable("settings");

    // Extract text stream from settings
    let text_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("text").stream()
    })
    .filter_map(|value| {
        future::ready(match value {
            Value::Text(text, _) => Some(text.text().to_string()),
            Value::Number(n, _) => Some(n.number().to_string()),
            _ => None,
        })
    });

    // Extract font.size from style
    let sv_font_size = tagged_object.expect_variable("settings");
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_size.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(font_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(n.number() as u32),
                _ => None,
            })
        })
        .boxed_local()
    });

    // Extract font.color from style
    // CRITICAL: Use oklch_to_css_stream (not filter_map for Text only) because
    // physical themes use Oklch[...] tagged objects for colors.
    let sv_font_color = tagged_object.expect_variable("settings");
    let font_color_signal = signal::from_stream({
        let style_stream = switch_map(sv_font_color.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let font_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("font") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(
            switch_map(font_stream, |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }),
            |value| oklch_to_css_stream(value),
        )
        .boxed_local()
    });

    El::new()
        .s(Font::new().size_signal(font_size_signal.map(|opt| opt.unwrap_or(16))))
        .update_raw_el(|raw_el| {
            // Use Option<String> so dominator removes the property on None
            // instead of panicking on empty string
            raw_el.style_signal("color", font_color_signal)
        })
        .child_signal(signal::from_stream(text_stream))
        .update_raw_el(move |raw_el| apply_physical_css(raw_el, &sv_physical, &ctx_physical))
        .after_remove(move |_| {
            drop(tagged_object);
        })
}

/// Element/block - renders a styled block element with a single child.
/// Structure: ElementBlock[element?, settings[style, child]]
fn element_block(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let settings_variable = tagged_object.expect_variable("settings");
    let sv_physical = settings_variable.clone();
    let ctx_physical = construct_context.clone();

    // Extract child stream from settings
    let child_stream = switch_map(settings_variable.clone().stream(), |value| {
        value.expect_object().expect_variable("child").stream()
    })
    .map({
        let construct_context = construct_context.clone();
        move |value| value_to_element(value, construct_context.clone())
    });

    // Reactive width signal: subscribes to style.width or style.size
    let sv_width = tagged_object.expect_variable("settings");
    let width_signal = signal::from_stream({
        let style_stream = switch_map(sv_width.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            if let Some(var) = obj.variable("width") {
                var.stream().boxed_local()
            } else if let Some(var) = obj.variable("size") {
                var.stream().boxed_local()
            } else {
                stream::empty().boxed_local()
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Tag(tag, _) if tag.tag() == "Fill" => Some("100%".to_owned()),
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            })
        })
        .boxed_local()
    });

    // Reactive height signal: subscribes to style.height or style.size
    let sv_height = tagged_object.expect_variable("settings");
    let height_signal = signal::from_stream({
        let style_stream = switch_map(sv_height.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            if let Some(var) = obj.variable("height") {
                var.stream().boxed_local()
            } else if let Some(var) = obj.variable("size") {
                var.stream().boxed_local()
            } else {
                stream::empty().boxed_local()
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Tag(tag, _) if tag.tag() == "Fill" => Some("100%".to_owned()),
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            })
        })
        .boxed_local()
    });

    // Extract style as a CSS string via raw_el for simplicity.
    // This covers padding and align from the style object.
    // Width and height are handled by separate reactive signals above.
    let sv_style = tagged_object.expect_variable("settings");
    let style_css_signal = signal::from_stream({
        let style_stream = switch_map(sv_style.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        style_stream
            .filter_map(|value| async move {
                let obj = match value {
                    Value::Object(obj, _) => obj,
                    _ => return None,
                };
                let mut css = String::new();

                // Padding
                if let Some(v) = obj.variable("padding") {
                    match v.value_actor().current_value().await {
                        Ok(Value::Number(n, _)) => {
                            css.push_str(&format!("padding:{}px;", n.number()));
                        }
                        Ok(Value::Object(pad_obj, _)) => {
                            let get = |obj: &Arc<Object>, key: &str, fallback: &str| {
                                let obj = obj.clone();
                                let key = key.to_string();
                                let fallback = fallback.to_string();
                                async move {
                                    if let Some(v) = obj.variable(&key) {
                                        if let Ok(Value::Number(n, _)) =
                                            v.value_actor().current_value().await
                                        {
                                            return n.number();
                                        }
                                    }
                                    if let Some(v) = obj.variable(&fallback) {
                                        if let Ok(Value::Number(n, _)) =
                                            v.value_actor().current_value().await
                                        {
                                            return n.number();
                                        }
                                    }
                                    0.0
                                }
                            };
                            let top = get(&pad_obj, "top", "column").await;
                            let right = get(&pad_obj, "right", "row").await;
                            let bottom = get(&pad_obj, "bottom", "column").await;
                            let left = get(&pad_obj, "left", "row").await;
                            css.push_str(&format!(
                                "padding:{}px {}px {}px {}px;",
                                top, right, bottom, left
                            ));
                        }
                        _ => {}
                    }
                }

                // Align
                if let Some(v) = obj.variable("align") {
                    if let Ok(Value::Object(align_obj, _)) = v.value_actor().current_value().await {
                        if let Some(row_v) = align_obj.variable("row") {
                            if let Ok(Value::Tag(tag, _)) =
                                row_v.value_actor().current_value().await
                            {
                                match tag.tag() {
                                    "Center" => css.push_str("text-align:center;"),
                                    "Right" => css.push_str("text-align:right;"),
                                    _ => {}
                                }
                            }
                        }
                    }
                }

                Some(css)
            })
            .boxed_local()
    });

    // Reactive background-image from style.background.url
    let sv_bg = settings_variable.clone();
    let bg_image_signal = signal::from_stream({
        let style_stream = switch_map(sv_bg.stream(), |value| {
            value.expect_object().expect_variable("style").stream()
        });
        let bg_stream = switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("background") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        });
        switch_map(bg_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("url") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Text(text, _) => Some(format!("url({})", text.text())),
                _ => None,
            })
        })
        .boxed_local()
    });

    El::new()
        .update_raw_el(|raw_el| {
            raw_el.update_dom_builder(|dom_builder| {
                let element: web_sys::Element = dom_builder.__internal_element().into();
                dom_builder.future(style_css_signal.for_each(move |css_opt| {
                    let style = element.unchecked_ref::<web_sys::HtmlElement>().style();
                    async move {
                        let css = css_opt.unwrap_or_default();
                        for pair in css.split(';') {
                            let pair = pair.trim();
                            if pair.is_empty() {
                                continue;
                            }
                            if let Some((name, value)) = pair.split_once(':') {
                                let _ = style.set_property(name.trim(), value.trim());
                            }
                        }
                    }
                }))
            })
        })
        .update_raw_el(|raw_el| {
            raw_el
                .style_signal("width", width_signal)
                .style_signal("height", height_signal)
                .style_signal("background-image", bg_image_signal)
                .style("background-size", "contain")
                .style("background-repeat", "no-repeat")
                .style("background-position", "center")
        })
        .child_signal(signal::from_stream(child_stream))
        .update_raw_el(move |raw_el| apply_physical_css(raw_el, &sv_physical, &ctx_physical))
        .after_remove(move |_| {
            drop(tagged_object);
        })
}

#[pin_project]
#[derive(Debug)]
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

/// Converts a ListChange stream to a VecDiff stream for UI rendering.
/// Tracks the list state internally to convert identity-based Remove to index-based RemoveAt.
fn list_change_to_vec_diff_stream(
    change_stream: impl Stream<Item = ListChange>,
) -> impl Stream<Item = VecDiff<ActorHandle>> {
    use futures_signals::signal_vec::VecDiff;

    change_stream.scan(Vec::<ActorHandle>::new(), move |items, change| {
        let vec_diff = match change {
            ListChange::Replace { items: new_items } => {
                *items = new_items.to_vec();
                VecDiff::Replace {
                    values: new_items.to_vec(),
                }
            }
            ListChange::InsertAt { index, item } => {
                let insert_index = index.min(items.len());
                if insert_index != index {
                    zoon::eprintln!(
                        "[actors-bridge-clamp] InsertAt index={} len={}",
                        index,
                        items.len()
                    );
                }
                items.insert(insert_index, item.clone());
                VecDiff::InsertAt {
                    index: insert_index,
                    value: item,
                }
            }
            ListChange::UpdateAt { index, item } => {
                if index < items.len() {
                    items[index] = item.clone();
                    VecDiff::UpdateAt { index, value: item }
                } else {
                    zoon::eprintln!(
                        "[actors-bridge-fallback] UpdateAt index={} len={}",
                        index,
                        items.len()
                    );
                    VecDiff::Replace {
                        values: items.clone(),
                    }
                }
            }
            ListChange::Remove { id } => {
                // Find index by PersistenceId
                if let Some(index) = items.iter().position(|item| item.persistence_id() == id) {
                    items.remove(index);
                    VecDiff::RemoveAt { index }
                } else {
                    // Item not found - emit a no-op Replace with current items
                    // This shouldn't happen in normal operation
                    VecDiff::Replace {
                        values: items.clone(),
                    }
                }
            }
            ListChange::Move {
                old_index,
                new_index,
            } => {
                if old_index < items.len() {
                    let item = items.remove(old_index);
                    let insert_index = if new_index > old_index {
                        new_index.saturating_sub(1).min(items.len())
                    } else {
                        new_index.min(items.len())
                    };
                    items.insert(insert_index, item);
                    VecDiff::Move {
                        old_index,
                        new_index: insert_index,
                    }
                } else {
                    zoon::eprintln!(
                        "[actors-bridge-fallback] Move old_index={} new_index={} len={}",
                        old_index,
                        new_index,
                        items.len()
                    );
                    VecDiff::Replace {
                        values: items.clone(),
                    }
                }
            }
            ListChange::Push { item } => {
                items.push(item.clone());
                VecDiff::Push { value: item }
            }
            ListChange::Pop => {
                items.pop();
                VecDiff::Pop {}
            }
            ListChange::Clear => {
                items.clear();
                VecDiff::Clear {}
            }
        };
        future::ready(Some(vec_diff))
    })
}
