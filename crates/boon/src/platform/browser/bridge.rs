use std::sync::Arc;

use zoon::futures_util::{future, select, stream, StreamExt};
use zoon::futures_util::stream::LocalBoxStream;
use zoon::*;

use super::engine::{
    ActorContext, ActorLoop, ConstructContext, ConstructInfo, ListChange, NamedChannel, Object,
    TaggedObject, TimestampedEvent, TypedStream, Value, ValueActor, ValueIdempotencyKey, Variable,
    Text as EngineText, Tag as EngineTag, switch_map,
};
use crate::parser;

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
        zoon::eprintln!("[TYPE_MISMATCH] {}: expected {}, got {}", context, expected, type_name);
    }
    // In release mode, do nothing - this maintains current behavior
    #[cfg(not(debug_assertions))]
    let _ = (context, expected, got);
}

pub fn object_with_document_to_element_signal(
    root_object: Arc<Object>,
    construct_context: ConstructContext,
) -> impl Signal<Item = Option<RawElOrText>> {
    let document_variable = root_object.expect_variable("document").clone();
    let doc_actor = document_variable.value_actor();

    // CRITICAL: Use switch_map (not flat_map) because the inner stream is infinite.
    // When example is switched, the document changes and we MUST switch to the new
    // root_element stream. flat_map would stay subscribed to the old one forever.
    let element_stream = switch_map(
        doc_actor.clone().stream(),
        |value| {
            let document_object = value.expect_object();
            let root_element_var = document_object.expect_variable("root_element").clone();
            root_element_var.value_actor().clone().stream()
        }
    )
        .map(move |value| value_to_element(value, construct_context.clone()))
        .boxed_local();

    signal::from_stream(element_stream)
}

fn value_to_element(value: Value, construct_context: ConstructContext) -> RawElOrText {
    match value {
        Value::Text(text, _) => zoon::Text::new(text.text()).unify(),
        Value::Number(number, _) => zoon::Text::new(number.number()).unify(),
        Value::Tag(tag, _) => {
            // Handle special tags like NoElement
            match tag.tag() {
                "NoElement" => El::new().unify(), // Empty element
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
            "ElementLabel" => element_label(tagged_object, construct_context).unify(),
            "ElementParagraph" => element_paragraph(tagged_object, construct_context).unify(),
            "ElementLink" => element_link(tagged_object, construct_context).unify(),
            other => panic!("Element cannot be created from the tagged object with tag '{other}'"),
        },
        Value::Flushed(inner, _) => {
            // Unwrap Flushed and recursively handle the inner value
            value_to_element(*inner, construct_context)
        }
        Value::Object(obj, _) => {
            // Object can't be rendered as element - render as debug info
            zoon::eprintln!("Warning: Object value passed to element context - rendering as empty. Object has {} variables", obj.variables().len());
            El::new().unify()
        }
        Value::List(_list, _) => {
            // List can't be rendered as a single element - render as debug info
            zoon::eprintln!("Warning: List value passed to element context - rendering as empty");
            El::new().unify()
        }
    }
}

fn element_container(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let settings_variable = tagged_object.expect_variable("settings");

    // Use switch_map (not flat_map) because child stream is infinite.
    // When example switches, we must re-subscribe to the new child element.
    let child_stream = switch_map(
        settings_variable.clone().stream(),
        |value| value.expect_object().expect_variable("child").stream()
    )
        .map({
            let construct_context = construct_context.clone();
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

    // Padding with directional support
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let padding_signal = signal::from_stream({
        let style_stream = switch_map(
            sv7.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("padding") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| async move {
                match value {
                    Value::Number(n, _) => Some(format!("{}px", n.number())),
                    Value::Object(obj, _) => {
                        let top = if let Some(v) = obj.variable("top") {
                            if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                                n.number()
                            } else { 0.0 }
                        } else if let Some(v) = obj.variable("column") {
                            if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                                n.number()
                            } else { 0.0 }
                        } else { 0.0 };
                        let right = if let Some(v) = obj.variable("right") {
                            if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                                n.number()
                            } else { 0.0 }
                        } else if let Some(v) = obj.variable("row") {
                            if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                                n.number()
                            } else { 0.0 }
                        } else { 0.0 };
                        let bottom = if let Some(v) = obj.variable("bottom") {
                            if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                                n.number()
                            } else { 0.0 }
                        } else if let Some(v) = obj.variable("column") {
                            if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                                n.number()
                            } else { 0.0 }
                        } else { 0.0 };
                        let left = if let Some(v) = obj.variable("left") {
                            if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                                n.number()
                            } else { 0.0 }
                        } else if let Some(v) = obj.variable("row") {
                            if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                                n.number()
                            } else { 0.0 }
                        } else { 0.0 };
                        Some(format!("{}px {}px {}px {}px", top, right, bottom, left))
                    }
                    _ => None,
                }
            })
            .boxed_local()
    });

    // Font size
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_size.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let font_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("font") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            font_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("size") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| {
            let result = if let Value::Number(n, _) = value {
                Some(format!("{}px", n.number()))
            } else { None };
            future::ready(result)
        })
        .boxed_local()
    });

    // Font color
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let font_color_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_color.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let font_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("font") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            switch_map(
                font_stream,
                |value| {
                    let obj = value.expect_object();
                    match obj.variable("color") {
                        Some(var) => var.stream().left_stream(),
                        None => stream::empty().right_stream(),
                    }
                }
            ),
            |value| oklch_to_css_stream(value)
        )
        .boxed_local()
    });

    // Font weight
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let font_weight_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_weight.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let font_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("font") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            font_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("weight") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => {
                    match tag.tag() {
                        "Hairline" => Some("100"),
                        "ExtraLight" | "UltraLight" => Some("200"),
                        "Light" => Some("300"),
                        "Regular" | "Normal" => Some("400"),
                        "Medium" => Some("500"),
                        "SemiBold" | "DemiBold" => Some("600"),
                        "Bold" => Some("700"),
                        "ExtraBold" | "UltraBold" => Some("800"),
                        "Black" | "Heavy" => Some("900"),
                        _ => None,
                    }.map(|s| s.to_string())
                }
                Value::Number(n, _) => Some(n.number().to_string()),
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Align (row: Center -> text-align: center + display: flex + justify-content)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let align_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_align.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let align_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("align") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            align_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("row") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => {
                    match tag.tag() {
                        "Center" => Some("center"),
                        "Left" | "Start" => Some("flex-start"),
                        "Right" | "End" => Some("flex-end"),
                        _ => None,
                    }.map(|s| s.to_string())
                }
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let width_signal = signal::from_stream({
        let style_stream = switch_map(
            sv2.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("width") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| future::ready(match value {
            Value::Number(n, _) => Some(format!("{}px", n.number())),
            _ => None,
        }))
    });

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let height_signal = signal::from_stream({
        let style_stream = switch_map(
            sv3.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("height") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| future::ready(match value {
            Value::Number(n, _) => Some(format!("{}px", n.number())),
            _ => None,
        }))
    });

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let background_signal = signal::from_stream({
        let style_stream = switch_map(
            sv4.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let bg_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("background") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            switch_map(
                bg_stream,
                |value| {
                    let obj = value.expect_object();
                    match obj.variable("color") {
                        Some(var) => var.stream().left_stream(),
                        None => stream::empty().right_stream(),
                    }
                }
            ),
            |value| oklch_to_css_stream(value)
        )
        .boxed_local()
    });

    // Background image URL
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let background_image_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_bg_url.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let bg_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("background") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            bg_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("url") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| future::ready(match value {
            Value::Text(text, _) => Some(format!("url(\"{}\")", text.text())),
            _ => None,
        }))
    });

    // Size (shorthand for width + height)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let size_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_size.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("size") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| future::ready(match value {
            Value::Number(n, _) => Some(format!("{}px", n.number())),
            _ => None,
        }))
    });

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let border_radius_signal = signal::from_stream({
        let style_stream = switch_map(
            sv5.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("rounded_corners") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| future::ready(match value {
            Value::Number(n, _) => Some(format!("{}px", n.number())),
            _ => None,
        }))
    });

    // Transform: move_right, move_down, and rotate
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let transform_signal = signal::from_stream({
        let style_stream = switch_map(
            sv6.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("transform") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
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
                let mut transforms = Vec::new();
                if move_right != 0.0 || move_down != 0.0 {
                    transforms.push(format!("translate({}px, {}px)", move_right, move_down));
                }
                if rotate != 0.0 {
                    transforms.push(format!("rotate({}deg)", rotate));
                }
                if transforms.is_empty() {
                    None
                } else {
                    Some(transforms.join(" "))
                }
            })
            .boxed_local()
    });

    // Size signal for height (duplicate for separate signal)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_size2 = tagged_object.expect_variable("settings");
    let size_for_height_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_size2.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("size") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| future::ready(match value {
            Value::Number(n, _) => Some(format!("{}px", n.number())),
            _ => None,
        }))
    });

    El::new()
        .update_raw_el(|raw_el| {
            raw_el
                .style_signal("padding", padding_signal)
                // Apply width from style.width, then size overrides if present
                .style_signal("width", width_signal)
                .style_signal("width", size_signal)
                .style_signal("height", height_signal)
                .style_signal("height", size_for_height_signal)
                .style_signal("background-color", background_signal)
                .style_signal("background-image", background_image_signal)
                .style("background-size", "contain")
                .style("background-repeat", "no-repeat")
                .style_signal("border-radius", border_radius_signal)
                .style_signal("transform", transform_signal)
                .style_signal("font-size", font_size_signal)
                .style_signal("color", font_color_signal)
                .style_signal("font-weight", font_weight_signal)
                .style_signal("text-align", align_signal)
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("align-items", "center")
        })
        .child_signal(signal::from_stream(child_stream))
        .after_remove(move |_| {
            drop(tagged_object);
        })
}

fn element_stripe(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    // TimestampedEvent captures Lamport time at DOM callback for consistent ordering
    let (hovered_sender, mut hovered_receiver) = NamedChannel::<TimestampedEvent<bool>>::new("element.hovered", 2);

    // Set up hovered link if element field exists with hovered property
    // Access element through settings, like other properties (style, direction, etc.)
    let sv_element_for_hover = tagged_object.expect_variable("settings");
    // CRITICAL: Use switch_map (not flat_map) because element variable stream is infinite.
    let hovered_stream = switch_map(
        sv_element_for_hover.stream(),
        |value| {
            // Get element from settings object if it exists
            let obj = value.expect_object();
            match obj.variable("element") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        }
    )
    .filter_map(|value| {
        let obj = value.expect_object();
        future::ready(obj.variable("hovered"))
    })
    .map(|variable| variable.expect_link_value_sender())
    .chain(stream::pending());

    let hovered_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let mut hovered_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut hovered_stream = hovered_stream.fuse();
            loop {
                select! {
                    new_sender = hovered_stream.next() => {
                        if let Some(sender) = new_sender {
                            // Send initial hover state (false) when link is established
                            let initial_hover_value = EngineTag::new_value(
                                ConstructInfo::new("stripe::hovered::initial", None, "Initial stripe hovered state"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                "False",
                            );
                            sender.send_or_drop(initial_hover_value);
                            hovered_link_value_sender = Some(sender);
                        }
                    }
                    event = hovered_receiver.select_next_some() => {
                        if let Some(sender) = hovered_link_value_sender.as_ref() {
                            let hover_tag = if event.data { "True" } else { "False" };
                            let event_value = EngineTag::new_value_with_lamport_time(
                                ConstructInfo::new("stripe::hovered", None, "Stripe hovered state"),
                                construct_context.clone(),
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

    let direction_stream = switch_map(
        settings_variable.clone().stream(),
        |value| {
            let object = value.expect_object();
            // object.expect_variable returns Arc<Variable> which is kept alive by switch_map
            object.expect_variable("direction").stream()
        }
    )
    .map(|direction| match direction.expect_tag().tag() {
        "Column" => Direction::Column,
        "Row" => Direction::Row,
        other => panic!("Invalid Stripe element direction value: Found: '{other}', Expected: 'Column' or 'Row'"),
    });

    let gap_stream = switch_map(
        settings_variable.clone().stream(),
        |value| {
            let object = value.expect_object();
            object.expect_variable("gap").stream()
        }
    )
    .filter_map(|value| {
        future::ready(match value {
            Value::Number(n, _) => Some(format!("{}px", n.number())),
            _ => None,
        })
    });

    // Style property streams for element_stripe
    // Width with Fill and min/max support: Fill | number | [sizing: Fill, minimum: X, maximum: Y]
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_width = tagged_object.expect_variable("settings");
    let width_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_width.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("width") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                Value::Tag(tag, _) if tag.tag() == "Fill" => Some("100%".to_string()),
                Value::Object(obj, _) => {
                    // Handle [sizing: Fill, minimum: X, maximum: Y]
                    if let Some(v) = obj.variable("sizing") {
                        if let Ok(Value::Tag(tag, _)) = v.value_actor().current_value().await {
                            if tag.tag() == "Fill" {
                                return Some("100%".to_string());
                            }
                        }
                    }
                    None
                }
                _ => None,
            }
        })
        .boxed_local()
    });

    // Min-width
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_min_width = tagged_object.expect_variable("settings");
    let min_width_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_min_width.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("width") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| async move {
            if let Value::Object(obj, _) = value {
                if let Some(v) = obj.variable("minimum") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        return Some(format!("{}px", n.number()));
                    }
                }
            }
            None
        })
        .boxed_local()
    });

    // Max-width
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_max_width = tagged_object.expect_variable("settings");
    let max_width_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_max_width.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("width") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| async move {
            if let Value::Object(obj, _) = value {
                if let Some(v) = obj.variable("maximum") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        return Some(format!("{}px", n.number()));
                    }
                }
            }
            None
        })
        .boxed_local()
    });

    // Height with Fill and minimum: Screen support
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_height = tagged_object.expect_variable("settings");
    let height_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_height.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("height") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                Value::Tag(tag, _) if tag.tag() == "Fill" => Some("100%".to_string()),
                Value::Object(obj, _) => {
                    if let Some(v) = obj.variable("sizing") {
                        if let Ok(Value::Tag(tag, _)) = v.value_actor().current_value().await {
                            if tag.tag() == "Fill" {
                                return Some("100%".to_string());
                            }
                        }
                    }
                    None
                }
                _ => None,
            }
        })
        .boxed_local()
    });

    // Min-height (supports Screen -> 100vh)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_min_height = tagged_object.expect_variable("settings");
    let min_height_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_min_height.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("height") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| async move {
            if let Value::Object(obj, _) = value {
                if let Some(v) = obj.variable("minimum") {
                    match v.value_actor().current_value().await {
                        Ok(Value::Number(n, _)) => return Some(format!("{}px", n.number())),
                        Ok(Value::Tag(tag, _)) if tag.tag() == "Screen" => return Some("100vh".to_string()),
                        _ => {}
                    }
                }
            }
            None
        })
        .boxed_local()
    });

    // Background color
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let sv_bg = tagged_object.expect_variable("settings");
    let background_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_bg.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let bg_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("background") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            switch_map(
                bg_stream,
                |value| {
                    let obj = value.expect_object();
                    match obj.variable("color") {
                        Some(var) => var.stream().left_stream(),
                        None => stream::empty().right_stream(),
                    }
                }
            ),
            |value| oklch_to_css_stream(value)
        )
        .boxed_local()
    });

    // Padding (directional: [top, column, left, right, row, bottom])
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_padding = tagged_object.expect_variable("settings");
    let padding_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_padding.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("padding") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| async move {
                match value {
                    Value::Number(n, _) => Some(format!("{}px", n.number())),
                    Value::Object(obj, _) => {
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
                        let top = get_num(&obj, "top").await;
                        let bottom = get_num(&obj, "bottom").await;
                        let left = get_num(&obj, "left").await;
                        let right = get_num(&obj, "right").await;
                        let column = get_num(&obj, "column").await;
                        let row = get_num(&obj, "row").await;

                        let final_top = if top > 0.0 { top } else { column };
                        let final_bottom = if bottom > 0.0 { bottom } else { column };
                        let final_left = if left > 0.0 { left } else { row };
                        let final_right = if right > 0.0 { right } else { row };

                        Some(format!("{}px {}px {}px {}px", final_top, final_right, final_bottom, final_left))
                    }
                    _ => None,
                }
            })
            .boxed_local()
    });

    // Shadows (box-shadow from LIST of shadow objects)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_shadows = tagged_object.expect_variable("settings");
    let shadows_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_shadows.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("shadows") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| async move {
                if let Value::List(list, _) = &value {
                    let mut shadow_parts = Vec::new();
                    // Get all items from the list (snapshot returns (ItemId, Arc<ValueActor>) pairs)
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
                            let x = get_num(&obj, "x").await;
                            let y = get_num(&obj, "y").await;
                            let blur = get_num(&obj, "blur").await;
                            let spread = get_num(&obj, "spread").await;

                            // Check for inset (direction: Inwards)
                            let inset = if let Some(v) = obj.variable("direction") {
                                match v.value_actor().current_value().await {
                                    Ok(Value::Tag(tag, _)) if tag.tag() == "Inwards" => true,
                                    _ => false,
                                }
                            } else {
                                false
                            };

                            // Get color
                            let color_css = if let Some(v) = obj.variable("color") {
                                if let Ok(color_value) = v.value_actor().current_value().await {
                                    oklch_to_css(color_value).await.unwrap_or_else(|| "rgba(0,0,0,0.2)".to_string())
                                } else {
                                    "rgba(0,0,0,0.2)".to_string()
                                }
                            } else {
                                "rgba(0,0,0,0.2)".to_string()
                            };

                            let inset_str = if inset { "inset " } else { "" };
                            shadow_parts.push(format!("{}{}px {}px {}px {}px {}", inset_str, x, y, blur, spread, color_css));
                        }
                    }
                    if !shadow_parts.is_empty() {
                        Some(shadow_parts.join(", "))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .boxed_local()
    });

    // Font size (cascading to children)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_size = tagged_object.expect_variable("settings");
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_size.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let font_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("font") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            font_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("size") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            })
        })
    });

    // Font color
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let sv_font_color = tagged_object.expect_variable("settings");
    let font_color_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_color.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let font_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("font") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            switch_map(
                font_stream,
                |value| {
                    let obj = value.expect_object();
                    match obj.variable("color") {
                        Some(var) => var.stream().left_stream(),
                        None => stream::empty().right_stream(),
                    }
                }
            ),
            |value| oklch_to_css_stream(value)
        )
        .boxed_local()
    });

    // Font weight
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_weight = tagged_object.expect_variable("settings");
    let font_weight_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_weight.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let font_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("font") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            font_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("weight") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => {
                    match tag.tag() {
                        "Hairline" => Some("100"),
                        "ExtraLight" | "UltraLight" => Some("200"),
                        "Light" => Some("300"),
                        "Regular" | "Normal" => Some("400"),
                        "Medium" => Some("500"),
                        "SemiBold" | "DemiBold" => Some("600"),
                        "Bold" => Some("700"),
                        "ExtraBold" | "UltraBold" => Some("800"),
                        "Black" | "Heavy" => Some("900"),
                        _ => None,
                    }.map(|s| s.to_string())
                }
                Value::Number(n, _) => Some(n.number().to_string()),
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Font family
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_family = tagged_object.expect_variable("settings");
    let font_family_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_family.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let font_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("font") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            font_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("family") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| async move {
            if let Value::List(list, _) = &value {
                // Get all items from list (snapshot returns (ItemId, Arc<ValueActor>) pairs)
                let snapshot = list.snapshot().await;
                let mut families = Vec::new();
                for (_item_id, actor) in snapshot {
                    if let Ok(item) = actor.current_value().await {
                        match item {
                            Value::Text(t, _) => families.push(format!("\"{}\"", t.text())),
                            Value::Tag(tag, _) => match tag.tag() {
                                "SansSerif" => families.push("sans-serif".to_string()),
                                "Serif" => families.push("serif".to_string()),
                                "Monospace" => families.push("monospace".to_string()),
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                }
                if !families.is_empty() {
                    Some(families.join(", "))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .boxed_local()
    });

    // Font align (text-align)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_align = tagged_object.expect_variable("settings");
    let font_align_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_align.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let font_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("font") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            font_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("align") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => {
                    match tag.tag() {
                        "Center" => Some("center"),
                        "Left" => Some("left"),
                        "Right" => Some("right"),
                        _ => None,
                    }.map(|s| s.to_string())
                }
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Borders (supports [top: [color: Oklch[...]]])
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_borders = tagged_object.expect_variable("settings");
    let border_top_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_borders.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let borders_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("borders") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        let top_stream = switch_map(
            borders_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("top") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            top_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("color") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| async move {
            if let Some(color) = oklch_to_css(value).await {
                Some(format!("3px solid {}", color))
            } else {
                None
            }
        })
        .boxed_local()
    });

    // Align (row: Center -> justify-content: center for Row, align-items: center for Column)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_align = tagged_object.expect_variable("settings");
    let align_items_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_align.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let align_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("align") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            align_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("row") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => {
                    match tag.tag() {
                        "Center" => Some("center"),
                        "Start" => Some("flex-start"),
                        "End" => Some("flex-end"),
                        _ => None,
                    }.map(|s| s.to_string())
                }
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Justify content signal for column alignment (controls main axis - vertical for Column direction)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_justify = tagged_object.expect_variable("settings");
    let justify_content_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_justify.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let align_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("align") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            align_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("column") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| {
            let result = match value {
                Value::Tag(tag, _) => {
                    match tag.tag() {
                        "Center" => Some("center"),
                        "Start" => Some("flex-start"),
                        "End" => Some("flex-end"),
                        _ => None,
                    }.map(|s| s.to_string())
                }
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Use switch_map for items stream - critical for proper re-rendering when example switches
    let items_vec_diff_stream = switch_map(
        switch_map(
            settings_variable.stream(),
            |value| {
                let object = value.expect_object();
                object.expect_variable("items").stream()
            }
        ),
        |value| {
            // value.expect_list() returns Arc<List> which is kept alive by the stream
            list_change_to_vec_diff_stream(value.expect_list().stream())
        }
    );

    Stripe::new()
        .direction_signal(signal::from_stream(direction_stream).map(Option::unwrap_or_default))
        .items_signal_vec(VecDiffStreamSignalVec(items_vec_diff_stream).map_signal(
            move |value_actor| {
                // value_actor is kept alive by the stream_sync() → signal::from_stream chain
                signal::from_stream(value_actor.stream().map({
                    let construct_context = construct_context.clone();
                    move |value| value_to_element(value, construct_context.clone())
                }))
            },
        ))
        .on_hovered_change(move |is_hovered| {
            // Capture Lamport time NOW at DOM callback, before channel
            hovered_sender.send_or_drop(TimestampedEvent::now(is_hovered));
        })
        .update_raw_el(|raw_el| {
            raw_el
                .style_signal("gap", signal::from_stream(gap_stream))
                .style_signal("width", width_signal)
                .style_signal("min-width", min_width_signal)
                .style_signal("max-width", max_width_signal)
                .style_signal("height", height_signal)
                .style_signal("min-height", min_height_signal)
                .style_signal("background-color", background_signal)
                .style_signal("padding", padding_signal)
                .style_signal("box-shadow", shadows_signal)
                .style_signal("font-size", font_size_signal)
                .style_signal("color", font_color_signal)
                .style_signal("font-weight", font_weight_signal)
                .style_signal("font-family", font_family_signal)
                .style_signal("text-align", font_align_signal)
                .style_signal("border-top", border_top_signal)
                .style_signal("align-items", align_items_signal)
                .style_signal("justify-content", justify_content_signal)
        })
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
    let settings_variable = tagged_object.expect_variable("settings");

    // NOTE: Arc-wrapped Objects/Variables stay alive through the stream chain.
    // No Mutex needed - expect_variable returns Arc<Variable>, expect_list returns Arc<List>.

    // Use switch_map for layers stream - critical for proper re-rendering when example switches
    let layers_vec_diff_stream = switch_map(
        switch_map(
            settings_variable.clone().stream(),
            |value| {
                let object = value.expect_object();
                object.expect_variable("layers").stream()
            }
        ),
        |value| {
            list_change_to_vec_diff_stream(value.expect_list().stream())
        }
    );

    // Create individual style streams directly from settings
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let settings_variable_2 = tagged_object.expect_variable("settings");
    let settings_variable_3 = tagged_object.expect_variable("settings");
    let settings_variable_4 = tagged_object.expect_variable("settings");

    let width_signal = signal::from_stream({
        let style_stream = switch_map(
            settings_variable_2.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("width") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| future::ready(match value {
            Value::Number(n, _) => Some(format!("{}px", n.number())),
            _ => None,
        }))
    });

    let height_signal = signal::from_stream({
        let style_stream = switch_map(
            settings_variable_3.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("height") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        )
        .filter_map(|value| future::ready(match value {
            Value::Number(n, _) => Some(format!("{}px", n.number())),
            _ => None,
        }))
    });

    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let background_signal = signal::from_stream({
        let style_stream = switch_map(
            settings_variable_4.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        let bg_stream = switch_map(
            style_stream,
            |value| {
                let obj = value.expect_object();
                match obj.variable("background") {
                    Some(var) => var.stream().left_stream(),
                    None => stream::empty().right_stream(),
                }
            }
        );
        switch_map(
            switch_map(
                bg_stream,
                |value| {
                    let obj = value.expect_object();
                    match obj.variable("color") {
                        Some(var) => var.stream().left_stream(),
                        None => stream::empty().right_stream(),
                    }
                }
            ),
            |value| oklch_to_css_stream(value)
        )
        .boxed_local()
    });

    Stack::new()
        .update_raw_el(|raw_el| {
            raw_el
                .style_signal("width", width_signal)
                .style_signal("height", height_signal)
                .style_signal("background-color", background_signal)
        })
        .layers_signal_vec(VecDiffStreamSignalVec(layers_vec_diff_stream).map_signal(
            move |value_actor| {
                signal::from_stream(value_actor.stream().map({
                    let construct_context = construct_context.clone();
                    move |value| value_to_element(value, construct_context.clone())
                }))
            },
        ))
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
                    format!("oklch({}% {} {} / {})", lightness * 100.0, chroma, hue, alpha)
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

/// Create a reactive stream that emits CSS color strings whenever Oklch components change.
/// This fixes the bug where Oklch internal variables (lightness, chroma, hue) weren't subscribed to.
/// When any Oklch component (lightness, chroma, hue, alpha) changes, a new CSS string is emitted.
fn oklch_to_css_stream(value: Value) -> LocalBoxStream<'static, String> {
    match value {
        Value::TaggedObject(tagged, _) if tagged.tag() == "Oklch" => {
            // Create streams for each component, with defaults for missing variables
            // Use enum to identify which component is emitting
            #[derive(Clone, Copy)]
            enum Component { Lightness, Chroma, Hue, Alpha }

            let lightness_stream: LocalBoxStream<'static, (Component, f64)> =
                if let Some(v) = tagged.variable("lightness") {
                    v.stream()
                        .filter_map(|val| future::ready(match &val {
                            Value::Number(n, _) => Some((Component::Lightness, n.number())),
                            _ => None,
                        }))
                        .boxed_local()
                } else {
                    stream::once(future::ready((Component::Lightness, 0.5)))
                        .chain(stream::pending())
                        .boxed_local()
                };

            let chroma_stream: LocalBoxStream<'static, (Component, f64)> =
                if let Some(v) = tagged.variable("chroma") {
                    v.stream()
                        .filter_map(|val| future::ready(match val {
                            Value::Number(n, _) => Some((Component::Chroma, n.number())),
                            _ => None,
                        }))
                        .boxed_local()
                } else {
                    stream::once(future::ready((Component::Chroma, 0.0)))
                        .chain(stream::pending())
                        .boxed_local()
                };

            let hue_stream: LocalBoxStream<'static, (Component, f64)> =
                if let Some(v) = tagged.variable("hue") {
                    v.stream()
                        .filter_map(|val| future::ready(match val {
                            Value::Number(n, _) => Some((Component::Hue, n.number())),
                            _ => None,
                        }))
                        .boxed_local()
                } else {
                    stream::once(future::ready((Component::Hue, 0.0)))
                        .chain(stream::pending())
                        .boxed_local()
                };

            let alpha_stream: LocalBoxStream<'static, (Component, f64)> =
                if let Some(v) = tagged.variable("alpha") {
                    v.stream()
                        .filter_map(|val| future::ready(match val {
                            Value::Number(n, _) => Some((Component::Alpha, n.number())),
                            _ => None,
                        }))
                        .boxed_local()
                } else {
                    stream::once(future::ready((Component::Alpha, 1.0)))
                        .chain(stream::pending())
                        .boxed_local()
                };

            // Combine all streams - emit new CSS whenever any component changes
            // Use scan to maintain state of all components
            stream::select_all([
                lightness_stream,
                chroma_stream,
                hue_stream,
                alpha_stream,
            ])
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

fn element_button(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    // TimestampedEvent captures Lamport time at DOM callback for consistent ordering
    let (press_event_sender, mut press_event_receiver) = NamedChannel::<TimestampedEvent<()>>::new("button.press_event", 8);
    let (hovered_sender, mut hovered_receiver) = NamedChannel::<TimestampedEvent<bool>>::new("button.hovered", 2);

    let element_variable = tagged_object.expect_variable("element");

    // Set up press event handler - use same subscription pattern as text_input
    // Chain with pending() to prevent stream termination, which would cause busy-polling
    // in the select! loop (fused stream returns Ready(None) immediately when exhausted)
    // Use switch_map (not flat_map) because variable.stream() is infinite.
    // When element is recreated, switch_map cancels old subscription and re-subscribes to new one.
    let mut press_stream = switch_map(
        element_variable
            .clone()
            .stream()
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable.stream()
    )
        .filter_map(|value| future::ready(value.expect_object().variable("press")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    // Set up hovered link if element field exists with hovered property
    // Chain with pending() to prevent stream termination (same as press_stream)
    let hovered_stream = element_variable
        .stream()
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending());

    let event_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let mut press_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut hovered_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut press_event_object_value_version = 0u64;
            let mut hovered_stream = hovered_stream.fuse();
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
                            let initial_hover_value = EngineTag::new_value(
                                ConstructInfo::new("button::hovered::initial", None, "Initial button hovered state"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                "False",
                            );
                            sender.send_or_drop(initial_hover_value);
                            hovered_link_value_sender = Some(sender);
                        }
                    }
                    event = press_event_receiver.select_next_some() => {
                        if let Some(press_link_value_sender) = press_link_value_sender.as_ref() {
                            let press_event_object_value = Object::new_value_with_lamport_time(
                                ConstructInfo::new(format!("bridge::element_button::press_event, version: {press_event_object_value_version}"), None, "Button press event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                [],
                            );
                            press_event_object_value_version += 1;
                            press_link_value_sender.send_or_drop(press_event_object_value);
                        }
                    }
                    event = hovered_receiver.select_next_some() => {
                        if let Some(sender) = hovered_link_value_sender.as_ref() {
                            let hover_tag = if event.data { "True" } else { "False" };
                            let event_value = EngineTag::new_value_with_lamport_time(
                                ConstructInfo::new("button::hovered", None, "Button hovered state"),
                                construct_context.clone(),
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

    // CRITICAL: Use switch_map (not flat_map) because label variable stream is infinite.
    // When settings change, switch_map cancels the old label subscription.
    let label_stream = switch_map(
        settings_variable.clone().stream(),
        |value| value.expect_object().expect_variable("label").stream()
    )
    .map({
        let construct_context = construct_context.clone();
        move |value| value_to_element(value, construct_context.clone())
    });

    // Font size signal
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_size = settings_variable.clone();
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_size.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
                Some(format!("{}px", n.number()))
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
        let style_stream = switch_map(
            sv_font_color.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
            |value| oklch_to_css_stream(value)
        )
        .boxed_local()
    });

    // Padding signal
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_padding = settings_variable.clone();
    let padding_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_padding.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("padding") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                Value::Object(obj, _) => {
                    let top = if let Some(v) = obj.variable("top") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number()
                        } else { 0.0 }
                    } else if let Some(v) = obj.variable("column") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number()
                        } else { 0.0 }
                    } else { 0.0 };
                    let right = if let Some(v) = obj.variable("right") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number()
                        } else { 0.0 }
                    } else if let Some(v) = obj.variable("row") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number()
                        } else { 0.0 }
                    } else { 0.0 };
                    let bottom = if let Some(v) = obj.variable("bottom") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number()
                        } else { 0.0 }
                    } else if let Some(v) = obj.variable("column") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number()
                        } else { 0.0 }
                    } else { 0.0 };
                    let left = if let Some(v) = obj.variable("left") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number()
                        } else { 0.0 }
                    } else if let Some(v) = obj.variable("row") {
                        if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                            n.number()
                        } else { 0.0 }
                    } else { 0.0 };
                    Some(format!("{}px {}px {}px {}px", top, right, bottom, left))
                }
                _ => None,
            }
        })
        .boxed_local()
    });

    // Size (width) signal
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_size_width = settings_variable.clone();
    let size_width_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_size_width.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = if let Value::Number(n, _) = value {
                Some(format!("{}px", n.number()))
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
        let style_stream = switch_map(
            sv_size_height.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("size") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = if let Value::Number(n, _) = value {
                Some(format!("{}px", n.number()))
            } else {
                None
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Rounded corners signal
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_rounded = settings_variable.clone();
    let rounded_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_rounded.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("rounded_corners") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            let result = if let Value::Number(n, _) = value {
                Some(format!("{}px", n.number()))
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
        let style_stream = switch_map(
            sv_transform.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
                    } else { 0.0 }
                } else { 0.0 };
                let move_down = if let Some(v) = obj.variable("move_down") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        n.number()
                    } else { 0.0 }
                } else { 0.0 };
                let move_up = if let Some(v) = obj.variable("move_up") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        n.number()
                    } else { 0.0 }
                } else { 0.0 };
                let move_right = if let Some(v) = obj.variable("move_right") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        n.number()
                    } else { 0.0 }
                } else { 0.0 };
                let rotate = if let Some(v) = obj.variable("rotate") {
                    if let Ok(Value::Number(n, _)) = v.value_actor().current_value().await {
                        n.number()
                    } else { 0.0 }
                } else { 0.0 };
                // Calculate final x (negative for move_left, positive for move_right)
                let x = move_right - move_left;
                // Calculate final y (positive for move_down, negative for move_up)
                let y = move_down - move_up;
                let mut transforms = Vec::new();
                if x != 0.0 || y != 0.0 {
                    transforms.push(format!("translate({}px, {}px)", x, y));
                }
                if rotate != 0.0 {
                    transforms.push(format!("rotate({}deg)", rotate));
                }
                if transforms.is_empty() {
                    None
                } else {
                    Some(transforms.join(" "))
                }
            } else {
                None
            }
        })
        .boxed_local()
    });

    // Font align signal (text-align)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_align = settings_variable.clone();
    let font_align_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_align.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
            let result = match value {
                Value::Tag(tag, _) => {
                    match tag.tag() {
                        "Center" => Some("center".to_string()),
                        "Left" => Some("left".to_string()),
                        "Right" => Some("right".to_string()),
                        _ => None,
                    }
                }
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    // Background color signal
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let sv_background = settings_variable.clone();
    let background_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_background.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
            |value| oklch_to_css_stream(value)
        )
        .boxed_local()
    });

    // Outline signal - handles both NoOutline tag and Object with color/thickness/style fields.
    // CRITICAL: Uses nested switch_map (not flat_map) because all variable streams are infinite.
    // The innermost switch_map handles pending() streams from NoOutline tag.
    let sv_outline = settings_variable.clone();
    let button_id = Arc::as_ptr(&sv_outline) as usize;
    let outline_value_stream = {
        let style_stream = switch_map(
            sv_outline.stream(),
            move |value| {
                zoon::println!("[OUTLINE_DEBUG btn={:x}] sv_outline emitted, getting style variable", button_id);
                value.expect_object().expect_variable("style").stream()
            }
        );
        switch_map(style_stream, move |value| {
            zoon::println!("[OUTLINE_DEBUG btn={:x}] style_stream emitted, getting outline variable", button_id);
            let obj = value.expect_object();
            match obj.variable("outline") {
                Some(var) => {
                    zoon::println!("[OUTLINE_DEBUG btn={:x}] Got outline variable, subscribing to its value_actor", button_id);
                    var.value_actor().clone().stream()
                        .map(move |v| {
                            zoon::println!("[OUTLINE_DEBUG btn={:x}] outline value_actor emitted: {}", button_id, match &v {
                                Value::Tag(t, _) => format!("Tag({})", t.tag()),
                                Value::Object(_, _) => "Object".to_string(),
                                _ => "other".to_string(),
                            });
                            v
                        })
                        .left_stream()
                }
                None => stream::empty().right_stream(),
            }
        })
    };
    let outline_signal = signal::from_stream(
        switch_map(outline_value_stream, |value| {
            match &value {
                Value::Tag(tag, _) if tag.tag() == "NoOutline" => {
                    stream::once(future::ready("none".to_string()))
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
                                    zoon::println!("[OUTLINE] Generated CSS: {}px {} {}", thickness, line_style, css_color);
                                    return Some(format!("{}px {} {}", thickness, line_style, css_color));
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
                    .filter_map(|x| async move { x })
                    .chain(stream::pending())
                    .boxed_local()
                }
                other => {
                    log_unexpected_type("button outline", "Object or NoOutline tag", other);
                    stream::pending::<String>().boxed_local()
                }
            }
        })
    );

    Button::new()
        .label_signal(signal::from_stream(label_stream).map(|label| {
            if let Some(label) = label {
                label
            } else {
                zoon::Text::new("").unify()
            }
        }))
        // @TODO Handle press event only when it's defined in Boon code? Add `.on_press_signal` to Zoon?
        .on_press(move || {
            // Capture Lamport time NOW at DOM callback, before channel
            press_event_sender.send_or_drop(TimestampedEvent::now(()));
        })
        .on_hovered_change(move |is_hovered| {
            // Capture Lamport time NOW at DOM callback, before channel
            hovered_sender.send_or_drop(TimestampedEvent::now(is_hovered));
        })
        .update_raw_el(|raw_el| {
            raw_el
                .style_signal("font-size", font_size_signal)
                .style_signal("color", font_color_signal)
                .style_signal("padding", padding_signal)
                .style_signal("width", size_width_signal)
                .style_signal("height", size_height_signal)
                .style_signal("border-radius", rounded_signal)
                .style_signal("transform", transform_signal)
                .style_signal("text-align", font_align_signal)
                .style_signal("outline", outline_signal)
                .style_signal("background-color", background_signal)
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
    zoon::println!("[EVENT:TextInput:v2] element_text_input CALLED - creating new TextInput");
    // Separate channels for each event type.
    // TimestampedEvent captures Lamport time at DOM callback, ensuring correct ordering
    // even when select! processes events out of order.
    let (change_event_sender, mut change_event_receiver) = NamedChannel::<TimestampedEvent<String>>::new("text_input.change", 16);
    let (key_down_event_sender, mut key_down_event_receiver) = NamedChannel::<TimestampedEvent<String>>::new("text_input.key_down", 32);
    let (blur_event_sender, mut blur_event_receiver) = NamedChannel::<TimestampedEvent<()>>::new("text_input.blur", 8);

    let element_variable = tagged_object.expect_variable("element");

    // Set up event handlers - create separate subscriptions for each event type
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    // CRITICAL: Use switch_map (not flat_map) because variable.stream() is infinite.
    // When element is recreated during example switching, switch_map cancels old subscription
    // and re-subscribes to the new element's event streams, preventing stale LINK bugs.
    let mut change_stream = switch_map(
        element_variable
            .clone()
            .stream()
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable.stream()
    )
        .filter_map(|value| future::ready(value.expect_object().variable("change")))
        .map(move |variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    let mut key_down_stream = switch_map(
        element_variable
            .clone()
            .stream()
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable.stream()
    )
        .filter_map(|value| future::ready(value.expect_object().variable("key_down")))
        .map(move |variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    let mut blur_stream = switch_map(
        element_variable
            .stream()
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable.stream()
    )
        .filter_map(|value| future::ready(value.expect_object().variable("blur")))
        .map(move |variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    // Helper to create change event value with captured Lamport timestamp
    fn create_change_event_value(construct_context: &ConstructContext, text: String, lamport_time: u64) -> Value {
        Object::new_value_with_lamport_time(
            ConstructInfo::new("text_input::change_event", None, "TextInput change event"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            lamport_time,
            [Variable::new_arc(
                ConstructInfo::new("text_input::change_event::text", None, "change text"),
                construct_context.clone(),
                "text",
                ValueActor::new_arc(
                    ConstructInfo::new("text_input::change_event::text_actor", None, "change text actor"),
                    ActorContext::default(),
                    TypedStream::infinite(stream::once(future::ready(EngineText::new_value_with_lamport_time(
                        ConstructInfo::new("text_input::change_event::text_value", None, "change text value"),
                        construct_context.clone(),
                        ValueIdempotencyKey::new(),
                        lamport_time,
                        text,
                    ))).chain(stream::pending())),
                    parser::PersistenceId::new(),
                ),
                parser::PersistenceId::default(),
                parser::Scope::Root,
            )],
        )
    }

    // Helper to create key_down event value with captured Lamport timestamp
    // Only contains 'key', no 'text' - text should be obtained from the change event using LATEST
    fn create_key_down_event_value(construct_context: &ConstructContext, key: String, lamport_time: u64) -> Value {
        Object::new_value_with_lamport_time(
            ConstructInfo::new("text_input::key_down_event", None, "TextInput key_down event"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            lamport_time,
            [
                Variable::new_arc(
                    ConstructInfo::new("text_input::key_down_event::key", None, "key_down key"),
                    construct_context.clone(),
                    "key",
                    ValueActor::new_arc(
                        ConstructInfo::new("text_input::key_down_event::key_actor", None, "key_down key actor"),
                        ActorContext::default(),
                        TypedStream::infinite(stream::once(future::ready(EngineTag::new_value_with_lamport_time(
                            ConstructInfo::new("text_input::key_down_event::key_value", None, "key_down key value"),
                            construct_context.clone(),
                            ValueIdempotencyKey::new(),
                            lamport_time,
                            key,
                        ))).chain(stream::pending())),
                        parser::PersistenceId::new(),
                    ),
                    parser::PersistenceId::default(),
                    parser::Scope::Root,
                ),
            ],
        )
    }

    let event_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            zoon::println!("[EVENT:TextInput] Event handler loop STARTED");
            let mut change_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut key_down_link_value_sender: Option<NamedChannel<Value>> = None;
            let mut blur_link_value_sender: Option<NamedChannel<Value>> = None;

            // RACE CONDITION FIX: Buffer events that arrive before sender is ready.
            // When switching examples quickly, DOM events can arrive before the Boon
            // LINK subscription is established. Without buffering, these events are lost.
            let mut pending_change_events: Vec<TimestampedEvent<String>> = Vec::new();
            let mut pending_key_down_events: Vec<TimestampedEvent<String>> = Vec::new();
            let mut pending_blur_events: Vec<TimestampedEvent<()>> = Vec::new();

            loop {
                select! {
                    // These branches get the Boon-side senders for each event type
                    result = change_stream.next() => {
                        if let Some(sender) = result {
                            zoon::println!("[EVENT:TextInput] change_link_value_sender READY");
                            // Flush any buffered events first
                            for buffered_event in pending_change_events.drain(..) {
                                zoon::println!("[EVENT:TextInput] Flushing buffered change event: lamport={}", buffered_event.lamport_time);
                                sender.send_or_drop(create_change_event_value(&construct_context, buffered_event.data, buffered_event.lamport_time));
                            }
                            change_link_value_sender = Some(sender);
                        }
                    }
                    result = key_down_stream.next() => {
                        if let Some(sender) = result {
                            zoon::println!("[EVENT:TextInput] key_down_link_value_sender READY");
                            // Flush any buffered events first
                            for buffered_event in pending_key_down_events.drain(..) {
                                zoon::println!("[EVENT:TextInput] Flushing buffered key_down event: key='{}', lamport={}",
                                    buffered_event.data, buffered_event.lamport_time);
                                let event_value = create_key_down_event_value(&construct_context, buffered_event.data, buffered_event.lamport_time);
                                let _ = sender.try_send(event_value);
                            }
                            key_down_link_value_sender = Some(sender);
                        }
                    }
                    result = blur_stream.next() => {
                        if let Some(sender) = result {
                            zoon::println!("[EVENT:TextInput] blur_link_value_sender READY");
                            // Flush any buffered events first
                            for buffered_event in pending_blur_events.drain(..) {
                                zoon::println!("[EVENT:TextInput] Flushing buffered blur event: lamport={}", buffered_event.lamport_time);
                                let event_value = Object::new_value_with_lamport_time(
                                    ConstructInfo::new("text_input::blur_event", None, "TextInput blur event"),
                                    construct_context.clone(),
                                    ValueIdempotencyKey::new(),
                                    buffered_event.lamport_time,
                                    [],
                                );
                                sender.send_or_drop(event_value);
                            }
                            blur_link_value_sender = Some(sender);
                        }
                    }
                    // TimestampedEvent carries Lamport time captured at DOM callback
                    // This ensures correct ordering even when select! processes events out of order
                    event = change_event_receiver.select_next_some() => {
                        zoon::println!("[EVENT:TextInput] LOOP received change: text='{}', lamport={}, sender_ready={}",
                            if event.data.len() > 50 { format!("{}...", &event.data[..50]) } else { event.data.clone() },
                            event.lamport_time,
                            change_link_value_sender.is_some());
                        if let Some(sender) = change_link_value_sender.as_ref() {
                            sender.send_or_drop(create_change_event_value(&construct_context, event.data, event.lamport_time));
                        } else {
                            // Buffer event until sender is ready
                            zoon::println!("[EVENT:TextInput] Buffering change event (sender not ready)");
                            pending_change_events.push(event);
                        }
                    }
                    event = key_down_event_receiver.select_next_some() => {
                        zoon::println!("[EVENT:TextInput] LOOP received key_down: key='{}', lamport={}, sender_ready={}",
                            event.data, event.lamport_time, key_down_link_value_sender.is_some());
                        if let Some(sender) = key_down_link_value_sender.as_ref() {
                            let event_value = create_key_down_event_value(&construct_context, event.data, event.lamport_time);
                            let result = sender.try_send(event_value);
                            zoon::println!("[EVENT:TextInput] LINK send key_down result: {:?}", result.is_ok());
                            if result.is_err() {
                                zoon::println!("[EVENT:TextInput] LINK send FAILED - channel closed or full!");
                            }
                        } else {
                            // Buffer event until sender is ready
                            zoon::println!("[EVENT:TextInput] Buffering key_down event (sender not ready)");
                            pending_key_down_events.push(event);
                        }
                    }
                    event = blur_event_receiver.select_next_some() => {
                        zoon::println!("[EVENT:TextInput] LOOP received blur: lamport={}, sender_ready={}",
                            event.lamport_time, blur_link_value_sender.is_some());
                        if let Some(sender) = blur_link_value_sender.as_ref() {
                            let event_value = Object::new_value_with_lamport_time(
                                ConstructInfo::new("text_input::blur_event", None, "TextInput blur event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                event.lamport_time,
                                [],
                            );
                            sender.send_or_drop(event_value);
                        } else {
                            // Buffer event until sender is ready
                            zoon::println!("[EVENT:TextInput] Buffering blur event (sender not ready)");
                            pending_blur_events.push(event);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    // CRITICAL: Use switch_map (not flat_map) because text variable stream is infinite.
    let text_stream = switch_map(
        settings_variable.clone().stream(),
        |value| value.expect_object().expect_variable("text").stream()
    )
    .filter_map(|value| {
        future::ready(match value {
            Value::Text(text, _) => Some(text.text().to_string()),
            _ => None,
        })
    });

    // Placeholder text stream - extract actual text from placeholder object
    // CRITICAL: Use nested switch_map (not flat_map) because all variable streams are infinite.
    let placeholder_text_stream = switch_map(
        settings_variable.clone().stream(),
        |value| value.expect_object().expect_variable("placeholder").stream()
    );
    let placeholder_text_stream = switch_map(
        placeholder_text_stream,
        |value| {
            match value {
                Value::Object(obj, _) => {
                    match obj.variable("text") {
                        Some(var) => var.stream().left_stream(),
                        None => stream::empty().right_stream(),
                    }
                }
                _ => stream::empty().right_stream(),
            }
        }
    )
    .filter_map(|value| {
        future::ready(match value {
            Value::Text(text, _) => Some(text.text().to_string()),
            _ => None,
        })
    });

    // Placeholder signal for TextInput
    let placeholder_signal = signal::from_stream(placeholder_text_stream);

    // Width signal from style
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_width = tagged_object.expect_variable("settings");
    let width_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_width.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
    });

    // Padding signal from style - supports simple number or directional object
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_padding = tagged_object.expect_variable("settings");
    let padding_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_padding.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("padding") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| async move {
            match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                Value::Object(obj, _) => {
                    // Handle directional padding: [top, column, left, right, row, bottom]
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
                    let top = get_num(&obj, "top").await;
                    let bottom = get_num(&obj, "bottom").await;
                    let left = get_num(&obj, "left").await;
                    let right = get_num(&obj, "right").await;
                    let column = get_num(&obj, "column").await;
                    let row = get_num(&obj, "row").await;

                    // column applies to top/bottom, row applies to left/right
                    let final_top = if top > 0.0 { top } else { column };
                    let final_bottom = if bottom > 0.0 { bottom } else { column };
                    let final_left = if left > 0.0 { left } else { row };
                    let final_right = if right > 0.0 { right } else { row };

                    Some(format!("{}px {}px {}px {}px", final_top, final_right, final_bottom, final_left))
                }
                _ => None,
            }
        })
        .boxed_local()
    });

    // Font size signal from style
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_font_size = tagged_object.expect_variable("settings");
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_size.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            })
        })
    });

    // Font color signal from style
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let sv_font_color = tagged_object.expect_variable("settings");
    let font_color_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_font_color.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
            |value| oklch_to_css_stream(value)
        )
        .boxed_local()
    });

    // Background color signal from style
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let sv_bg_color = tagged_object.expect_variable("settings");
    let background_color_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_bg_color.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
            |value| oklch_to_css_stream(value)
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
        settings_variable.stream(),
        |value| value.expect_object().expect_variable("focus").stream()
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
        .placeholder(Placeholder::with_signal(placeholder_signal.map(|t| t.unwrap_or_default())))
        .on_change({
            let sender = change_event_sender.clone();
            move |text| {
                // Capture Lamport time NOW at DOM callback, before channel
                let event = TimestampedEvent::now(text);
                zoon::println!("[EVENT:TextInput] on_change fired: text='{}', lamport={}",
                    if event.data.len() > 50 { format!("{}...", &event.data[..50]) } else { event.data.clone() },
                    event.lamport_time);
                sender.send_or_drop(event);
            }
        })
        .on_key_down_event({
            let sender = key_down_event_sender.clone();
            move |event| {
                let key_name = match event.key() {
                    Key::Enter => "Enter".to_string(),
                    Key::Escape => "Escape".to_string(),
                    Key::Other(k) => k.clone(),
                };
                // Capture Lamport time NOW at DOM callback, before channel
                let ts_event = TimestampedEvent::now(key_name);
                zoon::println!("[EVENT:TextInput] on_key_down fired: key='{}', lamport={}",
                    ts_event.data, ts_event.lamport_time);
                sender.send_or_drop(ts_event);
            }
        })
        .on_blur({
            let sender = blur_event_sender.clone();
            move || {
                // Capture Lamport time NOW at DOM callback, before channel
                let event = TimestampedEvent::now(());
                zoon::println!("[EVENT:TextInput] on_blur fired: lamport={}", event.lamport_time);
                sender.send_or_drop(event);
            }
        })
        .focus_signal(focus_signal)
        .update_raw_el(|raw_el| {
            raw_el
                .style("box-sizing", "border-box")
                .style_signal("width", width_signal)
                .style_signal("padding", padding_signal)
                .style_signal("font-size", font_size_signal)
                .style_signal("color", font_color_signal)
                .style_signal("background-color", background_color_signal)
        })
        .after_remove(move |_| {
            zoon::println!("[EVENT:TextInput] Element REMOVED - dropping event handlers");
            drop(event_handler_loop);
            drop(focus_loop);
        })
}

fn element_checkbox(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    // TimestampedEvent captures Lamport time at DOM callback for consistent ordering
    let (click_event_sender, mut click_event_receiver) = NamedChannel::<TimestampedEvent<()>>::new("checkbox.click", 8);

    let element_variable = tagged_object.expect_variable("element");

    // Use switch_map (not flat_map) because variable.stream() is infinite.
    // When element is recreated, switch_map cancels old subscription and re-subscribes to new one.
    let event_stream = switch_map(
        element_variable
            .stream()
            .filter_map(|value| future::ready(value.expect_object().variable("event"))),
        |variable| variable.stream()
    );

    // Get the click Variable (not just the sender) so we can access its registry_key
    let click_var_stream = event_stream
        .filter_map(|value| future::ready(value.expect_object().variable("click")));

    // Map to sender - the Variable stream already handles uniqueness via persistence_id + scope
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    let click_sender_stream = click_var_stream.map(move |variable| {
        variable.expect_link_value_sender()
    });

    let mut click_sender_stream = click_sender_stream.chain(stream::pending()).fuse();

    let event_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
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
                                    construct_context.clone(),
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
                                construct_context.clone(),
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

    // CRITICAL: Use switch_map (not flat_map) because variable streams are infinite.
    let checked_stream = switch_map(
        settings_variable.clone().stream(),
        |value| value.expect_object().expect_variable("checked").stream()
    )
    .filter_map(|value| {
        future::ready(match value {
            Value::Tag(tag, _) => Some(tag.tag() == "True"),
            _ => None,
        })
    });

    // CRITICAL: Use switch_map (not flat_map) because icon variable stream is infinite.
    let icon_stream = switch_map(
        settings_variable.stream(),
        |value| value.expect_object().expect_variable("icon").stream()
    )
    .map(move |value| value_to_element(value, construct_context.clone()));

    Checkbox::new()
        .label_hidden("checkbox")
        .checked_signal(signal::from_stream(checked_stream).map(|c| c.unwrap_or(false)))
        .icon(move |_checked_mutable| {
            El::new().child_signal(signal::from_stream(icon_stream))
        })
        .on_click({
            let sender = click_event_sender.clone();
            move || {
                // Capture Lamport time NOW at DOM callback, before channel
                sender.send_or_drop(TimestampedEvent::now(()));
            }
        })
        .after_remove(move |_| {
            drop(event_handler_loop)
        })
}

fn element_label(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    // TimestampedEvent captures Lamport time at DOM callback for consistent ordering
    let (double_click_sender, mut double_click_receiver) = NamedChannel::<TimestampedEvent<()>>::new("double_click.event", 8);
    let (hovered_sender, _hovered_receiver) = NamedChannel::<TimestampedEvent<bool>>::new("double_click.hovered", 2);

    let element_variable = tagged_object.expect_variable("element");

    // Set up hovered link
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    let hovered_stream = element_variable
        .clone()
        .stream()
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending());

    // Set up double_click event
    // Use switch_map (not flat_map) because variable.stream() is infinite.
    // When element is recreated, switch_map cancels old subscription and re-subscribes to new one.
    let event_stream = switch_map(
        element_variable
            .stream()
            .filter_map(move |value| {
                let obj = value.expect_object();
                future::ready(obj.variable("event"))
            }),
        move |variable| variable.stream()
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
        let construct_context = construct_context.clone();
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
                            let initial_hover_value = EngineTag::new_value(
                                ConstructInfo::new("label::hovered::initial", None, "Initial label hovered state"),
                                construct_context.clone(),
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
                                construct_context.clone(),
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
    let label_stream = switch_map(
        settings_variable.clone().stream(),
        |value| value.expect_object().expect_variable("label").stream()
    )
    .map({
        let construct_context = construct_context.clone();
        move |value| value_to_element(value, construct_context.clone())
    });

    // Create style streams
    let sv2 = tagged_object.expect_variable("settings");
    let sv3 = tagged_object.expect_variable("settings");
    let sv4 = tagged_object.expect_variable("settings");

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let padding_signal = signal::from_stream({
        let style_stream = switch_map(
            sv2.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
        switch_map(style_stream, |value| {
            let obj = value.expect_object();
            match obj.variable("padding") {
                Some(var) => var.stream().left_stream(),
                None => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| future::ready(match value {
            Value::Number(n, _) => Some(format!("{}px", n.number())),
            _ => None,
        }))
    });

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    // oklch_to_css_stream subscribes to Oklch internal variables (lightness, chroma, hue)
    let font_color_signal = signal::from_stream({
        let style_stream = switch_map(
            sv3.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
            |value| oklch_to_css_stream(value)
        )
        .boxed_local()
    });

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let font_size_signal = signal::from_stream({
        let style_stream = switch_map(
            sv4.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            })
        })
    });

    Label::new()
        .update_raw_el(|raw_el| {
            raw_el
                .style_signal("padding", padding_signal)
                .style_signal("color", font_color_signal)
                .style_signal("font-size", font_size_signal)
        })
        .label_signal(signal::from_stream(label_stream).map(|l| {
            l.unwrap_or_else(|| zoon::Text::new("").unify())
        }))
        .on_double_click({
            let sender = double_click_sender.clone();
            move || {
                // Capture Lamport time NOW at DOM callback, before channel
                sender.send_or_drop(TimestampedEvent::now(()));
            }
        })
        .after_remove(move |_| drop(event_handler_loop))
}

fn element_paragraph(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let settings_variable = tagged_object.expect_variable("settings");

    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let contents_stream = switch_map(
        settings_variable.stream(),
        |value| value.expect_object().expect_variable("contents").stream()
    );
    let contents_vec_diff_stream = switch_map(
        contents_stream,
        |value| list_change_to_vec_diff_stream(value.expect_list().stream())
    );

    Paragraph::new()
        .update_raw_el(|raw_el| {
            raw_el.style("white-space", "pre-wrap")
        })
        .contents_signal_vec(
            VecDiffStreamSignalVec(contents_vec_diff_stream).map_signal(move |value_actor| {
                signal::from_stream(value_actor.stream().map({
                    let construct_context = construct_context.clone();
                    move |value| value_to_element(value, construct_context.clone())
                }))
            }),
        )
}

fn element_link(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    // TimestampedEvent captures Lamport time at DOM callback for consistent ordering
    let (hovered_sender, mut hovered_receiver) = NamedChannel::<TimestampedEvent<bool>>::new("link.hovered", 2);

    let element_variable = tagged_object.expect_variable("element");

    // Set up hovered handler
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    let mut hovered_stream = element_variable
        .stream()
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    let event_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let mut hovered_link_value_sender: Option<NamedChannel<Value>> = None;

            loop {
                select! {
                    sender = hovered_stream.select_next_some() => {
                        // Send initial hover state (false) when link is established
                        let initial_hover_value = EngineTag::new_value(
                            ConstructInfo::new("link::hovered::initial", None, "Initial link hovered state"),
                            construct_context.clone(),
                            ValueIdempotencyKey::new(),
                            "False",
                        );
                        sender.send_or_drop(initial_hover_value);
                        hovered_link_value_sender = Some(sender);
                    }
                    event = hovered_receiver.select_next_some() => {
                        if let Some(sender) = hovered_link_value_sender.as_ref() {
                            let hover_tag = if event.data { "True" } else { "False" };
                            let event_value = EngineTag::new_value_with_lamport_time(
                                ConstructInfo::new("link::hovered", None, "Link hovered state"),
                                construct_context.clone(),
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

    // CRITICAL: Use switch_map (not flat_map) because label variable stream is infinite.
    // When settings change, switch_map cancels the old label subscription.
    let label_stream = switch_map(
        settings_variable.clone().stream(),
        |value| value.expect_object().expect_variable("label").stream()
    )
    .map({
        let construct_context = construct_context.clone();
        move |value| value_to_element(value, construct_context.clone())
    });

    let sv_to = settings_variable.clone();
    // CRITICAL: Use switch_map (not flat_map) because 'to' variable stream is infinite.
    let to_stream = switch_map(
        sv_to.stream(),
        |value| value.expect_object().expect_variable("to").stream()
    )
    .filter_map(|value| {
        future::ready(match value {
            Value::Text(text, _) => Some(text.text().to_string()),
            _ => None,
        })
    });

    // Underline signal (font.line.underline)
    // CRITICAL: Use nested switch_map (not flat_map) because variable streams are infinite.
    let sv_underline = settings_variable.clone();
    let underline_signal = signal::from_stream({
        let style_stream = switch_map(
            sv_underline.stream(),
            |value| value.expect_object().expect_variable("style").stream()
        );
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
                Value::Tag(tag, _) => {
                    match tag.tag() {
                        "True" => Some("underline".to_string()),
                        "False" => Some("none".to_string()),
                        _ => None,
                    }
                }
                _ => None,
            };
            future::ready(result)
        })
        .boxed_local()
    });

    Link::new()
        .label_signal(signal::from_stream(label_stream).map(|l| {
            l.unwrap_or_else(|| zoon::Text::new("").unify())
        }))
        .to_signal(signal::from_stream(to_stream).map(|t| t.unwrap_or_default()))
        .new_tab(NewTab::new())
        .on_hovered_change(move |is_hovered| {
            // Capture Lamport time NOW at DOM callback, before channel
            hovered_sender.send_or_drop(TimestampedEvent::now(is_hovered));
        })
        .update_raw_el(|raw_el| {
            raw_el.style_signal("text-decoration", underline_signal)
        })
        .after_remove(move |_| {
            drop(event_handler_loop);
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
) -> impl Stream<Item = VecDiff<Arc<ValueActor>>> {
    use futures_signals::signal_vec::VecDiff;

    change_stream.scan(
        Vec::<Arc<ValueActor>>::new(),
        move |items, change| {
            let vec_diff = match change {
                ListChange::Replace { items: new_items } => {
                    *items = new_items.clone();
                    VecDiff::Replace { values: new_items }
                }
                ListChange::InsertAt { index, item } => {
                    if index <= items.len() {
                        items.insert(index, item.clone());
                    }
                    VecDiff::InsertAt { index, value: item }
                }
                ListChange::UpdateAt { index, item } => {
                    if index < items.len() {
                        items[index] = item.clone();
                    }
                    VecDiff::UpdateAt { index, value: item }
                }
                ListChange::Remove { id } => {
                    // Find index by PersistenceId
                    if let Some(index) = items.iter().position(|item| item.persistence_id() == id) {
                        items.remove(index);
                        VecDiff::RemoveAt { index }
                    } else {
                        // Item not found - emit a no-op Replace with current items
                        // This shouldn't happen in normal operation
                        VecDiff::Replace { values: items.clone() }
                    }
                }
                ListChange::Move { old_index, new_index } => {
                    if old_index < items.len() {
                        let item = items.remove(old_index);
                        let insert_index = if new_index > old_index {
                            new_index.saturating_sub(1).min(items.len())
                        } else {
                            new_index.min(items.len())
                        };
                        items.insert(insert_index, item);
                    }
                    VecDiff::Move { old_index, new_index }
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
        },
    )
}

