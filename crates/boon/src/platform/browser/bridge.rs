use std::sync::Arc;

use zoon::futures_util::{future, select, stream, StreamExt};
use zoon::futures_channel::mpsc;
use zoon::*;

use super::engine::{
    ActorContext, ActorLoop, ConstructContext, ConstructInfo, ListChange, Object,
    TaggedObject, TypedStream, Value, ValueActor, ValueIdempotencyKey, Variable,
    Text as EngineText, Tag as EngineTag,
};
use crate::parser;

pub fn object_with_document_to_element_signal(
    root_object: Arc<Object>,
    construct_context: ConstructContext,
) -> impl Signal<Item = Option<RawElOrText>> {
    let document_variable = root_object.expect_variable("document").clone();
    let doc_actor = document_variable.value_actor();

    let element_stream = doc_actor.clone().stream_sync()
        .flat_map(|value| {
            let document_object = value.expect_object();
            let root_element_var = document_object.expect_variable("root_element").clone();
            root_element_var.value_actor().clone().stream_sync()
        })
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

    let child_stream = settings_variable
        .clone()
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("child").stream_sync())
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
    let padding_signal = signal::from_stream(
        sv7.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("padding")).flat_map(|var| var.stream_sync())
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
    );

    // Font size
    let font_size_signal = signal::from_stream(
        sv_font_size
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("size")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| {
                let result = if let Value::Number(n, _) = value {
                    Some(format!("{}px", n.number()))
                } else { None };
                future::ready(result)
            })
            .boxed_local()
    );

    // Font color
    let font_color_signal = signal::from_stream(
        sv_font_color
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| oklch_to_css(value))
            .boxed_local()
    );

    // Font weight
    let font_weight_signal = signal::from_stream(
        sv_font_weight
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("weight")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Align (row: Center -> text-align: center + display: flex + justify-content)
    let align_signal = signal::from_stream(
        sv_align
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("align")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("row")).flat_map(|var| var.stream_sync())
            })
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
    );

    let width_signal = signal::from_stream(
        sv2.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("width")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let height_signal = signal::from_stream(
        sv3.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("height")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let background_signal = signal::from_stream(
        sv4.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("background")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| oklch_to_css(value))
            .boxed_local()
    );

    // Background image URL
    let background_image_signal = signal::from_stream(
        sv_bg_url
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("background")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("url")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| future::ready(match value {
                Value::Text(text, _) => Some(format!("url(\"{}\")", text.text())),
                _ => None,
            }))
    );

    // Size (shorthand for width + height)
    let size_signal = signal::from_stream(
        sv_size
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("size")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let border_radius_signal = signal::from_stream(
        sv5.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("rounded_corners")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    // Transform: move_right and move_down
    let transform_signal = signal::from_stream(
        sv6.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("transform")).flat_map(|var| var.stream_sync())
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
                if move_right != 0.0 || move_down != 0.0 {
                    Some(format!("translate({}px, {}px)", move_right, move_down))
                } else {
                    None
                }
            })
            .boxed_local()
    );

    // Size signal for height (duplicate for separate signal)
    let sv_size2 = tagged_object.expect_variable("settings");
    let size_for_height_signal = signal::from_stream(
        sv_size2
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("size")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

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
    let (hovered_sender, mut hovered_receiver) = mpsc::unbounded::<bool>();

    // Set up hovered link if element field exists with hovered property
    // Access element through settings, like other properties (style, direction, etc.)
    let sv_element_for_hover = tagged_object.expect_variable("settings");
    let hovered_stream = sv_element_for_hover
        .stream_sync()
        .flat_map(|value| {
            // Get element from settings object if it exists
            let obj = value.expect_object();
            stream::iter(obj.variable("element")).flat_map(|var| var.stream_sync())
        })
        .filter_map(|value| {
            let obj = value.expect_object();
            future::ready(obj.variable("hovered"))
        })
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending());

    let hovered_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let mut hovered_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
            let mut hovered_stream = hovered_stream.fuse();
            loop {
                select! {
                    new_sender = hovered_stream.next() => {
                        if let Some(sender) = new_sender {
                            hovered_link_value_sender = Some(sender);
                        }
                    }
                    is_hovered = hovered_receiver.select_next_some() => {
                        if let Some(sender) = hovered_link_value_sender.as_ref() {
                            let hover_tag = if is_hovered { "True" } else { "False" };
                            let event_value = EngineTag::new_value(
                                ConstructInfo::new("stripe::hovered", None, "Stripe hovered state"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                hover_tag,
                            );
                            if let Err(e) = sender.unbounded_send(event_value) {
                                zoon::println!("[DOM] stripe::hovered event send failed: {e}");
                            }
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    // NOTE: The flat_map closures below extract Objects/Variables from Values.
    // These are Arc-wrapped, so when we call `expect_variable()`, we get an Arc<Variable>
    // that stays alive independently of the parent Object. The stream keeps the Variable
    // alive for its lifetime, and signal::from_stream keeps the stream alive for the
    // element's lifetime. No Mutex needed.

    let direction_stream = settings_variable
        .clone()
        .stream_sync()
        .flat_map(|value| {
            let object = value.expect_object();
            // object.expect_variable returns Arc<Variable> which is kept alive by the stream
            object.expect_variable("direction").stream_sync()
        })
        .map(|direction| match direction.expect_tag().tag() {
            "Column" => Direction::Column,
            "Row" => Direction::Row,
            other => panic!("Invalid Stripe element direction value: Found: '{other}', Expected: 'Column' or 'Row'"),
        });

    let gap_stream = settings_variable
        .clone()
        .stream_sync()
        .flat_map(|value| {
            let object = value.expect_object();
            object.expect_variable("gap").stream_sync()
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            })
        });

    // Style property streams for element_stripe
    // Width with Fill and min/max support: Fill | number | [sizing: Fill, minimum: X, maximum: Y]
    let sv_width = tagged_object.expect_variable("settings");
    let width_signal = signal::from_stream(
        sv_width
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("width")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Min-width
    let sv_min_width = tagged_object.expect_variable("settings");
    let min_width_signal = signal::from_stream(
        sv_min_width
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("width")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Max-width
    let sv_max_width = tagged_object.expect_variable("settings");
    let max_width_signal = signal::from_stream(
        sv_max_width
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("width")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Height with Fill and minimum: Screen support
    let sv_height = tagged_object.expect_variable("settings");
    let height_signal = signal::from_stream(
        sv_height
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("height")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Min-height (supports Screen -> 100vh)
    let sv_min_height = tagged_object.expect_variable("settings");
    let min_height_signal = signal::from_stream(
        sv_min_height
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("height")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Background color
    let sv_bg = tagged_object.expect_variable("settings");
    let background_signal = signal::from_stream(
        sv_bg
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("background")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| oklch_to_css(value))
            .boxed_local()
    );

    // Padding (directional: [top, column, left, right, row, bottom])
    let sv_padding = tagged_object.expect_variable("settings");
    let padding_signal = signal::from_stream(
        sv_padding
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("padding")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Shadows (box-shadow from LIST of shadow objects)
    let sv_shadows = tagged_object.expect_variable("settings");
    let shadows_signal = signal::from_stream(
        sv_shadows
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("shadows")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Font size (cascading to children)
    let sv_font_size = tagged_object.expect_variable("settings");
    let font_size_signal = signal::from_stream(
        sv_font_size
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("size")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| {
                future::ready(match value {
                    Value::Number(n, _) => Some(format!("{}px", n.number())),
                    _ => None,
                })
            })
    );

    // Font color
    let sv_font_color = tagged_object.expect_variable("settings");
    let font_color_signal = signal::from_stream(
        sv_font_color
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| oklch_to_css(value))
            .boxed_local()
    );

    // Font weight
    let sv_font_weight = tagged_object.expect_variable("settings");
    let font_weight_signal = signal::from_stream(
        sv_font_weight
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("weight")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Font family
    let sv_font_family = tagged_object.expect_variable("settings");
    let font_family_signal = signal::from_stream(
        sv_font_family
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("family")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Font align (text-align)
    let sv_font_align = tagged_object.expect_variable("settings");
    let font_align_signal = signal::from_stream(
        sv_font_align
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("align")).flat_map(|var| var.stream_sync())
            })
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
    );

    // Borders (supports [top: [color: Oklch[...]]])
    let sv_borders = tagged_object.expect_variable("settings");
    let border_top_signal = signal::from_stream(
        sv_borders
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("borders")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("top")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| async move {
                if let Some(color) = oklch_to_css(value).await {
                    Some(format!("1px solid {}", color))
                } else {
                    None
                }
            })
            .boxed_local()
    );

    // Align (row: Center -> justify-content: center for Row, align-items: center for Column)
    let sv_align = tagged_object.expect_variable("settings");
    let align_items_signal = signal::from_stream(
        sv_align
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("align")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("row")).flat_map(|var| var.stream_sync())
            })
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
    );

    let items_vec_diff_stream = settings_variable
        .stream_sync()
        .flat_map(|value| {
            let object = value.expect_object();
            object.expect_variable("items").stream_sync()
        })
        .flat_map(|value| {
            // value.expect_list() returns Arc<List> which is kept alive by the stream
            value.expect_list().stream()
        })
        .map(list_change_to_vec_diff);

    Stripe::new()
        .direction_signal(signal::from_stream(direction_stream).map(Option::unwrap_or_default))
        .items_signal_vec(VecDiffStreamSignalVec(items_vec_diff_stream).map_signal(
            move |value_actor| {
                // value_actor is kept alive by the stream_sync() → signal::from_stream chain
                signal::from_stream(value_actor.stream_sync().map({
                    let construct_context = construct_context.clone();
                    move |value| value_to_element(value, construct_context.clone())
                }))
            },
        ))
        .on_hovered_change(move |is_hovered| {
            if let Err(e) = hovered_sender.unbounded_send(is_hovered) {
                zoon::println!("[DOM] hovered change event send failed: {e}");
            }
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

    let layers_vec_diff_stream = settings_variable
        .clone()
        .stream_sync()
        .flat_map(|value| {
            let object = value.expect_object();
            object.expect_variable("layers").stream_sync()
        })
        .flat_map(|value| {
            value.expect_list().stream()
        })
        .map(list_change_to_vec_diff);

    // Create individual style streams directly from settings
    let settings_variable_2 = tagged_object.expect_variable("settings");
    let settings_variable_3 = tagged_object.expect_variable("settings");
    let settings_variable_4 = tagged_object.expect_variable("settings");

    let width_signal = signal::from_stream(
        settings_variable_2.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("width")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let height_signal = signal::from_stream(
        settings_variable_3.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("height")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let background_signal = signal::from_stream(
        settings_variable_4.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("background")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| oklch_to_css(value))
            .boxed_local()
    );

    Stack::new()
        .update_raw_el(|raw_el| {
            raw_el
                .style_signal("width", width_signal)
                .style_signal("height", height_signal)
                .style_signal("background-color", background_signal)
        })
        .layers_signal_vec(VecDiffStreamSignalVec(layers_vec_diff_stream).map_signal(
            move |value_actor| {
                signal::from_stream(value_actor.stream_sync().map({
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
                // Helper to extract number from Variable's current value
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

                let lightness = get_num(&tagged, "lightness", 0.0).await;
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

fn element_button(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    type PressEvent = ();

    let (press_event_sender, mut press_event_receiver) = mpsc::unbounded::<PressEvent>();
    let (hovered_sender, mut hovered_receiver) = mpsc::unbounded::<bool>();

    let element_variable = tagged_object.expect_variable("element");

    // Set up press event handler - use same subscription pattern as text_input
    // Chain with pending() to prevent stream termination, which would cause busy-polling
    // in the select! loop (fused stream returns Ready(None) immediately when exhausted)
    let mut press_stream = element_variable
        .clone()
        .stream_sync()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.stream_sync())
        .filter_map(|value| future::ready(value.expect_object().variable("press")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    // Set up hovered link if element field exists with hovered property
    // Chain with pending() to prevent stream termination (same as press_stream)
    let hovered_stream = element_variable
        .stream_sync()
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending());

    let event_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let mut press_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
            let mut hovered_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
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
                            hovered_link_value_sender = Some(sender);
                        }
                    }
                    press_event = press_event_receiver.select_next_some() => {
                        if let Some(press_link_value_sender) = press_link_value_sender.as_ref() {
                            let press_event_object_value = Object::new_value(
                                ConstructInfo::new(format!("bridge::element_button::press_event, version: {press_event_object_value_version}"), None, "Button press event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                [],
                            );
                            press_event_object_value_version += 1;
                            if let Err(error) = press_link_value_sender.unbounded_send(press_event_object_value) {
                                zoon::eprintln!("Failed to send button press event to event press link variable: {error}");
                            }
                        }
                    }
                    is_hovered = hovered_receiver.select_next_some() => {
                        if let Some(sender) = hovered_link_value_sender.as_ref() {
                            let hover_tag = if is_hovered { "True" } else { "False" };
                            let event_value = EngineTag::new_value(
                                ConstructInfo::new("button::hovered", None, "Button hovered state"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                hover_tag,
                            );
                            if let Err(e) = sender.unbounded_send(event_value) {
                                zoon::println!("[DOM] button::hovered event send failed: {e}");
                            }
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    let label_stream = settings_variable
        .clone()
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("label").stream_sync())
        .map({
            let construct_context = construct_context.clone();
            move |value| value_to_element(value, construct_context.clone())
        });

    // Font size signal
    let sv_font_size = settings_variable.clone();
    let font_size_signal = signal::from_stream(
        sv_font_size
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("size")).flat_map(|var| var.stream_sync())
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
    );

    // Font color signal
    let sv_font_color = settings_variable.clone();
    let font_color_signal = signal::from_stream(
        sv_font_color
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| oklch_to_css(value))
            .boxed_local()
    );

    // Padding signal
    let sv_padding = settings_variable.clone();
    let padding_signal = signal::from_stream(
        sv_padding
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("padding")).flat_map(|var| var.stream_sync())
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
    );

    // Size (width) signal
    let sv_size_width = settings_variable.clone();
    let size_width_signal = signal::from_stream(
        sv_size_width
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("size")).flat_map(|var| var.stream_sync())
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
    );

    // Size (height) signal
    let sv_size_height = settings_variable.clone();
    let size_height_signal = signal::from_stream(
        sv_size_height
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("size")).flat_map(|var| var.stream_sync())
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
    );

    // Rounded corners signal
    let sv_rounded = settings_variable.clone();
    let rounded_signal = signal::from_stream(
        sv_rounded
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("rounded_corners")).flat_map(|var| var.stream_sync())
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
    );

    // Transform signal (move_left, move_down -> translate)
    let sv_transform = settings_variable.clone();
    let transform_signal = signal::from_stream(
        sv_transform
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("transform")).flat_map(|var| var.stream_sync())
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
                    // Calculate final x (negative for move_left, positive for move_right)
                    let x = move_right - move_left;
                    // Calculate final y (positive for move_down, negative for move_up)
                    let y = move_down - move_up;
                    if x != 0.0 || y != 0.0 {
                        Some(format!("translate({}px, {}px)", x, y))
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .boxed_local()
    );

    // Font align signal (text-align)
    let sv_font_align = settings_variable.clone();
    let font_align_signal = signal::from_stream(
        sv_font_align
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("align")).flat_map(|var| var.stream_sync())
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
    );

    // Outline signal
    let sv_outline = settings_variable.clone();
    let outline_signal = signal::from_stream(
        sv_outline
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("outline")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| async move {
                match value {
                    Value::Tag(tag, _) if tag.tag() == "NoOutline" => {
                        Some("none".to_string())
                    }
                    Value::Object(obj, _) => {
                        // Get color from outline object
                        if let Some(color_var) = obj.variable("color") {
                            if let Ok(color_value) = color_var.value_actor().current_value().await {
                                if let Some(css_color) = oklch_to_css(color_value).await {
                                    // Default to 1px solid outline
                                    return Some(format!("1px solid {}", css_color));
                                }
                            }
                        }
                        None
                    }
                    _ => None,
                }
            })
            .boxed_local()
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
            let press_event: PressEvent = ();
            if let Err(error) = press_event_sender.unbounded_send(press_event) {
                zoon::eprintln!("Failed to send button press event from on_press handler: {error}");
            }
        })
        .on_hovered_change(move |is_hovered| {
            if let Err(e) = hovered_sender.unbounded_send(is_hovered) {
                zoon::println!("[DOM] hovered change event send failed: {e}");
            }
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
    type ChangeEvent = String;
    type KeyDownEvent = String;
    type BlurEvent = ();

    let (change_event_sender, mut change_event_receiver) = mpsc::unbounded::<ChangeEvent>();
    let (key_down_event_sender, mut key_down_event_receiver) = mpsc::unbounded::<KeyDownEvent>();
    let (blur_event_sender, mut blur_event_receiver) = mpsc::unbounded::<BlurEvent>();

    let element_variable = tagged_object.expect_variable("element");

    // Set up event handlers - create separate subscriptions for each event type
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    let mut change_stream = element_variable
        .clone()
        .stream_sync()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.stream_sync())
        .filter_map(|value| future::ready(value.expect_object().variable("change")))
        .map(move |variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    let mut key_down_stream = element_variable
        .clone()
        .stream_sync()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.stream_sync())
        .filter_map(|value| future::ready(value.expect_object().variable("key_down")))
        .map(move |variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    let mut blur_stream = element_variable
        .stream_sync()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.stream_sync())
        .filter_map(|value| future::ready(value.expect_object().variable("blur")))
        .map(move |variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    let event_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let mut change_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
            let mut key_down_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
            let mut blur_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
            let mut pending_change_events: Vec<String> = Vec::new();

            loop {
                select! {
                    result = change_stream.next() => {
                        if let Some(sender) = result {
                            change_link_value_sender = Some(sender.clone());

                            // Flush any pending change events
                            for text in pending_change_events.drain(..) {
                                let event_value = Object::new_value(
                                    ConstructInfo::new("text_input::change_event", None, "TextInput change event"),
                                    construct_context.clone(),
                                    ValueIdempotencyKey::new(),
                                    [Variable::new_arc(
                                        ConstructInfo::new("text_input::change_event::text", None, "change text"),
                                        construct_context.clone(),
                                        "text",
                                        ValueActor::new_arc(
                                            ConstructInfo::new("text_input::change_event::text_actor", None, "change text actor"),
                                            ActorContext::default(),
                                            TypedStream::infinite(stream::once(future::ready(EngineText::new_value(
                                                ConstructInfo::new("text_input::change_event::text_value", None, "change text value"),
                                                construct_context.clone(),
                                                ValueIdempotencyKey::new(),
                                                text,
                                            ))).chain(stream::pending())),
                                            None,
                                        ),
                                        parser::PersistenceId::default(),
                                        parser::Scope::Root,
                                    )],
                                );
                                if let Err(e) = sender.unbounded_send(event_value) {
                                    zoon::println!("[DOM] text_input::hovered event send failed: {e}");
                                }
                            }
                        }
                    }
                    result = key_down_stream.next() => {
                        if let Some(sender) = result {
                            key_down_link_value_sender = Some(sender);
                        }
                    }
                    result = blur_stream.next() => {
                        if let Some(sender) = result {
                            blur_link_value_sender = Some(sender);
                        }
                    }
                    text = change_event_receiver.select_next_some() => {
                        if let Some(sender) = change_link_value_sender.as_ref() {
                            let event_value = Object::new_value(
                                ConstructInfo::new("text_input::change_event", None, "TextInput change event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                [Variable::new_arc(
                                    ConstructInfo::new("text_input::change_event::text", None, "change text"),
                                    construct_context.clone(),
                                    "text",
                                    ValueActor::new_arc(
                                        ConstructInfo::new("text_input::change_event::text_actor", None, "change text actor"),
                                        ActorContext::default(),
                                        // Already infinite via chain(pending())
                                        TypedStream::infinite(stream::once(future::ready(EngineText::new_value(
                                            ConstructInfo::new("text_input::change_event::text_value", None, "change text value"),
                                            construct_context.clone(),
                                            ValueIdempotencyKey::new(),
                                            text,
                                        ))).chain(stream::pending())),
                                        None,
                                    ),
                                    parser::PersistenceId::default(),
                                    parser::Scope::Root,
                                )],
                            );
                            if let Err(e) = sender.unbounded_send(event_value) {
                                zoon::println!("[DOM] text_input::change event send failed: {e}");
                            }
                        } else {
                            pending_change_events.push(text);
                        }
                    }
                    key = key_down_event_receiver.select_next_some() => {
                        if let Some(sender) = key_down_link_value_sender.as_ref() {
                            let event_value = Object::new_value(
                                ConstructInfo::new("text_input::key_down_event", None, "TextInput key_down event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                [Variable::new_arc(
                                    ConstructInfo::new("text_input::key_down_event::key", None, "key_down key"),
                                    construct_context.clone(),
                                    "key",
                                    ValueActor::new_arc(
                                        ConstructInfo::new("text_input::key_down_event::key_actor", None, "key_down key actor"),
                                        ActorContext::default(),
                                        // Already infinite via chain(pending())
                                        TypedStream::infinite(stream::once(future::ready(EngineTag::new_value(
                                            ConstructInfo::new("text_input::key_down_event::key_value", None, "key_down key value"),
                                            construct_context.clone(),
                                            ValueIdempotencyKey::new(),
                                            key.clone(),
                                        ))).chain(stream::pending())),
                                        None,
                                    ),
                                    parser::PersistenceId::default(),
                                    parser::Scope::Root,
                                )],
                            );
                            if let Err(e) = sender.unbounded_send(event_value) {
                                zoon::println!("[DOM] text_input::key_down event send failed: {e}");
                            }
                        }
                    }
                    _blur = blur_event_receiver.select_next_some() => {
                        if let Some(sender) = blur_link_value_sender.as_ref() {
                            let event_value = Object::new_value(
                                ConstructInfo::new("text_input::blur_event", None, "TextInput blur event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                [],
                            );
                            if let Err(e) = sender.unbounded_send(event_value) {
                                zoon::println!("[DOM] text_input::blur event send failed: {e}");
                            }
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    let text_stream = settings_variable
        .clone()
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("text").stream_sync())
        .filter_map(|value| {
            future::ready(match value {
                Value::Text(text, _) => Some(text.text().to_string()),
                _ => None,
            })
        });

    // Placeholder text stream - extract actual text from placeholder object
    let placeholder_text_stream = settings_variable
        .clone()
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("placeholder").stream_sync())
        .flat_map(|value| {
            match value {
                Value::Object(obj, _) => {
                    stream::iter(obj.variable("text")).flat_map(|var| var.stream_sync()).left_stream()
                }
                _ => stream::empty().right_stream(),
            }
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Text(text, _) => Some(text.text().to_string()),
                _ => None,
            })
        });

    // Placeholder signal for TextInput
    let placeholder_signal = signal::from_stream(placeholder_text_stream);

    // Width signal from style
    let sv_width = tagged_object.expect_variable("settings");
    let width_signal = signal::from_stream(
        sv_width
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("width")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| {
                future::ready(match value {
                    Value::Number(n, _) => Some(format!("{}px", n.number())),
                    Value::Tag(tag, _) if tag.tag() == "Fill" => Some("100%".to_string()),
                    _ => None,
                })
            }),
    );

    // Padding signal from style - supports simple number or directional object
    let sv_padding = tagged_object.expect_variable("settings");
    let padding_signal = signal::from_stream(
        sv_padding
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("padding")).flat_map(|var| var.stream_sync())
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
    );

    // Font size signal from style
    let sv_font_size = tagged_object.expect_variable("settings");
    let font_size_signal = signal::from_stream(
        sv_font_size
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("size")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| {
                future::ready(match value {
                    Value::Number(n, _) => Some(format!("{}px", n.number())),
                    _ => None,
                })
            })
    );

    // Font color signal from style
    let sv_font_color = tagged_object.expect_variable("settings");
    let font_color_signal = signal::from_stream(
        sv_font_color
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| oklch_to_css(value))
            .boxed_local()
    );

    // Background color signal from style
    let sv_bg_color = tagged_object.expect_variable("settings");
    let background_color_signal = signal::from_stream(
        sv_bg_color
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("background")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| oklch_to_css(value))
            .boxed_local()
    );

    // Focus signal - use Mutable with stream updates
    // Start with false, stream will set to true if focus: True is specified
    let focus_mutable = Mutable::new(false);
    let focus_signal = focus_mutable.signal();

    // Update the mutable when the stream emits
    let focus_stream = settings_variable
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("focus").stream_sync())
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
                if let Err(e) = sender.unbounded_send(text.clone()) {
                    zoon::println!("[DOM] text_input on_change event send failed: {e}");
                }
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
                if let Err(e) = sender.unbounded_send(key_name) {
                    zoon::println!("[DOM] text_input on_key_down event send failed: {e}");
                }
            }
        })
        .on_blur({
            let sender = blur_event_sender.clone();
            move || {
                if let Err(e) = sender.unbounded_send(()) {
                    zoon::println!("[DOM] text_input on_blur event send failed: {e}");
                }
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
            drop(event_handler_loop);
            drop(focus_loop);
        })
}

fn element_checkbox(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    type ClickEvent = ();

    let (click_event_sender, mut click_event_receiver) = mpsc::unbounded::<ClickEvent>();

    let element_variable = tagged_object.expect_variable("element");

    let event_stream = element_variable
        .stream_sync()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.stream_sync());

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
            let mut click_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
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
                                if let Err(e) = sender.unbounded_send(event_value) {
                                    zoon::println!("[DOM] checkbox pending click event send failed: {e}");
                                }
                            }
                            pending_clicks = 0;
                        }
                    }
                    _click = click_event_receiver.select_next_some() => {
                        if let Some(sender) = click_link_value_sender.as_ref() {
                            let event_value = Object::new_value(
                                ConstructInfo::new("checkbox::click_event", None, "Checkbox click event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                [],
                            );
                            if let Err(e) = sender.unbounded_send(event_value) {
                                zoon::println!("[DOM] checkbox click event send failed: {e}");
                            }
                        } else {
                            // Buffer the click to send when sender becomes available
                            pending_clicks += 1;
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    let checked_stream = settings_variable
        .clone()
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("checked").stream_sync())
        .filter_map(|value| {
            future::ready(match value {
                Value::Tag(tag, _) => Some(tag.tag() == "True"),
                _ => None,
            })
        });

    let icon_stream = settings_variable
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("icon").stream_sync())
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
                if let Err(e) = sender.unbounded_send(()) {
                    zoon::println!("[DOM] checkbox on_click event send failed: {e}");
                }
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
    type DoubleClickEvent = ();

    let (double_click_sender, mut double_click_receiver) = mpsc::unbounded::<DoubleClickEvent>();
    let (hovered_sender, _hovered_receiver) = mpsc::unbounded::<bool>();

    let element_variable = tagged_object.expect_variable("element");

    // Set up hovered link
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    let hovered_stream = element_variable
        .clone()
        .stream_sync()
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending());

    // Set up double_click event
    let event_stream = element_variable
        .stream_sync()
        .filter_map(move |value| {
            let obj = value.expect_object();
            future::ready(obj.variable("event"))
        })
        .flat_map(move |variable| variable.stream_sync());

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
            let mut double_click_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
            let mut _hovered_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
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
                            _hovered_link_value_sender = Some(sender);
                        }
                    }
                    _click = double_click_receiver.select_next_some() => {
                        if let Some(sender) = double_click_link_value_sender.as_ref() {
                            let event_value = Object::new_value(
                                ConstructInfo::new("label::double_click_event", None, "Label double_click event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                [],
                            );
                            if let Err(e) = sender.unbounded_send(event_value) {
                                zoon::println!("[DOM] label::double_click event send failed: {e}");
                            }
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    let label_stream = settings_variable
        .clone()
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("label").stream_sync())
        .map({
            let construct_context = construct_context.clone();
            move |value| value_to_element(value, construct_context.clone())
        });

    // Create style streams
    let sv2 = tagged_object.expect_variable("settings");
    let sv3 = tagged_object.expect_variable("settings");
    let sv4 = tagged_object.expect_variable("settings");

    let padding_signal = signal::from_stream(
        sv2.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("padding")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let font_color_signal = signal::from_stream(
        sv3.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| oklch_to_css(value))
            .boxed_local()
    );

    let font_size_signal = signal::from_stream(
        sv4.stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("size")).flat_map(|var| var.stream_sync())
            })
            .filter_map(|value| {
                future::ready(match value {
                    Value::Number(n, _) => Some(format!("{}px", n.number())),
                    _ => None,
                })
            })
    );

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
                if let Err(e) = sender.unbounded_send(()) {
                    zoon::println!("[DOM] label on_double_click event send failed: {e}");
                }
            }
        })
        .after_remove(move |_| drop(event_handler_loop))
}

fn element_paragraph(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let settings_variable = tagged_object.expect_variable("settings");

    let contents_vec_diff_stream = settings_variable
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("contents").stream_sync())
        .flat_map(|value| value.expect_list().stream())
        .map(list_change_to_vec_diff);

    Paragraph::new()
        .update_raw_el(|raw_el| {
            raw_el.style("white-space", "pre-wrap")
        })
        .contents_signal_vec(
            VecDiffStreamSignalVec(contents_vec_diff_stream).map_signal(move |value_actor| {
                signal::from_stream(value_actor.stream_sync().map({
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
    let (hovered_sender, mut hovered_receiver) = mpsc::unbounded::<bool>();

    let element_variable = tagged_object.expect_variable("element");

    // Set up hovered handler
    // Chain with pending() to prevent stream termination causing busy-polling in select!
    let mut hovered_stream = element_variable
        .stream_sync()
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender())
        .chain(stream::pending())
        .fuse();

    let event_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let mut hovered_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;

            loop {
                select! {
                    sender = hovered_stream.select_next_some() => {
                        hovered_link_value_sender = Some(sender);
                    }
                    is_hovered = hovered_receiver.select_next_some() => {
                        if let Some(sender) = hovered_link_value_sender.as_ref() {
                            let hover_tag = if is_hovered { "True" } else { "False" };
                            let event_value = EngineTag::new_value(
                                ConstructInfo::new("link::hovered", None, "Link hovered state"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                hover_tag,
                            );
                            if let Err(e) = sender.unbounded_send(event_value) {
                                zoon::println!("[DOM] link::hovered event send failed: {e}");
                            }
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    let label_stream = settings_variable
        .clone()
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("label").stream_sync())
        .map({
            let construct_context = construct_context.clone();
            move |value| value_to_element(value, construct_context.clone())
        });

    let sv_to = settings_variable.clone();
    let to_stream = sv_to
        .stream_sync()
        .flat_map(|value| value.expect_object().expect_variable("to").stream_sync())
        .filter_map(|value| {
            future::ready(match value {
                Value::Text(text, _) => Some(text.text().to_string()),
                _ => None,
            })
        });

    // Underline signal (font.line.underline)
    let sv_underline = settings_variable.clone();
    let underline_signal = signal::from_stream(
        sv_underline
            .stream_sync()
            .flat_map(|value| value.expect_object().expect_variable("style").stream_sync())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("line")).flat_map(|var| var.stream_sync())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("underline")).flat_map(|var| var.stream_sync())
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
    );

    Link::new()
        .label_signal(signal::from_stream(label_stream).map(|l| {
            l.unwrap_or_else(|| zoon::Text::new("").unify())
        }))
        .to_signal(signal::from_stream(to_stream).map(|t| t.unwrap_or_default()))
        .new_tab(NewTab::new())
        .on_hovered_change(move |is_hovered| {
            if let Err(e) = hovered_sender.unbounded_send(is_hovered) {
                zoon::println!("[DOM] hovered change event send failed: {e}");
            }
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

fn list_change_to_vec_diff(change: ListChange) -> VecDiff<Arc<ValueActor>> {
    match change {
        ListChange::Replace { items } => VecDiff::Replace { values: items },
        ListChange::InsertAt { index, item } => VecDiff::InsertAt { index, value: item },
        ListChange::UpdateAt { index, item } => VecDiff::UpdateAt { index, value: item },
        ListChange::RemoveAt { index } => VecDiff::RemoveAt { index },
        ListChange::Move {
            old_index,
            new_index,
        } => VecDiff::Move {
            old_index,
            new_index,
        },
        ListChange::Push { item } => VecDiff::Push { value: item },
        ListChange::Pop => VecDiff::Pop {},
        ListChange::Clear => VecDiff::Clear {},
    }
}

