use std::sync::Arc;

use zoon::futures_util::{future, select, stream, StreamExt};
use zoon::futures_channel::mpsc;
use zoon::{eprintln, *};

use super::engine::{
    ActorContext, ActorLoop, ConstructContext, ConstructInfo, ListChange, Object,
    TaggedObject, TypedStream, Value, ValueActor, ValueIdempotencyKey, Variable,
    Text as EngineText, Tag as EngineTag,
};

pub fn object_with_document_to_element_signal(
    root_object: Arc<Object>,
    construct_context: ConstructContext,
) -> impl Signal<Item = Option<RawElOrText>> {
    let document_variable = root_object.expect_variable("document").clone();
    let doc_actor = document_variable.value_actor();

    let element_stream = doc_actor.clone().subscribe()
        .flat_map(|value| {
            let document_object = value.expect_object();
            let root_element_var = document_object.expect_variable("root_element").clone();
            root_element_var.value_actor().clone().subscribe()
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
            eprintln!("Warning: Object value passed to element context - rendering as empty. Object has {} variables", obj.variables().len());
            El::new().unify()
        }
        Value::List(_list, _) => {
            // List can't be rendered as a single element - render as debug info
            eprintln!("Warning: List value passed to element context - rendering as empty");
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
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("child").subscribe())
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

    let padding_signal = signal::from_stream(
        sv7.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("padding")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let width_signal = signal::from_stream(
        sv2.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("width")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let height_signal = signal::from_stream(
        sv3.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("height")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let background_signal = signal::from_stream(
        sv4.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("background")).flat_map(|var| var.subscribe())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready(oklch_to_css(value)))
    );

    let border_radius_signal = signal::from_stream(
        sv5.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("rounded_corners")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    // Transform: move_right and move_down
    let transform_signal = signal::from_stream(
        sv6.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("transform")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready({
                let obj = value.expect_object();
                let move_right = obj.variable("move_right")
                    .and_then(|v| v.value_actor().stored_value())
                    .and_then(|val| match val {
                        Value::Number(n, _) => Some(n.number()),
                        _ => None,
                    })
                    .unwrap_or(0.0);
                let move_down = obj.variable("move_down")
                    .and_then(|v| v.value_actor().stored_value())
                    .and_then(|val| match val {
                        Value::Number(n, _) => Some(n.number()),
                        _ => None,
                    })
                    .unwrap_or(0.0);
                if move_right != 0.0 || move_down != 0.0 {
                    Some(format!("translate({}px, {}px)", move_right, move_down))
                } else {
                    None
                }
            }))
    );

    El::new()
        .update_raw_el(|raw_el| {
            raw_el
                .style_signal("padding", padding_signal)
                .style_signal("width", width_signal)
                .style_signal("height", height_signal)
                .style_signal("background-color", background_signal)
                .style_signal("border-radius", border_radius_signal)
                .style_signal("transform", transform_signal)
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
    let element_variable = tagged_object.variable("element");
    let hovered_stream = stream::iter(element_variable)
        .flat_map(|variable| variable.subscribe())
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender());

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
                            let _ = sender.unbounded_send(event_value);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    // In the flat_map closures below, the settings Object is extracted as a temporary
    // and dropped at the end of the closure. We need to keep the Object alive to prevent
    // its Variables (direction, style, items) from being dropped.
    // Solution: Store the Object when we first receive it and keep it in a shared cell.
    let settings_object: std::sync::Arc<std::sync::Mutex<Option<Arc<Object>>>> = Default::default();
    let settings_object_for_direction = settings_object.clone();
    let settings_object_for_gap = settings_object.clone();
    let settings_object_for_items = settings_object.clone();

    // Similarly, we need to keep the items List value alive to prevent the underlying
    // ValueActor ("Persistent list wrapper") from being dropped during flat_map processing.
    let items_list_value: std::sync::Arc<std::sync::Mutex<Option<Value>>> = Default::default();
    let items_list_value_for_stream = items_list_value.clone();

    // Keep item ValueActors alive - each item's FunctionCall actor needs to survive
    // so that its arguments (like List/map actors) stay alive for the element's lifetime.
    let item_actors: std::sync::Arc<std::sync::Mutex<Vec<Arc<ValueActor>>>> = Default::default();
    let item_actors_for_stream = item_actors.clone();

    let direction_stream = settings_variable
        .clone()
        .subscribe()
        .flat_map(move |value| {
            let object = value.expect_object();
            // Keep the Object alive by storing it
            *settings_object_for_direction.lock().unwrap() = Some(object.clone());
            object.expect_variable("direction").subscribe()
        })
        .map(|direction| match direction.expect_tag().tag() {
            "Column" => Direction::Column,
            "Row" => Direction::Row,
            other => panic!("Invalid Stripe element direction value: Found: '{other}', Expected: 'Column' or 'Row'"),
        });

    let gap_stream = settings_variable
        .clone()
        .subscribe()
        .flat_map(move |value| {
            let object = value.expect_object();
            *settings_object_for_gap.lock().unwrap() = Some(object.clone());
            object.expect_variable("gap").subscribe()
        })
        .filter_map(|value| {
            future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            })
        });

    let items_vec_diff_stream = settings_variable
        .subscribe()
        .flat_map(move |value| {
            let object = value.expect_object();
            // Keep the Object alive by storing it
            *settings_object_for_items.lock().unwrap() = Some(object.clone());
            object.expect_variable("items").subscribe()
        })
        .flat_map(move |value| {
            // Keep the Value alive to prevent its underlying structures from being dropped
            *items_list_value_for_stream.lock().unwrap() = Some(value.clone());
            value.expect_list().subscribe()
        })
        .map(list_change_to_vec_diff);

    Stripe::new()
        .direction_signal(signal::from_stream(direction_stream).map(Option::unwrap_or_default))
        .items_signal_vec(VecDiffStreamSignalVec(items_vec_diff_stream).map_signal(
            move |value_actor| {
                // Keep the value_actor alive for the lifetime of the element.
                // This ensures that nested elements (like inner stripes with List/map)
                // keep their argument actors alive.
                item_actors_for_stream.lock().unwrap().push(value_actor.clone());
                signal::from_stream(value_actor.subscribe().map({
                    let construct_context = construct_context.clone();
                    move |value| value_to_element(value, construct_context.clone())
                }))
            },
        ))
        .on_hovered_change(move |is_hovered| {
            let _ = hovered_sender.unbounded_send(is_hovered);
        })
        .update_raw_el(|raw_el| {
            raw_el.style_signal("gap", signal::from_stream(gap_stream))
        })
        // Keep tagged_object, settings_object, items_list_value, and item_actors alive for the lifetime of this element
        .after_remove(move |_| {
            drop(tagged_object);
            drop(settings_object);
            drop(items_list_value);
            drop(item_actors);
            drop(hovered_handler_loop);
        })
}

fn element_stack(
    tagged_object: Arc<TaggedObject>,
    construct_context: ConstructContext,
) -> impl Element {
    let settings_variable = tagged_object.expect_variable("settings");

    // Keep the settings Object alive to prevent its Variables from being dropped
    let settings_object: std::sync::Arc<std::sync::Mutex<Option<Arc<Object>>>> = Default::default();
    let settings_object_for_layers = settings_object.clone();

    // Keep the layers List value alive
    let layers_list_value: std::sync::Arc<std::sync::Mutex<Option<Value>>> = Default::default();
    let layers_list_value_for_stream = layers_list_value.clone();

    let layers_vec_diff_stream = settings_variable
        .clone()
        .subscribe()
        .flat_map(move |value| {
            let object = value.expect_object();
            *settings_object_for_layers.lock().unwrap() = Some(object.clone());
            object.expect_variable("layers").subscribe()
        })
        .flat_map(move |value| {
            *layers_list_value_for_stream.lock().unwrap() = Some(value.clone());
            value.expect_list().subscribe()
        })
        .map(list_change_to_vec_diff);

    // Create individual style streams directly from settings
    let settings_variable_2 = tagged_object.expect_variable("settings");
    let settings_variable_3 = tagged_object.expect_variable("settings");
    let settings_variable_4 = tagged_object.expect_variable("settings");

    let width_signal = signal::from_stream(
        settings_variable_2.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("width")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let height_signal = signal::from_stream(
        settings_variable_3.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("height")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let background_signal = signal::from_stream(
        settings_variable_4.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("background")).flat_map(|var| var.subscribe())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready(oklch_to_css(value)))
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
                signal::from_stream(value_actor.subscribe().map({
                    let construct_context = construct_context.clone();
                    move |value| value_to_element(value, construct_context.clone())
                }))
            },
        ))
        // Keep tagged_object, settings_object, and layers_list_value alive
        .after_remove(move |_| {
            drop(tagged_object);
            drop(settings_object);
            drop(layers_list_value);
        })
}

/// Convert color value to CSS color string
/// Handles both Oklch[...] tagged objects and plain color tags like White, Black, etc.
fn oklch_to_css(value: Value) -> Option<String> {
    match value {
        Value::TaggedObject(tagged, _) => {
            if tagged.tag() == "Oklch" {
                // Helper to extract number from Variable's stored value
                let get_num = |name: &str, default: f64| -> f64 {
                    tagged.variable(name)
                        .and_then(|v| v.value_actor().stored_value())
                        .and_then(|val| match val {
                            Value::Number(n, _) => Some(n.number()),
                            _ => None,
                        })
                        .unwrap_or(default)
                };

                let lightness = get_num("lightness", 0.0);
                let chroma = get_num("chroma", 0.0);
                let hue = get_num("hue", 0.0);
                let alpha = get_num("alpha", 1.0);

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

    let element_variable = tagged_object.expect_variable("element");

    // Set up press event handler - use same subscription pattern as text_input
    let mut press_stream = element_variable
        .clone()
        .subscribe()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.subscribe())
        .filter_map(|value| future::ready(value.expect_object().variable("press")))
        .map(|variable| variable.expect_link_value_sender())
        .fuse();

    let press_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let mut press_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
            let mut press_event_object_value_version = 0u64;
            loop {
                select! {
                    new_press_link_value_sender = press_stream.next() => {
                        if let Some(new_press_link_value_sender) = new_press_link_value_sender {
                            zoon::println!("[BRIDGE] event_handler: press LINK sender set up (ready to receive events)");
                            press_link_value_sender = Some(new_press_link_value_sender);
                        } else {
                            break
                        }
                    }
                    press_event = press_event_receiver.select_next_some() => {
                        if let Some(press_link_value_sender) = press_link_value_sender.as_ref() {
                            zoon::println!("[BRIDGE] event_handler: Received press event from MoonZoon");
                            let press_event_object_value = Object::new_value(
                                ConstructInfo::new(format!("bridge::element_button::press_event, version: {press_event_object_value_version}"), None, "Button press event"),
                                construct_context.clone(),
                                ValueIdempotencyKey::new(),
                                [],
                            );
                            press_event_object_value_version += 1;
                            let result = press_link_value_sender.unbounded_send(press_event_object_value);
                            zoon::println!("[BRIDGE] event_handler: Sent press event to Boon engine LINK, result: {}", result.is_ok());
                            if let Err(error) = result {
                                eprintln!("Failed to send button press event to event press link variable: {error}");
                            }
                        } else {
                            zoon::println!("[BRIDGE] event_handler: Press event received but no LINK sender set up yet");
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    let label_stream = settings_variable
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("label").subscribe())
        .map(move |value| value_to_element(value, construct_context.clone()));

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
                eprintln!("Failed to send button press event from on_press handler: {error}");
            }
        })
        .after_remove(move |_| drop(press_handler_loop))
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

    // Set up event handlers - create separate subscriptions
    let mut change_stream = element_variable
        .clone()
        .subscribe()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.subscribe())
        .filter_map(|value| future::ready(value.expect_object().variable("change")))
        .map(|variable| variable.expect_link_value_sender())
        .fuse();

    let mut key_down_stream = element_variable
        .clone()
        .subscribe()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.subscribe())
        .filter_map(|value| future::ready(value.expect_object().variable("key_down")))
        .map(|variable| variable.expect_link_value_sender())
        .fuse();

    let mut blur_stream = element_variable
        .subscribe()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.subscribe())
        .filter_map(|value| future::ready(value.expect_object().variable("blur")))
        .map(|variable| variable.expect_link_value_sender())
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
                    new_sender = change_stream.next() => {
                        if let Some(sender) = new_sender {
                            zoon::println!("[BRIDGE] event_handler: change LINK sender set up (ready to receive events)");
                            change_link_value_sender = Some(sender.clone());
                            // Flush any pending change events
                            for text in pending_change_events.drain(..) {
                                zoon::println!("[BRIDGE] event_handler: Flushing buffered change event: '{}'", text);
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
                                        None,
                                    )],
                                );
                                let _ = sender.unbounded_send(event_value);
                            }
                        }
                    }
                    new_sender = key_down_stream.next() => {
                        if let Some(sender) = new_sender {
                            zoon::println!("[BRIDGE] event_handler: key_down LINK sender set up (ready to receive events)");
                            key_down_link_value_sender = Some(sender);
                        }
                    }
                    new_sender = blur_stream.next() => {
                        if let Some(sender) = new_sender {
                            blur_link_value_sender = Some(sender);
                        }
                    }
                    text = change_event_receiver.select_next_some() => {
                        zoon::println!("[BRIDGE] event_handler: Received change event with text: '{}'", text);
                        if let Some(sender) = change_link_value_sender.as_ref() {
                            zoon::println!("[BRIDGE] event_handler: LINK sender exists, sending change event");
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
                                    None,
                                )],
                            );
                            let _ = sender.unbounded_send(event_value);
                        } else {
                            zoon::println!("[BRIDGE] event_handler: LINK not ready, buffering change event");
                            pending_change_events.push(text);
                        }
                    }
                    key = key_down_event_receiver.select_next_some() => {
                        zoon::println!("[BRIDGE] event_handler: Received key '{}' from channel (from MoonZoon)", key);
                        if let Some(sender) = key_down_link_value_sender.as_ref() {
                            zoon::println!("[BRIDGE] event_handler: LINK sender exists, creating event object for key '{}'", key);
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
                                    None,
                                )],
                            );
                            let send_result = sender.unbounded_send(event_value);
                            zoon::println!("[BRIDGE] event_handler: Sent key '{}' event to Boon engine LINK, result: {:?}", key, send_result.is_ok());
                        } else {
                            zoon::println!("[BRIDGE] event_handler: WARNING - key '{}' received but NO LINK sender set up!", key);
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
                            let _ = sender.unbounded_send(event_value);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    let text_stream = settings_variable
        .clone()
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("text").subscribe())
        .filter_map(|value| {
            future::ready(match value {
                Value::Text(text, _) => Some(text.text().to_string()),
                _ => None,
            })
        });

    // Placeholder text stream - extract actual text from placeholder object
    let placeholder_text_stream = settings_variable
        .clone()
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("placeholder").subscribe())
        .flat_map(|value| {
            match value {
                Value::Object(obj, _) => {
                    stream::iter(obj.variable("text")).flat_map(|var| var.subscribe()).left_stream()
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
    let width_signal = signal::from_stream(
        settings_variable
            .clone()
            .subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("width")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| {
                future::ready(match value {
                    Value::Number(n, _) => Some(format!("{}px", n.number())),
                    Value::Tag(tag, _) if tag.tag() == "Fill" => Some("100%".to_string()),
                    _ => None,
                })
            }),
    );

    // Focus signal - use Mutable with stream updates
    // Start with false, stream will set to true if focus: True is specified
    let focus_mutable = Mutable::new(false);
    let focus_signal = focus_mutable.signal();

    // Update the mutable when the stream emits
    let focus_stream = settings_variable
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("focus").subscribe())
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
                let _ = sender.unbounded_send(text);
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
                zoon::println!("[BRIDGE] on_key_down_event: MoonZoon received key '{}' from DOM", key_name);
                let send_result = sender.unbounded_send(key_name.clone());
                zoon::println!("[BRIDGE] on_key_down_event: Sent '{}' to channel, result: {:?}", key_name, send_result.is_ok());
            }
        })
        .on_blur({
            let sender = blur_event_sender.clone();
            move || {
                let _ = sender.unbounded_send(());
            }
        })
        .focus_signal(focus_signal)
        .update_raw_el(|raw_el| {
            raw_el
                .style("padding", "8px 12px")
                .style("border", "1px solid #555")
                .style("border-radius", "4px")
                .style("background-color", "#2a2a2a")
                .style("color", "#fff")
                .style("font-size", "14px")
                .style("box-sizing", "border-box")
                .style_signal("width", width_signal)
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
        .subscribe()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.subscribe());

    let mut click_stream = event_stream
        .filter_map(|value| future::ready(value.expect_object().variable("click")))
        .map(|variable| variable.expect_link_value_sender())
        .fuse();

    let event_handler_loop = ActorLoop::new({
        let construct_context = construct_context.clone();
        async move {
            let mut click_link_value_sender: Option<mpsc::UnboundedSender<Value>> = None;
            loop {
                select! {
                    new_sender = click_stream.next() => {
                        if let Some(sender) = new_sender {
                            click_link_value_sender = Some(sender);
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
                            let _ = sender.unbounded_send(event_value);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    let checked_stream = settings_variable
        .clone()
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("checked").subscribe())
        .filter_map(|value| {
            future::ready(match value {
                Value::Tag(tag, _) => Some(tag.tag() == "True"),
                _ => None,
            })
        });

    let icon_stream = settings_variable
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("icon").subscribe())
        .map(move |value| value_to_element(value, construct_context.clone()));

    Checkbox::new()
        .label_hidden("checkbox")
        .checked_signal(signal::from_stream(checked_stream).map(|c| c.unwrap_or(false)))
        .icon(move |_checked_mutable| {
            // For now, just use the icon from settings
            El::new()
        })
        .on_click({
            let sender = click_event_sender.clone();
            move || {
                let _ = sender.unbounded_send(());
            }
        })
        .after_remove(move |_| drop(event_handler_loop))
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
    let hovered_stream = element_variable
        .clone()
        .subscribe()
        .filter_map(|value| future::ready(value.expect_object().variable("hovered")))
        .map(|variable| variable.expect_link_value_sender());

    // Set up double_click event
    let event_stream = element_variable
        .subscribe()
        .filter_map(|value| future::ready(value.expect_object().variable("event")))
        .flat_map(|variable| variable.subscribe());

    let mut double_click_stream = event_stream
        .filter_map(|value| future::ready(value.expect_object().variable("double_click")))
        .map(|variable| variable.expect_link_value_sender())
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
                            let _ = sender.unbounded_send(event_value);
                        }
                    }
                }
            }
        }
    });

    let settings_variable = tagged_object.expect_variable("settings");

    let label_stream = settings_variable
        .clone()
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("label").subscribe())
        .map({
            let construct_context = construct_context.clone();
            move |value| value_to_element(value, construct_context.clone())
        });

    // Create style streams
    let sv2 = tagged_object.expect_variable("settings");
    let sv3 = tagged_object.expect_variable("settings");

    let padding_signal = signal::from_stream(
        sv2.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("padding")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready(match value {
                Value::Number(n, _) => Some(format!("{}px", n.number())),
                _ => None,
            }))
    );

    let font_color_signal = signal::from_stream(
        sv3.subscribe()
            .flat_map(|value| value.expect_object().expect_variable("style").subscribe())
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("font")).flat_map(|var| var.subscribe())
            })
            .flat_map(|value| {
                let obj = value.expect_object();
                stream::iter(obj.variable("color")).flat_map(|var| var.subscribe())
            })
            .filter_map(|value| future::ready(oklch_to_css(value)))
    );

    Label::new()
        .update_raw_el(|raw_el| {
            raw_el
                .style_signal("padding", padding_signal)
                .style_signal("color", font_color_signal)
        })
        .label_signal(signal::from_stream(label_stream).map(|l| {
            l.unwrap_or_else(|| zoon::Text::new("").unify())
        }))
        .on_double_click({
            let sender = double_click_sender.clone();
            move || {
                let _ = sender.unbounded_send(());
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
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("contents").subscribe())
        .flat_map(|value| value.expect_list().subscribe())
        .map(list_change_to_vec_diff);

    Paragraph::new().contents_signal_vec(
        VecDiffStreamSignalVec(contents_vec_diff_stream).map_signal(move |value_actor| {
            signal::from_stream(value_actor.subscribe().map({
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
    let settings_variable = tagged_object.expect_variable("settings");

    let label_stream = settings_variable
        .clone()
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("label").subscribe())
        .map(move |value| value_to_element(value, construct_context.clone()));

    let to_stream = settings_variable
        .subscribe()
        .flat_map(|value| value.expect_object().expect_variable("to").subscribe())
        .filter_map(|value| {
            future::ready(match value {
                Value::Text(text, _) => Some(text.text().to_string()),
                _ => None,
            })
        });

    Link::new()
        .label_signal(signal::from_stream(label_stream).map(|l| {
            l.unwrap_or_else(|| zoon::Text::new("").unify())
        }))
        .to_signal(signal::from_stream(to_stream).map(|t| t.unwrap_or_default()))
        .new_tab(NewTab::new())
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
