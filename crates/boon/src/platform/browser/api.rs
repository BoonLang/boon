use std::future;
use std::sync::Arc;

use zoon::Timer;
use zoon::futures_util::{select, stream::{self, LocalBoxStream, Stream, StreamExt}, FutureExt};
use zoon::futures_channel::mpsc;
use zoon::{Deserialize, Serialize, serde};
use zoon::{window, history, Closure, JsValue, UnwrapThrowExt, JsCast, SendWrapper};

use super::engine::*;

use crate::parser::PersistenceId;

// @TODO make sure Values are deduplicated everywhere it makes sense

/// ```text
/// Document/new(root<INTO_ELEMENT>) -> [root_element<INTO_ELEMENT>]
/// INTO_ELEMENT: <ELEMENT | Text | Number>
/// ELEMENT: <
///     | ELEMENT_CONTAINER
///     | ELEMENT_STRIPE
///     | ELEMENT_BUTTON
/// >
/// ELEMENT_CONTAINER: ElementContainer[
///     settings<[
///         style<[]>
///         child<INTO_ELEMENT>
///     ]>
/// ]
/// ELEMENT_STRIPE: ElementStripe[
///     settings<[
///         direction<Column | Row>
///         style<[]>
///         items<List<INTO_ELEMENT>>
///     ]>
/// ]
/// ELEMENT_BUTTON: ElementButton[
///     event?<[
///         press?<LINK<[]>>
///     ]>
///     settings<[
///         style<[]>
///         label<INTO_ELEMENT>
///     ]>
/// ]
/// >
/// ```
pub fn function_document_new(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_root] = arguments.as_slice() else {
        panic!("Unexpected argument count")
    };
    let scoped_id = function_call_persistence_id;
    Object::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Document/new(..) -> [..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        [Variable::new_arc(
            ConstructInfo::new(
                function_call_id.with_child_id(1),
                None,
                "Document/new(..) -> [root_element]",
            ),
            construct_context,
            "root_element",
            argument_root.clone(),
            scoped_id.with_child_index(1),
            actor_context.scope.clone(),
        )],
    )
}

/// ```text
/// Element/stripe(
///     element<[]>
///     direction<Column | Row>
///     gap<Number>
///     style<[]>
///     items<List<INTO_ELEMENT>>
/// ) -> ELEMENT_STRIPE
/// ```
pub fn function_element_stripe(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_direction, argument_gap, argument_style, argument_items] =
        arguments.as_slice() else {
            panic!("Element/stripe requires 5 arguments, got {}", arguments.len());
        };
    let scoped_id = function_call_persistence_id;
    TaggedObject::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/stripe(..) -> ElementStripe[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementStripe",
        [Variable::new_arc(
            ConstructInfo::new(
                function_call_id.with_child_id(1),
                None,
                "Element/stripe(..) -> ElementStripe[settings]",
            ),
            construct_context.clone(),
            "settings",
            Object::new_arc_value_actor(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "Element/stripe(..) -> ElementStripe[settings: [..]]",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                actor_context.clone(),
                [
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(7),
                            None,
                            "Element/stripe(..) -> ElementStripe[settings: [element]]",
                        ),
                        construct_context.clone(),
                        "element",
                        argument_element.clone(),
                        scoped_id.with_child_index(7),
                        actor_context.scope.clone(),
                    ),
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(3),
                            None,
                            "Element/stripe(..) -> ElementStripe[settings: [direction]]",
                        ),
                        construct_context.clone(),
                        "direction",
                        argument_direction.clone(),
                        scoped_id.with_child_index(3),
                        actor_context.scope.clone(),
                    ),
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(6),
                            None,
                            "Element/stripe(..) -> ElementStripe[settings: [gap]]",
                        ),
                        construct_context.clone(),
                        "gap",
                        argument_gap.clone(),
                        scoped_id.with_child_index(6),
                        actor_context.scope.clone(),
                    ),
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(4),
                            None,
                            "Element/stripe(..) -> ElementStripe[settings: [style]]",
                        ),
                        construct_context.clone(),
                        "style",
                        argument_style.clone(),
                        scoped_id.with_child_index(4),
                        actor_context.scope.clone(),
                    ),
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(5),
                            None,
                            "Element/stripe(..) -> ElementStripe[settings: [items]]",
                        ),
                        construct_context,
                        "items",
                        argument_items.clone(),
                        scoped_id.with_child_index(5),
                        actor_context.scope.clone(),
                    ),
                ],
            ),
            scoped_id.with_child_index(1),
            actor_context.scope,
        )],
    )
}

/// ```text
/// Element/container(
///     element<[tag?: Tag]>
///     style<[]>
///     child<INTO_ELEMENT>
/// ) -> ELEMENT_CONTAINER
/// ```
pub fn function_element_container(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_child] = arguments.as_slice() else {
        panic!("Element/container expects 3 arguments")
    };
    let scoped_id = function_call_persistence_id;
    TaggedObject::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/container(..) -> ElementContainer[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementContainer",
        [Variable::new_arc(
            ConstructInfo::new(
                function_call_id.with_child_id(1),
                None,
                "Element/container(..) -> ElementContainer[settings]",
            ),
            construct_context.clone(),
            "settings",
            Object::new_arc_value_actor(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "Element/container(..) -> ElementContainer[settings: [..]]",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                actor_context.clone(),
                [
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(5),
                            None,
                            "Element/container(..) -> ElementContainer[settings: [element]]",
                        ),
                        construct_context.clone(),
                        "element",
                        argument_element.clone(),
                        scoped_id.with_child_index(5),
                        actor_context.scope.clone(),
                    ),
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(3),
                            None,
                            "Element/container(..) -> ElementContainer[settings: [style]]",
                        ),
                        construct_context.clone(),
                        "style",
                        argument_style.clone(),
                        scoped_id.with_child_index(3),
                        actor_context.scope.clone(),
                    ),
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(4),
                            None,
                            "Element/container(..) -> ElementContainer[settings: [child]]",
                        ),
                        construct_context,
                        "child",
                        argument_child.clone(),
                        scoped_id.with_child_index(4),
                        actor_context.scope.clone(),
                    ),
                ],
            ),
            scoped_id.with_child_index(1),
            actor_context.scope,
        )],
    )
}

/// ```text
/// Element/stack(
///     element<[tag?: Tag]>
///     style<[]>
///     layers<List<INTO_ELEMENT>>
/// ) -> ELEMENT_STACK
/// ```
pub fn function_element_stack(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_layers] = arguments.as_slice() else {
        panic!("Element/stack requires 3 arguments, got {}", arguments.len());
    };
    let scoped_id = function_call_persistence_id;
    TaggedObject::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/stack(..) -> ElementStack[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementStack",
        [Variable::new_arc(
            ConstructInfo::new(
                function_call_id.with_child_id(1),
                None,
                "Element/stack(..) -> ElementStack[settings]",
            ),
            construct_context.clone(),
            "settings",
            Object::new_arc_value_actor(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "Element/stack(..) -> ElementStack[settings: [..]]",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                actor_context.clone(),
                [
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(5),
                            None,
                            "Element/stack(..) -> ElementStack[settings: [element]]",
                        ),
                        construct_context.clone(),
                        "element",
                        argument_element.clone(),
                        scoped_id.with_child_index(5),
                        actor_context.scope.clone(),
                    ),
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(3),
                            None,
                            "Element/stack(..) -> ElementStack[settings: [style]]",
                        ),
                        construct_context.clone(),
                        "style",
                        argument_style.clone(),
                        scoped_id.with_child_index(3),
                        actor_context.scope.clone(),
                    ),
                    Variable::new_arc(
                        ConstructInfo::new(
                            function_call_id.with_child_id(4),
                            None,
                            "Element/stack(..) -> ElementStack[settings: [layers]]",
                        ),
                        construct_context,
                        "layers",
                        argument_layers.clone(),
                        scoped_id.with_child_index(4),
                        actor_context.scope.clone(),
                    ),
                ],
            ),
            scoped_id.with_child_index(1),
            actor_context.scope,
        )],
    )
}

/// ```text
/// Element/button(
///     element<[
///         event?<[
///             press?<LINK<[]>>
///         ]>
///     ]>
///     style<[]>
///     label<INTO_ELEMENT>
/// ) -> ELEMENT_BUTTON
/// ```
pub fn function_element_button(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_label] = arguments.as_slice() else {
        panic!("Unexpected argument count")
    };
    let scoped_id = function_call_persistence_id;
    // Create a derived actor that extracts `event` from argument_element
    // This allows direct access via `.event` instead of `.element.event`
    let event_stream = argument_element
        .clone()
        .stream_sync()
        .filter_map(|value| {
            // value is Object [event: [...]]
            future::ready(value.expect_object().variable("event"))
        })
        .then(|event_variable| async move {
            // Subscribe to get the event object values [press: LINK]
            event_variable.stream().await
        })
        .flatten();

    let event_actor = ValueActor::new_arc(
        ConstructInfo::new(
            function_call_id.with_child_id(7),
            None,
            "ElementButton[event] (derived)",
        ),
        actor_context.clone(),
        TypedStream::infinite(event_stream.chain(stream::pending())),
        None,
    );

    TaggedObject::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/button(..) -> ElementButton[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementButton",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "ElementButton[element]",
                ),
                construct_context.clone(),
                "element",
                argument_element.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "ElementButton[event]",
                ),
                construct_context.clone(),
                "event",
                event_actor,
                scoped_id.with_child_index(2),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(3),
                    None,
                    "Element/stripe(..) -> ElementButton[settings]",
                ),
                construct_context.clone(),
                "settings",
                Object::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(4),
                        None,
                        "Element/stripe(..) -> ElementButton[settings: [..]]",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    [
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(5),
                                None,
                                "Element/stripe(..) -> ElementButton[settings: [style]]",
                            ),
                            construct_context.clone(),
                            "style",
                            argument_style.clone(),
                            scoped_id.with_child_index(5),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(6),
                                None,
                                "Element/stripe(..) -> ElementButton[settings: [label]]",
                            ),
                            construct_context,
                            "label",
                            argument_label.clone(),
                            scoped_id.with_child_index(6),
                            actor_context.scope.clone(),
                        ),
                    ],
                ),
                scoped_id.with_child_index(3),
                actor_context.scope,
            ),
        ],
    )
}

/// ```text
/// Element/text_input(
///     element<[event?<[change?: LINK, key_down?: LINK, blur?: LINK]>]>
///     style<[]>
///     label<Hidden[text: Text] | ...>
///     text<Text>
///     placeholder<[style?: [], text?: Text]>
///     focus<Bool>
/// ) -> ELEMENT_TEXT_INPUT
/// ```
pub fn function_element_text_input(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_label, argument_text, argument_placeholder, argument_focus] =
        arguments.as_slice()
    else {
        panic!("Element/text_input expects 6 arguments")
    };
    let scoped_id = function_call_persistence_id;

    // Create a derived actor that extracts `event` from argument_element
    // This allows direct access via `.event` instead of `.element.event`
    let argument_element_for_event = argument_element.clone();
    let event_stream = stream::once(async move {
        argument_element_for_event
            .stream().await
            .filter_map(|value| {
                // value is Object [event: [...]]
                future::ready(value.expect_object().variable("event"))
            })
            .then(|event_variable| async move {
                // Subscribe to get the event object values [change: LINK, key_down: LINK]
                event_variable.stream().await
            })
            .flatten()
    }).flatten();

    let event_actor = ValueActor::new_arc(
        ConstructInfo::new(
            function_call_id.with_child_id(9),
            None,
            "ElementTextInput[event] (derived)",
        ),
        actor_context.clone(),
        TypedStream::infinite(event_stream.chain(stream::pending())),
        None,
    );

    TaggedObject::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/text_input(..) -> ElementTextInput[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementTextInput",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "ElementTextInput[element]",
                ),
                construct_context.clone(),
                "element",
                argument_element.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(10),
                    None,
                    "ElementTextInput[event]",
                ),
                construct_context.clone(),
                "event",
                event_actor,
                scoped_id.with_child_index(10),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(11),
                    None,
                    "ElementTextInput[text]",
                ),
                construct_context.clone(),
                "text",
                argument_text.clone(),
                scoped_id.with_child_index(11),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "ElementTextInput[settings]",
                ),
                construct_context.clone(),
                "settings",
                Object::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(3),
                        None,
                        "ElementTextInput[settings: [..]]",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    [
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(4),
                                None,
                                "ElementTextInput[settings: [style]]",
                            ),
                            construct_context.clone(),
                            "style",
                            argument_style.clone(),
                            scoped_id.with_child_index(4),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(5),
                                None,
                                "ElementTextInput[settings: [label]]",
                            ),
                            construct_context.clone(),
                            "label",
                            argument_label.clone(),
                            scoped_id.with_child_index(5),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(6),
                                None,
                                "ElementTextInput[settings: [text]]",
                            ),
                            construct_context.clone(),
                            "text",
                            argument_text.clone(),
                            scoped_id.with_child_index(6),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(7),
                                None,
                                "ElementTextInput[settings: [placeholder]]",
                            ),
                            construct_context.clone(),
                            "placeholder",
                            argument_placeholder.clone(),
                            scoped_id.with_child_index(7),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(8),
                                None,
                                "ElementTextInput[settings: [focus]]",
                            ),
                            construct_context,
                            "focus",
                            argument_focus.clone(),
                            scoped_id.with_child_index(8),
                            actor_context.scope.clone(),
                        ),
                    ],
                ),
                scoped_id.with_child_index(2),
                actor_context.scope,
            ),
        ],
    )
}

/// ```text
/// Element/checkbox(
///     element<[event?<[click?: LINK]>]>
///     style<[]>
///     label<Hidden[text: Text] | Reference[element: ...] | ...>
///     checked<Bool>
///     icon<Element>
/// ) -> ELEMENT_CHECKBOX
/// ```
pub fn function_element_checkbox(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_label, argument_checked, argument_icon] =
        arguments.as_slice()
    else {
        panic!("Element/checkbox expects 5 arguments")
    };
    let scoped_id = function_call_persistence_id;

    // Create a derived actor that extracts `event` from argument_element
    // This allows direct access via `.event` instead of `.element.event`
    let argument_element_for_event = argument_element.clone();
    let event_stream = stream::once(async move {
        argument_element_for_event
            .stream().await
            .filter_map(|value| {
                // value is Object [event: [...]]
                future::ready(value.expect_object().variable("event"))
            })
            .then(|event_variable| async move {
                // Subscribe to get the event object values [click: LINK]
                event_variable.stream().await
            })
            .flatten()
    }).flatten();

    let event_actor = ValueActor::new_arc(
        ConstructInfo::new(
            function_call_id.with_child_id(8),
            None,
            "ElementCheckbox[event] (derived)",
        ),
        actor_context.clone(),
        TypedStream::infinite(event_stream.chain(stream::pending())),
        None,
    );

    TaggedObject::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/checkbox(..) -> ElementCheckbox[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementCheckbox",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "ElementCheckbox[element]",
                ),
                construct_context.clone(),
                "element",
                argument_element.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(9),
                    None,
                    "ElementCheckbox[event]",
                ),
                construct_context.clone(),
                "event",
                event_actor,
                scoped_id.with_child_index(9),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "ElementCheckbox[settings]",
                ),
                construct_context.clone(),
                "settings",
                Object::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(3),
                        None,
                        "ElementCheckbox[settings: [..]]",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    [
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(4),
                                None,
                                "ElementCheckbox[settings: [style]]",
                            ),
                            construct_context.clone(),
                            "style",
                            argument_style.clone(),
                            scoped_id.with_child_index(4),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(5),
                                None,
                                "ElementCheckbox[settings: [label]]",
                            ),
                            construct_context.clone(),
                            "label",
                            argument_label.clone(),
                            scoped_id.with_child_index(5),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(6),
                                None,
                                "ElementCheckbox[settings: [checked]]",
                            ),
                            construct_context.clone(),
                            "checked",
                            argument_checked.clone(),
                            scoped_id.with_child_index(6),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(7),
                                None,
                                "ElementCheckbox[settings: [icon]]",
                            ),
                            construct_context,
                            "icon",
                            argument_icon.clone(),
                            scoped_id.with_child_index(7),
                            actor_context.scope.clone(),
                        ),
                    ],
                ),
                scoped_id.with_child_index(2),
                actor_context.scope,
            ),
        ],
    )
}

/// ```text
/// Element/label(
///     element<[event?<[double_click?: LINK]>, hovered?: LINK, nearby_element?: ...]>
///     style<[]>
///     label<Text | Element>
/// ) -> ELEMENT_LABEL
/// ```
pub fn function_element_label(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_label] = arguments.as_slice() else {
        panic!("Element/label expects 3 arguments")
    };
    let scoped_id = function_call_persistence_id;

    // Create a derived actor that extracts `event` from argument_element
    // This allows direct access via `.event` instead of `.element.event`
    let event_stream = argument_element
        .clone()
        .stream_sync()
        .filter_map(|value| {
            // value is Object [event: [...]]
            future::ready(value.expect_object().variable("event"))
        })
        .then(|event_variable| async move {
            // Subscribe to get the event object values [double_click: LINK]
            event_variable.stream().await
        })
        .flatten();

    let event_actor = ValueActor::new_arc(
        ConstructInfo::new(
            function_call_id.with_child_id(6),
            None,
            "ElementLabel[event] (derived)",
        ),
        actor_context.clone(),
        TypedStream::infinite(event_stream.chain(stream::pending())),
        None,
    );

    TaggedObject::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/label(..) -> ElementLabel[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementLabel",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "ElementLabel[element]",
                ),
                construct_context.clone(),
                "element",
                argument_element.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(7),
                    None,
                    "ElementLabel[event]",
                ),
                construct_context.clone(),
                "event",
                event_actor,
                scoped_id.with_child_index(7),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "ElementLabel[settings]",
                ),
                construct_context.clone(),
                "settings",
                Object::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(3),
                        None,
                        "ElementLabel[settings: [..]]",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    [
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(4),
                                None,
                                "ElementLabel[settings: [style]]",
                            ),
                            construct_context.clone(),
                            "style",
                            argument_style.clone(),
                            scoped_id.with_child_index(4),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(5),
                                None,
                                "ElementLabel[settings: [label]]",
                            ),
                            construct_context,
                            "label",
                            argument_label.clone(),
                            scoped_id.with_child_index(5),
                            actor_context.scope.clone(),
                        ),
                    ],
                ),
                scoped_id.with_child_index(2),
                actor_context.scope,
            ),
        ],
    )
}

/// ```text
/// Element/paragraph(
///     element<[]>
///     style<[]>
///     contents<List<Text | Link | ...>>
/// ) -> ELEMENT_PARAGRAPH
/// ```
pub fn function_element_paragraph(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_contents] = arguments.as_slice() else {
        panic!("Element/paragraph expects 3 arguments")
    };
    let scoped_id = function_call_persistence_id;
    TaggedObject::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/paragraph(..) -> ElementParagraph[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementParagraph",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "ElementParagraph[element]",
                ),
                construct_context.clone(),
                "element",
                argument_element.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "ElementParagraph[settings]",
                ),
                construct_context.clone(),
                "settings",
                Object::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(3),
                        None,
                        "ElementParagraph[settings: [..]]",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    [
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(4),
                                None,
                                "ElementParagraph[settings: [style]]",
                            ),
                            construct_context.clone(),
                            "style",
                            argument_style.clone(),
                            scoped_id.with_child_index(4),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(5),
                                None,
                                "ElementParagraph[settings: [contents]]",
                            ),
                            construct_context,
                            "contents",
                            argument_contents.clone(),
                            scoped_id.with_child_index(5),
                            actor_context.scope.clone(),
                        ),
                    ],
                ),
                scoped_id.with_child_index(2),
                actor_context.scope,
            ),
        ],
    )
}

/// ```text
/// Element/link(
///     element<[hovered?: LINK]>
///     style<[]>
///     label<Text | Element>
///     to<Text>
///     new_tab<[] | NewTab[...]>
/// ) -> ELEMENT_LINK
/// ```
pub fn function_element_link(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_label, argument_to, argument_new_tab] =
        arguments.as_slice()
    else {
        panic!("Element/link expects 5 arguments")
    };
    let scoped_id = function_call_persistence_id;
    TaggedObject::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/link(..) -> ElementLink[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementLink",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "ElementLink[element]",
                ),
                construct_context.clone(),
                "element",
                argument_element.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "ElementLink[settings]",
                ),
                construct_context.clone(),
                "settings",
                Object::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(3),
                        None,
                        "ElementLink[settings: [..]]",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    [
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(4),
                                None,
                                "ElementLink[settings: [style]]",
                            ),
                            construct_context.clone(),
                            "style",
                            argument_style.clone(),
                            scoped_id.with_child_index(4),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(5),
                                None,
                                "ElementLink[settings: [label]]",
                            ),
                            construct_context.clone(),
                            "label",
                            argument_label.clone(),
                            scoped_id.with_child_index(5),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(6),
                                None,
                                "ElementLink[settings: [to]]",
                            ),
                            construct_context.clone(),
                            "to",
                            argument_to.clone(),
                            scoped_id.with_child_index(6),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(7),
                                None,
                                "ElementLink[settings: [new_tab]]",
                            ),
                            construct_context,
                            "new_tab",
                            argument_new_tab.clone(),
                            scoped_id.with_child_index(7),
                            actor_context.scope.clone(),
                        ),
                    ],
                ),
                scoped_id.with_child_index(2),
                actor_context.scope,
            ),
        ],
    )
}

// @TODO refactor
/// ```text
/// Math/sum(increment<Number>) -> Number
/// ``````
pub fn function_math_sum(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    #[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
    #[serde(crate = "serde")]
    struct State {
        input_value_idempotency_key: Option<ValueIdempotencyKey>,
        sum: f64,
        output_value_idempotency_key: Option<ValueIdempotencyKey>,
    }

    let [argument_increment] = arguments.as_slice() else {
        panic!("Unexpected argument count")
    };
    let storage = construct_context.construct_storage.clone();

    let argument_increment_for_chain = argument_increment.clone();
    stream::once({
        let storage = storage.clone();
        async move {
            let loaded: Option<State> = storage.load_state(function_call_persistence_id).await;
            loaded
        }
    })
        .filter_map(future::ready)
        .chain(stream::once(async move {
            argument_increment_for_chain.stream().await.map(|value| State {
                input_value_idempotency_key: Some(value.idempotency_key()),
                sum: value.expect_number().number(),
                output_value_idempotency_key: None,
            })
        }).flatten())
        // @TODO refactor with async closure once possible?
        .scan(State::default(), {
            move |state,
                  State {
                      input_value_idempotency_key,
                      sum: number,
                      output_value_idempotency_key,
                  }| {
                let storage = storage.clone();
                let skip_value = state.input_value_idempotency_key == input_value_idempotency_key;
                if !skip_value {
                    state.input_value_idempotency_key = input_value_idempotency_key;
                    state.sum += number;
                    state.output_value_idempotency_key = if output_value_idempotency_key.is_some() {
                        output_value_idempotency_key
                    } else {
                        Some(ValueIdempotencyKey::new())
                    };
                }
                let state = *state;
                async move {
                    if skip_value {
                        Some(None)
                    } else {
                        storage
                            .save_state(function_call_persistence_id, &state)
                            .await;
                        Some(Some((
                            state.sum,
                            state.output_value_idempotency_key.unwrap(),
                        )))
                    }
                }
            }
        })
        .filter_map(future::ready)
        .map({
            let mut result_version = 0u64;
            move |(sum, idempotency_key)| {
                let value = Number::new_value(
                    ConstructInfo::new(
                        function_call_id
                            .with_child_id(format!("Math/sum result v.{result_version}")),
                        None,
                        "Math/sum(..) -> Number",
                    ),
                    construct_context.clone(),
                    idempotency_key,
                    sum,
                );
                result_version += 1;
                value
            }
        })
}

// @TODO remember configuration?
/// ```text
/// Timer/interval(duration<Duration[seconds<Number> | milliseconds<Number>]>) -> []
/// ```
pub fn function_timer_interval(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_duration] = arguments.as_slice() else {
        panic!("Unexpected argument count")
    };
    let argument_duration_for_stream = argument_duration.clone();
    stream::once(async move {
        argument_duration_for_stream.stream().await
            .then(|value| async move {
                let duration_object = value.expect_tagged_object("Duration");
                if let Some(seconds) = duration_object.variable("seconds") {
                    seconds.stream().await.map(|value| value.expect_number().number() * 1000.).left_stream()
                } else if let Some(milliseconds) = duration_object.variable("milliseconds") {
                    milliseconds.stream().await.map(|value| value.expect_number().number()).right_stream()
                } else {
                    panic!("Failed to get property 'seconds' or 'milliseconds' from tagged object 'Duration'");
                }
            })
            .flatten()
    }).flatten()
        .flat_map(move |milliseconds| {
            let function_call_id = function_call_id.clone();
            stream::unfold((function_call_id, 0u64), {
                let construct_context = construct_context.clone();
                move |(function_call_id, result_version)| {
                    let construct_context = construct_context.clone();
                    async move {
                        // @TODO How to properly resolve resuming? Only if it's a longer interval?
                        Timer::sleep(milliseconds.round().max(0.0).min(u32::MAX as f64) as u32).await;
                        let output_value = Object::new_value(
                            ConstructInfo::new(function_call_id.with_child_id("Timer/interval result v.{result_version}"), None, "Timer/interval(.. ) -> [..]"),
                            construct_context.clone(),
                            ValueIdempotencyKey::new(),
                            []
                        );
                        Some((output_value, (function_call_id, result_version + 1)))
                    }
                }
            })
        })
}

// --- Text functions ---

/// Text/empty constant
pub fn function_text_empty(
    _arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    stream::once(future::ready(Text::new_value(
        ConstructInfo::new(function_call_id.with_child_id(0), None, "Text/empty"),
        construct_context,
        ValueIdempotencyKey::new(),
        String::new(),
    )))
}

/// Text/space constant - returns a single space character " "
pub fn function_text_space(
    _arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    stream::once(future::ready(Text::new_value(
        ConstructInfo::new(function_call_id.with_child_id(0), None, "Text/space"),
        construct_context,
        ValueIdempotencyKey::new(),
        " ".to_string(),
    )))
}

/// Text/trim(text) -> Text
pub fn function_text_trim(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text] = arguments.as_slice() else {
        panic!("Text/trim expects 1 argument")
    };
    argument_text.clone().stream_sync().map(move |value| {
        let text = match &value {
            Value::Text(t, _) => t.text(),
            _ => panic!("Text/trim expects a Text value"),
        };
        Text::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, "Text/trim result"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            text.trim().to_string(),
        )
    })
}

/// Text/is_empty(text) -> Tag (True/False)
pub fn function_text_is_empty(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text] = arguments.as_slice() else {
        panic!("Text/is_empty expects 1 argument")
    };
    argument_text.clone().stream_sync().map(move |value| {
        let text = match &value {
            Value::Text(t, _) => t.text(),
            _ => panic!("Text/is_empty expects a Text value"),
        };
        let tag = if text.is_empty() { "True" } else { "False" };
        Tag::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, "Text/is_empty result"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            tag.to_string(),
        )
    })
}

/// Text/is_not_empty(text) -> Tag (True/False)
pub fn function_text_is_not_empty(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text] = arguments.as_slice() else {
        panic!("Text/is_not_empty expects 1 argument")
    };
    argument_text.clone().stream_sync().map(move |value| {
        let text = match &value {
            Value::Text(t, _) => t.text(),
            _ => panic!("Text/is_not_empty expects a Text value"),
        };
        let tag = if !text.is_empty() { "True" } else { "False" };
        Tag::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, "Text/is_not_empty result"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            tag.to_string(),
        )
    })
}

// --- Bool functions ---

/// Bool/not(value) -> Tag (True/False)
pub fn function_bool_not(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_value] = arguments.as_slice() else {
        panic!("Bool/not expects 1 argument")
    };
    argument_value.clone().stream_sync().map(move |value| {
        let is_true = match &value {
            Value::Tag(tag, _) => tag.tag() == "True",
            _ => panic!("Bool/not expects a Tag (True/False)"),
        };
        let result_tag = if is_true { "False" } else { "True" };
        Tag::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, "Bool/not result"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            result_tag.to_string(),
        )
    })
}

/// Bool/toggle(value, when) -> Tag (True/False)
/// Toggles the boolean value each time 'when' stream produces a value
pub fn function_bool_toggle(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    // Clone to avoid lifetime issues
    let argument_value = arguments[0].clone();
    let argument_when = arguments[1].clone();

    // Get initial value and toggle on each 'when' event
    let initial = argument_value.clone();
    let when_stream = argument_when.stream_sync();

    stream::once(async move {
        // Get initial boolean state
        let _current = true; // Will be set by first value
        initial
    })
    .chain(when_stream.map(move |_| {
        // This is a simplified implementation - real implementation would need state
        argument_value.clone()
    }))
    .then(|actor| async move { actor.stream().await })
    .flatten()
    .scan(None::<bool>, move |state, value| {
        let is_true = match &value {
            Value::Tag(tag, _) => tag.tag() == "True",
            _ => false,
        };
        let new_value = match state {
            None => is_true, // First value sets initial state
            Some(_) => !state.unwrap(), // Toggle on subsequent values
        };
        *state = Some(new_value);
        let result_tag = if new_value { "True" } else { "False" };
        future::ready(Some(Tag::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, "Bool/toggle result"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            result_tag.to_string(),
        )))
    })
}

/// Bool/or(this, that) -> Tag (True/False)
/// Returns True if either this or that is True
pub fn function_bool_or(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let this_actor = arguments[0].clone();
    let that_actor = arguments[1].clone();

    // Combine both boolean streams using select
    let this_stream = this_actor.stream_sync().map(|v| (true, v));
    let that_stream = that_actor.stream_sync().map(|v| (false, v));

    stream::select(this_stream, that_stream)
        .scan((None::<bool>, None::<bool>), move |state, (is_this, value)| {
            let is_true = match &value {
                Value::Tag(tag, _) => tag.tag() == "True",
                _ => false,
            };
            if is_this {
                state.0 = Some(is_true);
            } else {
                state.1 = Some(is_true);
            }
            future::ready(Some(*state))
        })
        .filter_map(move |(this_bool, that_bool)| {
            let construct_context = construct_context.clone();
            let function_call_id = function_call_id.clone();
            future::ready(match (this_bool, that_bool) {
                (Some(a), Some(b)) => {
                    let result = a || b;
                    let tag = if result { "True" } else { "False" };
                    Some(Tag::new_value(
                        ConstructInfo::new(function_call_id.with_child_id(0), None, "Bool/or result"),
                        construct_context,
                        ValueIdempotencyKey::new(),
                        tag.to_string(),
                    ))
                }
                _ => None, // Wait for both values
            })
        })
}

// --- List functions ---

/// List/empty() -> Tag (True/False)
/// Checks if the piped list is empty
pub fn function_list_empty(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let list_actor = arguments[0].clone();
    list_actor.stream_sync().filter_map(move |value| {
        let result = match &value {
            Value::List(list, _) => Some(list.clone()),
            _ => None,
        };
        future::ready(result)
    }).flat_map(move |list| {
        let construct_context = construct_context.clone();
        let function_call_id = function_call_id.clone();
        list.stream().scan(Vec::<Arc<ValueActor>>::new(), move |items, change| {
            change.apply_to_vec(items);
            let is_empty = items.is_empty();
            let tag = if is_empty { "True" } else { "False" };
            future::ready(Some(Tag::new_value(
                ConstructInfo::new(function_call_id.with_child_id(0), None, "List/empty result"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                tag.to_string(),
            )))
        })
    })
}

/// List/count -> Number
/// Returns the count of items in the list
pub fn function_list_count(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let list_actor = arguments[0].clone();
    list_actor.stream_sync().filter_map(move |value| {
        let result = match &value {
            Value::List(list, _) => Some(list.clone()),
            _ => None,
        };
        future::ready(result)
    }).flat_map(move |list| {
        let construct_context = construct_context.clone();
        let function_call_id = function_call_id.clone();
        list.stream().scan(Vec::<Arc<ValueActor>>::new(), move |items, change| {
            change.apply_to_vec(items);
            let count = items.len() as f64;
            future::ready(Some(Number::new_value(
                ConstructInfo::new(function_call_id.with_child_id(0), None, "List/count result"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                count,
            )))
        })
    })
}

/// List/not_empty() -> Tag (True/False)
/// Checks if the piped list is not empty (inverse of List/empty)
pub fn function_list_not_empty(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let list_actor = arguments[0].clone();
    list_actor.stream_sync().filter_map(move |value| {
        let result = match &value {
            Value::List(list, _) => Some(list.clone()),
            _ => None,
        };
        future::ready(result)
    }).flat_map(move |list| {
        let construct_context = construct_context.clone();
        let function_call_id = function_call_id.clone();
        list.stream().scan(Vec::<Arc<ValueActor>>::new(), move |items, change| {
            change.apply_to_vec(items);
            let is_not_empty = !items.is_empty();
            let tag = if is_not_empty { "True" } else { "False" };
            future::ready(Some(Tag::new_value(
                ConstructInfo::new(function_call_id.with_child_id(0), None, "List/not_empty result"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                tag.to_string(),
            )))
        })
    })
}

/// List/append(item: value) -> List
/// Appends an item to the list when the item stream produces a value
pub fn function_list_append(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    // arguments[0] = the list (piped)
    // arguments[1] = the item to append
    let list_actor = arguments[0].clone();

    // If item argument is SKIP (not provided), just forward the list unchanged
    if arguments.len() < 2 {
        return list_actor.stream_sync().left_stream();
    }

    let item_actor = arguments[1].clone();

    // Create a change stream that:
    // 1. Forwards all changes from the original list
    // 2. Adds Push changes when the item stream produces values
    //
    // IMPORTANT: The first change MUST be a Replace from the original list.
    // We use scan to ensure proper ordering: buffer append changes until
    // after the first list change arrives.
    let change_stream = {
        let function_call_id_for_append = function_call_id.clone();
        let actor_context_for_append = actor_context.clone();

        // Tag changes with their source so we can ensure proper ordering
        enum TaggedChange {
            FromList(ListChange),
            FromAppend(ListChange),
        }

        let list_changes = list_actor.clone().stream_sync().filter_map(|value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        }).flat_map(|list| list.stream()).map(TaggedChange::FromList);

        let append_changes = item_actor.clone().stream_sync().map(move |value| {
            // When item stream produces a value, create a new constant ValueActor
            // containing that specific value and push it.
            // Note: SKIP is not a Value - it's a stream behavior where streams end without
            // producing values. If item is SKIP, this map closure is never called.
            let new_item_actor = ValueActor::new_arc(
                ConstructInfo::new(
                    function_call_id_for_append.with_child_id("appended_item"),
                    None,
                    "List/append appended item",
                ),
                actor_context_for_append.clone(),
                constant(value),
                None,
            );
            TaggedChange::FromAppend(ListChange::Push { item: new_item_actor })
        });

        // Merge both change streams, then use scan to ensure proper ordering
        stream::select(list_changes, append_changes)
            .scan(
                (false, Vec::<ListChange>::new()), // (has_received_first_list_change, buffered_appends)
                |state, tagged_change| {
                    let (has_received_first, buffered) = state;

                    let changes_to_emit = match tagged_change {
                        TaggedChange::FromList(change) => {
                            if !*has_received_first {
                                // First list change - emit it plus any buffered appends
                                *has_received_first = true;
                                let mut all = vec![change];
                                all.append(buffered);
                                all
                            } else {
                                // Subsequent list change - emit directly
                                vec![change]
                            }
                        }
                        TaggedChange::FromAppend(change) => {
                            if *has_received_first {
                                // Already received first list change - emit directly
                                vec![change]
                            } else {
                                // Buffer until first list change arrives
                                buffered.push(change);
                                vec![]
                            }
                        }
                    };

                    future::ready(Some(changes_to_emit))
                }
            )
            .flat_map(|changes| stream::iter(changes))
    };

    let list = List::new_with_change_stream(
        ConstructInfo::new(function_call_id.with_child_id(ulid::Ulid::new().to_string()), None, "List/append result"),
        actor_context,
        change_stream,
        (list_actor, item_actor),
    );

    constant(Value::List(
        Arc::new(list),
        ValueMetadata { idempotency_key: ValueIdempotencyKey::new() },
    ))
    .right_stream()
}

/// List/clear(on: stream) -> List
/// Clears all items from the list when the trigger stream emits any value
pub fn function_list_clear(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    // arguments[0] = the list (piped)
    // arguments[1] = the trigger stream (on: xxx)
    let list_actor = arguments[0].clone();

    // If trigger argument is not provided, just forward the list unchanged
    if arguments.len() < 2 {
        return list_actor.stream_sync().left_stream();
    }

    let trigger_actor = arguments[1].clone();

    // Similar pattern to List/append:
    // 1. Forward all changes from the original list
    // 2. When trigger fires, emit Clear
    let change_stream = {
        enum TaggedChange {
            FromList(ListChange),
            Clear,
        }

        let list_changes = list_actor.clone().stream_sync().filter_map(|value| {
            future::ready(match value {
                Value::List(list, _) => Some(list),
                _ => None,
            })
        }).flat_map(|list| list.stream()).map(TaggedChange::FromList);

        // When trigger stream emits any value, emit Clear
        let clear_changes = trigger_actor.clone().stream_sync().map(|_value| {
            TaggedChange::Clear
        });

        // Merge both streams, use scan for proper ordering
        stream::select(list_changes, clear_changes)
            .scan(
                (false, false), // (has_received_first_list_change, has_pending_clear)
                |state, tagged_change| {
                    let (has_received_first, has_pending_clear) = state;

                    let changes_to_emit = match tagged_change {
                        TaggedChange::FromList(change) => {
                            if !*has_received_first {
                                *has_received_first = true;
                                // First list change - emit it plus pending clear if any
                                if *has_pending_clear {
                                    *has_pending_clear = false;
                                    vec![change, ListChange::Clear]
                                } else {
                                    vec![change]
                                }
                            } else {
                                vec![change]
                            }
                        }
                        TaggedChange::Clear => {
                            if *has_received_first {
                                vec![ListChange::Clear]
                            } else {
                                // Buffer the clear until first list change arrives
                                *has_pending_clear = true;
                                vec![]
                            }
                        }
                    };

                    future::ready(Some(changes_to_emit))
                }
            )
            .flat_map(|changes| stream::iter(changes))
    };

    let list = List::new_with_change_stream(
        ConstructInfo::new(function_call_id.with_child_id(0), None, "List/clear result"),
        actor_context,
        change_stream,
        (list_actor, trigger_actor),
    );

    constant(Value::List(
        Arc::new(list),
        ValueMetadata { idempotency_key: ValueIdempotencyKey::new() },
    ))
    .right_stream()
}

/// List/latest() -> Value
/// Merges a list of streams, emitting whenever any stream produces a value
/// Returns the value from the stream that most recently produced
pub fn function_list_latest(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let list_actor = arguments[0].clone();

    list_actor.stream_sync().filter_map(|value| {
        future::ready(match value {
            Value::List(list, _) => Some(list),
            _ => None,
        })
    }).flat_map(move |list| {
        let construct_context = construct_context.clone();
        let function_call_id = function_call_id.clone();

        // Subscribe to list changes and maintain current items
        list.stream().scan(Vec::<Arc<ValueActor>>::new(), move |items, change| {
            change.apply_to_vec(items);
            // Return current items for merging
            future::ready(Some(items.clone()))
        }).flat_map(move |items| {
            // Merge all item streams
            let streams: Vec<_> = items.iter().map(|item| item.clone().stream_sync()).collect();
            stream::select_all(streams)
        })
    })
}

// --- Router functions ---

/// Get the current URL pathname from the browser
fn get_current_pathname() -> String {
    window().location().pathname().unwrap_or_else(|_| "/".to_string())
}

/// Router/route() -> Text
/// Returns the current route/URL path as a reactive stream
/// Updates whenever the URL changes (via popstate event)
pub fn function_router_route(
    _arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    // Create a bounded channel for route changes (8 should be plenty for navigation)
    let (route_sender, route_receiver) = mpsc::channel::<String>(8);

    // Send initial route
    let initial_path = get_current_pathname();
    if let Err(e) = route_sender.try_send(initial_path) {
        log::debug!("[ROUTER] Failed to send initial route: {e}");
    }

    // Set up popstate listener for browser back/forward navigation
    let popstate_closure: Closure<dyn Fn()> = Closure::new({
        let route_sender = route_sender.clone();
        move || {
            let path = get_current_pathname();
            if let Err(e) = route_sender.try_send(path) {
                log::debug!("[ROUTER] Failed to send popstate route: {e}");
            }
        }
    });

    window()
        .add_event_listener_with_callback("popstate", popstate_closure.as_ref().unchecked_ref())
        .unwrap_throw();

    // Keep the closure alive by wrapping it
    let popstate_closure = SendWrapper::new(popstate_closure);

    // Store the global route sender for go_to to use
    ROUTE_SENDER.with(|cell| {
        *cell.borrow_mut() = Some(route_sender);
    });

    // Convert route strings to Text values
    route_receiver.map(move |path| {
        // Prevent drop: captured by `move` closure, lives as long as stream combinator
        let _popstate_closure = &popstate_closure;
        Text::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, "Router/route"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            path,
        )
    })
}

// Thread-local storage for route sender (allows go_to to trigger route updates)
thread_local! {
    static ROUTE_SENDER: std::cell::RefCell<Option<mpsc::Sender<String>>> = std::cell::RefCell::new(None);
}

/// Router/go_to(route) -> []
/// Navigates to the specified route using browser history API
pub fn function_router_go_to(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let route_actor = arguments[0].clone();

    route_actor.stream_sync().map(move |value| {
        let route = match &value {
            Value::Text(text, _) => text.text().to_string(),
            _ => "/".to_string(),
        };

        // Navigate using browser history API
        if route.starts_with('/') {
            history()
                .push_state_with_url(&JsValue::NULL, "", Some(&route))
                .unwrap_throw();

            // Notify route listeners about the change
            ROUTE_SENDER.with(|cell| {
                if let Some(sender) = cell.borrow().as_ref() {
                    if let Err(e) = sender.try_send(route) {
                        log::debug!("[ROUTER] Failed to send go_to route: {e}");
                    }
                }
            });
        }

        Object::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, "Router/go_to result"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            [],
        )
    })
}

// --- Ulid functions ---

/// Ulid/generate() -> Text
pub fn function_ulid_generate(
    _arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    stream::once(future::ready(Text::new_value(
        ConstructInfo::new(function_call_id.with_child_id(0), None, "Ulid/generate"),
        construct_context,
        ValueIdempotencyKey::new(),
        ulid::Ulid::new().to_string(),
    )))
}

// --- Log functions ---

use std::pin::Pin;
use std::future::Future;

/// Default timeout in milliseconds for waiting on nested actor values
const LOG_VALUE_DEFAULT_TIMEOUT_MS: u32 = 100;

/// Options extracted from the 'with' parameter for Log functions.
/// Contains optional label and timeout for resolving nested values.
struct LogOptions {
    label: Option<String>,
    timeout_ms: u32,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            label: None,
            timeout_ms: LOG_VALUE_DEFAULT_TIMEOUT_MS,
        }
    }
}

/// Async function to resolve a Value to string for logging.
/// Awaits nested actors with timeout - shows `?` for values that don't arrive in time.
/// Uses Pin<Box<...>> for recursive calls to break infinite type recursion.
fn resolve_value_for_log(value: Value, timeout_ms: u32) -> Pin<Box<dyn Future<Output = String>>> {
    Box::pin(async move {
        match value {
            Value::Text(text, _) => text.text().to_string(),
            Value::Number(num, _) => num.number().to_string(),
            Value::Tag(tag, _) => tag.tag().to_string(),
            Value::Object(object, _) => {
                let mut fields = Vec::new();
                for variable in object.variables() {
                    let name = variable.name().to_string();
                    let field_value = resolve_actor_value_for_log(variable.value_actor(), timeout_ms).await;
                    fields.push(format!("{}: {}", name, field_value));
                }
                format!("[{}]", fields.join(", "))
            }
            Value::TaggedObject(tagged, _) => {
                let mut fields = Vec::new();
                for variable in tagged.variables() {
                    let name = variable.name().to_string();
                    let field_value = resolve_actor_value_for_log(variable.value_actor(), timeout_ms).await;
                    fields.push(format!("{}: {}", name, field_value));
                }
                format!("{}[{}]", tagged.tag(), fields.join(", "))
            }
            Value::List(list, _) => {
                let mut items = Vec::new();
                for (_item_id, item_actor) in list.snapshot().await {
                    let item_value = resolve_actor_value_for_log(item_actor, timeout_ms).await;
                    items.push(item_value);
                }
                if items.is_empty() {
                    "LIST { }".to_string()
                } else {
                    format!("LIST {{ {} }}", items.join(", "))
                }
            }
            Value::Flushed(inner, _) => format!("Flushed[{}]", resolve_value_for_log(*inner, timeout_ms).await),
        }
    })
}

/// Async function to get value from a ValueActor for logging with timeout.
/// Returns `?` if no value arrives within the timeout.
fn resolve_actor_value_for_log(actor: Arc<ValueActor>, timeout_ms: u32) -> Pin<Box<dyn Future<Output = String>>> {
    Box::pin(async move {
        use zoon::futures_util::StreamExt;

        // Race subscription against timeout
        let get_value = async {
            actor.stream().await.next().await
        };
        let timeout = Timer::sleep(timeout_ms);

        select! {
            value = get_value.fuse() => {
                if let Some(value) = value {
                    resolve_value_for_log(value, timeout_ms).await
                } else {
                    "?".to_string()
                }
            }
            _ = timeout.fuse() => {
                "?".to_string()
            }
        }
    })
}

/// Resolve a value for logging with a specific timeout.
async fn resolve_value_for_log_with_timeout(value: Value, timeout_ms: u32) -> String {
    resolve_value_for_log(value, timeout_ms).await
}

/// Helper to extract log options from a 'with' object parameter.
/// The 'with' object can contain:
/// - 'label': Text label for the log message
/// - 'timeout': Duration[seconds: N] or Duration[milliseconds: N] for nested value resolution
/// Returns LogOptions with defaults if fields are not present.
async fn extract_log_options_from_with(with_actor: Arc<ValueActor>) -> LogOptions {
    use zoon::futures_util::StreamExt;

    let mut options = LogOptions::default();

    // Get the 'with' object value
    let with_value = match with_actor.stream().await.next().await {
        Some(v) => v,
        None => return options,
    };

    // Check if it's an Object and extract the fields
    if let Value::Object(obj, _) = with_value {
        // Extract label
        if let Some(label_var) = obj.variable("label") {
            if let Some(label_value) = label_var.value_actor().stream().await.next().await {
                options.label = Some(resolve_value_for_log(label_value).await);
            }
        }

        // Extract timeout from Duration[seconds: N] or Duration[milliseconds: N]
        if let Some(timeout_var) = obj.variable("timeout") {
            if let Some(timeout_value) = timeout_var.value_actor().stream().await.next().await {
                if let Value::TaggedObject(tagged, _) = timeout_value {
                    if tagged.tag() == "Duration" {
                        if let Some(seconds_var) = tagged.variable("seconds") {
                            if let Some(Value::Number(num, _)) = seconds_var.value_actor().stream().await.next().await {
                                options.timeout_ms = (num.number() * 1000.0).max(0.0).min(u32::MAX as f64) as u32;
                            }
                        } else if let Some(milliseconds_var) = tagged.variable("milliseconds") {
                            if let Some(Value::Number(num, _)) = milliseconds_var.value_actor().stream().await.next().await {
                                options.timeout_ms = num.number().max(0.0).min(u32::MAX as f64) as u32;
                            }
                        }
                    }
                }
            }
        }
    }

    options
}

/// Log/info(value: T) -> T
/// Log/info(value: T, with: [label: Text, timeout: Duration]) -> T
/// Logs an info message to the console and passes through the input value.
/// Output format: `[INFO] {label}: {value}` or `[INFO] {value}`
/// The 'with' parameter accepts:
/// - label: Text label for the log message
/// - timeout: Duration[milliseconds: N] or Duration[seconds: N] for nested value resolution
pub fn function_log_info(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let value_actor = arguments[0].clone();
    let with_actor = arguments.get(1).cloned();

    value_actor.stream_sync().map(move |value| {
        let with_actor = with_actor.clone();
        let value_clone = value.clone();
        // Spawn async task to resolve all nested values and log
        zoon::Task::start(async move {
            // Extract options (label and timeout) from 'with' object
            let options = if let Some(with) = with_actor {
                extract_log_options_from_with(with).await
            } else {
                LogOptions::default()
            };
            // Resolve value with the configured timeout
            let value_str = resolve_value_for_log_with_timeout(value_clone, options.timeout_ms).await;
            // Log with or without label
            match options.label {
                Some(label) if !label.is_empty() => zoon::println!("[INFO] {}: {}", label, value_str),
                _ => zoon::println!("[INFO] {}", value_str),
            }
        });
        // Pass through the input value immediately for chaining
        value
    })
    // Chain with pending() to keep stream alive forever - prevents actor termination
    .chain(stream::pending())
}

/// Log/error(value: T) -> T
/// Log/error(value: T, with: [label: Text, timeout: Duration]) -> T
/// Logs an error message to the console and passes through the input value.
/// Output format: `[ERROR] {label}: {value}` or `[ERROR] {value}`
/// The 'with' parameter accepts:
/// - label: Text label for the error message
/// - timeout: Duration[milliseconds: N] or Duration[seconds: N] for nested value resolution
pub fn function_log_error(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let value_actor = arguments[0].clone();
    let with_actor = arguments.get(1).cloned();

    value_actor.stream_sync().map(move |value| {
        let with_actor = with_actor.clone();
        let value_clone = value.clone();
        // Spawn async task to resolve all nested values and log
        zoon::Task::start(async move {
            // Extract options (label and timeout) from 'with' object
            let options = if let Some(with) = with_actor {
                extract_log_options_from_with(with).await
            } else {
                LogOptions::default()
            };
            // Resolve value with the configured timeout
            let value_str = resolve_value_for_log_with_timeout(value_clone, options.timeout_ms).await;
            // Log with or without label
            match options.label {
                Some(label) if !label.is_empty() => zoon::eprintln!("[ERROR] {}: {}", label, value_str),
                _ => zoon::eprintln!("[ERROR] {}", value_str),
            }
        });
        // Pass through the input value immediately for chaining
        value
    })
    // Chain with pending() to keep stream alive forever - prevents actor termination
    .chain(stream::pending())
}

// --- Build functions ---

/// Build/succeed() -> Tag (Success)
/// Returns a successful build result
pub fn function_build_succeed(
    _arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    stream::once(future::ready(Tag::new_value(
        ConstructInfo::new(function_call_id.with_child_id(0), None, "Build/succeed"),
        construct_context,
        ValueIdempotencyKey::new(),
        "Success".to_string(),
    )))
}

/// Build/fail(error) -> Tag (Failure)
/// Returns a failed build result (placeholder - logging to be implemented)
pub fn function_build_fail(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let error_actor = arguments[0].clone();
    error_actor.stream_sync().map(move |value| {
        let _error_message = match &value {
            Value::Text(text, _) => text.text().to_string(),
            _ => "Unknown build error".to_string(),
        };
        // @TODO: Add proper console logging when web_sys console feature is available
        Tag::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, "Build/fail"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            "Failure".to_string(),
        )
    })
}

// --- Scene functions ---

/// Scene/new(root<INTO_ELEMENT>) -> []
/// Creates a new scene for DOM rendering (stub - passes through to Document/new behavior)
/// @TODO: Implement proper scene management when needed
pub fn function_scene_new(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_root] = arguments.as_slice() else {
        panic!("Unexpected argument count for Scene/new")
    };
    let scoped_id = function_call_persistence_id;
    // Scene/new returns an empty object - the actual rendering is handled by the element tree
    Object::new_constant(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Scene/new(..) -> []",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        [Variable::new_arc(
            ConstructInfo::new(
                function_call_id.with_child_id(1),
                None,
                "Scene/new(..) -> [root_element]",
            ),
            construct_context,
            "root_element",
            argument_root.clone(),
            scoped_id.with_child_index(1),
            actor_context.scope,
        )],
    )
}

// --- Theme functions ---

/// Theme/background_color() -> Text
/// Returns the current theme background color (stub)
pub fn function_theme_background_color(
    _arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    stream::once(future::ready(Text::new_value(
        ConstructInfo::new(function_call_id.with_child_id(0), None, "Theme/background_color"),
        construct_context,
        ValueIdempotencyKey::new(),
        "#ffffff".to_string(),
    )))
}

/// Theme/text_color() -> Text
/// Returns the current theme text color (stub)
pub fn function_theme_text_color(
    _arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    stream::once(future::ready(Text::new_value(
        ConstructInfo::new(function_call_id.with_child_id(0), None, "Theme/text_color"),
        construct_context,
        ValueIdempotencyKey::new(),
        "#000000".to_string(),
    )))
}

/// Theme/accent_color() -> Text
/// Returns the current theme accent color (stub)
pub fn function_theme_accent_color(
    _arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    stream::once(future::ready(Text::new_value(
        ConstructInfo::new(function_call_id.with_child_id(0), None, "Theme/accent_color"),
        construct_context,
        ValueIdempotencyKey::new(),
        "#0066cc".to_string(),
    )))
}

// --- File functions ---

/// File/read_text(path) -> Text
/// Reads text content from a file at the given path
pub fn function_file_read_text(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let path_actor = arguments[0].clone();
    path_actor.stream_sync().then(move |value| {
        let construct_context = construct_context.clone();
        let function_call_id = function_call_id.clone();
        async move {
            let path = match &value {
                Value::Text(text, _) => text.text().to_string(),
                _ => String::new(),
            };
            let content = construct_context
                .virtual_fs
                .read_text(&path)
                .await
                .unwrap_or_default();
            Text::new_value(
                ConstructInfo::new(function_call_id.with_child_id(0), None, "File/read_text"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                content,
            )
        }
    })
}

/// File/write_text(path, content) -> Tag (Success/Failure)
/// Writes text content to a file at the given path
pub fn function_file_write_text(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let path_actor = arguments[0].clone();
    let content_actor = arguments[1].clone();

    let construct_context_clone = construct_context.clone();
    path_actor.stream_sync().flat_map(move |path_value| {
        let path = match &path_value {
            Value::Text(text, _) => text.text().to_string(),
            _ => String::new(),
        };
        let function_call_id = function_call_id.clone();
        let construct_context = construct_context_clone.clone();
        content_actor.clone().stream_sync().map(move |content_value| {
            let content = match &content_value {
                Value::Text(text, _) => text.text().to_string(),
                _ => String::new(),
            };
            construct_context.virtual_fs.write_text(&path, content);
            Tag::new_value(
                ConstructInfo::new(function_call_id.with_child_id(0), None, "File/write_text"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                "Success".to_string(),
            )
        })
    })
}

// --- Stream functions ---

/// Stream/skip(count) -> Stream<Value>
/// Skips the first N values from the piped stream.
/// When `count` changes, the skip counter resets and starts skipping again.
///
/// # Implementation
/// Uses `stream::unfold()` for a pure demand-driven stream (no Task spawn).
pub fn function_stream_skip(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let stream_actor = arguments[0].clone();
    let count_actor = arguments[1].clone();

    // State type for unfold
    type FusedSub = stream::Fuse<LocalBoxStream<'static, Value>>;
    type InitialState = (Option<FusedSub>, Option<FusedSub>, Arc<ValueActor>, Arc<ValueActor>, usize, usize, bool, Vec<Value>);

    // Defer subscription to inside async unfold
    let initial_state: InitialState = (
        None,  // stream_sub - will be initialized in unfold
        None,  // count_sub - will be initialized in unfold
        stream_actor,
        count_actor,
        0,     // current_skip_count
        0,     // skipped
        false, // count_received
        Vec::new(), // buffered_values
    );

    stream::unfold(initial_state, |state| async move {
        let (stream_sub_opt, count_sub_opt, stream_actor, count_actor, mut skip_count, mut skipped, mut count_received, mut buffer) = state;

        // Subscribe on first iteration
        let mut stream_sub = match stream_sub_opt {
            Some(s) => s,
            None => stream_actor.clone().stream().await.boxed_local().fuse(),
        };
        let mut count_sub = match count_sub_opt {
            Some(s) => s,
            None => count_actor.clone().stream().await.boxed_local().fuse(),
        };

        loop {
            // If we have buffered values and count is received, process buffer first
            if count_received && !buffer.is_empty() {
                let buffered = buffer.remove(0);
                if skipped < skip_count {
                    skipped += 1;
                    // Continue loop to process next buffered value
                } else {
                    return Some((buffered, (Some(stream_sub), Some(count_sub), stream_actor, count_actor, skip_count, skipped, count_received, buffer)));
                }
                continue;
            }

            if !count_received {
                // Wait for count first, buffer stream values
                select! {
                    count_value = count_sub.next() => {
                        match count_value {
                            Some(value) => {
                                skip_count = match &value {
                                    Value::Number(num, _) => num.number().max(0.0).min(usize::MAX as f64) as usize,
                                    _ => 0,
                                };
                                count_received = true;
                                // Buffer processing will happen on next loop iteration
                            }
                            None => return None, // Count stream ended
                        }
                    }
                    stream_value = stream_sub.next() => {
                        match stream_value {
                            Some(value) => buffer.push(value),
                            None => return None, // Stream ended
                        }
                    }
                }
            } else {
                // Normal operation - skip values
                select! {
                    count_value = count_sub.next() => {
                        match count_value {
                            Some(value) => {
                                skip_count = match &value {
                                    Value::Number(num, _) => num.number().max(0.0).min(usize::MAX as f64) as usize,
                                    _ => 0,
                                };
                                skipped = 0; // Reset on count change
                            }
                            None => return None, // Count stream ended
                        }
                    }
                    stream_value = stream_sub.next() => {
                        match stream_value {
                            Some(value) => {
                                if skipped < skip_count {
                                    skipped += 1;
                                } else {
                                    return Some((value, (Some(stream_sub), Some(count_sub), stream_actor, count_actor, skip_count, skipped, count_received, buffer)));
                                }
                            }
                            None => return None, // Stream ended
                        }
                    }
                }
            }
        }
    })
}

/// Stream/take(count) -> Stream<Value>
/// Takes only the first N values from the piped stream.
/// When `count` changes, the take counter resets.
///
/// # Implementation
/// Uses `stream::unfold()` for a pure demand-driven stream (no Task spawn).
pub fn function_stream_take(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let stream_actor = arguments[0].clone();
    let count_actor = arguments[1].clone();

    // State type for unfold
    type FusedSub = stream::Fuse<LocalBoxStream<'static, Value>>;
    type TakeState = (Option<FusedSub>, Option<FusedSub>, Arc<ValueActor>, Arc<ValueActor>, usize, usize, bool, Vec<Value>);

    // Defer subscription to inside async unfold
    let initial_state: TakeState = (
        None,  // stream_sub - will be initialized in unfold
        None,  // count_sub - will be initialized in unfold
        stream_actor,
        count_actor,
        0,     // current_take_count
        0,     // taken
        false, // count_received
        Vec::new(), // buffered_values
    );

    stream::unfold(initial_state, |state| async move {
        let (stream_sub_opt, count_sub_opt, stream_actor, count_actor, mut take_count, mut taken, mut count_received, mut buffer) = state;

        // Subscribe on first iteration
        let mut stream_sub = match stream_sub_opt {
            Some(s) => s,
            None => stream_actor.clone().stream().await.boxed_local().fuse(),
        };
        let mut count_sub = match count_sub_opt {
            Some(s) => s,
            None => count_actor.clone().stream().await.boxed_local().fuse(),
        };

        loop {
            // If we have buffered values and count is received, process buffer first
            if count_received && !buffer.is_empty() {
                let buffered = buffer.remove(0);
                if taken < take_count {
                    taken += 1;
                    return Some((buffered, (Some(stream_sub), Some(count_sub), stream_actor, count_actor, take_count, taken, count_received, buffer)));
                }
                // Exceeded take limit, drop this buffered value and continue
                continue;
            }

            if !count_received {
                // Wait for count first, buffer stream values
                select! {
                    count_value = count_sub.next() => {
                        match count_value {
                            Some(value) => {
                                take_count = match &value {
                                    Value::Number(num, _) => num.number().max(0.0).min(usize::MAX as f64) as usize,
                                    _ => 0,
                                };
                                count_received = true;
                                // Buffer processing will happen on next loop iteration
                            }
                            None => return None, // Count stream ended
                        }
                    }
                    stream_value = stream_sub.next() => {
                        match stream_value {
                            Some(value) => buffer.push(value),
                            None => return None, // Stream ended
                        }
                    }
                }
            } else {
                // Normal operation - take values up to limit
                select! {
                    count_value = count_sub.next() => {
                        match count_value {
                            Some(value) => {
                                take_count = match &value {
                                    Value::Number(num, _) => num.number().max(0.0).min(usize::MAX as f64) as usize,
                                    _ => 0,
                                };
                                taken = 0; // Reset on count change
                            }
                            None => return None, // Count stream ended
                        }
                    }
                    stream_value = stream_sub.next() => {
                        match stream_value {
                            Some(value) => {
                                if taken < take_count {
                                    taken += 1;
                                    return Some((value, (Some(stream_sub), Some(count_sub), stream_actor, count_actor, take_count, taken, count_received, buffer)));
                                }
                                // After taking enough, drop subsequent values (but keep listening for count changes)
                            }
                            None => return None, // Stream ended
                        }
                    }
                }
            }
        }
    })
}

/// Stream/distinct() -> Stream<Value>
/// Suppresses consecutive duplicate values.
pub fn function_stream_distinct(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let stream_actor = arguments[0].clone();

    stream_actor.stream_sync().scan(None::<ValueIdempotencyKey>, move |last_key, value| {
        let current_key = value.idempotency_key();
        let should_emit = last_key.map_or(true, |k| k != current_key);
        *last_key = Some(current_key);
        if should_emit {
            future::ready(Some(Some(value)))
        } else {
            future::ready(Some(None))
        }
    })
    .filter_map(future::ready)
}

/// Stream/pulses() -> Stream<Number>
/// Generates N pulses (1, 2, 3, ..., N) from the piped count.
/// When the count changes, restarts pulse generation from 1.
///
/// Uses pure stream combinators (no Task, no Rc<RefCell>) per Engine Architecture Rules.
/// Initial pulses are emitted synchronously via stream::iter() to ensure HOLD + Stream/pulses
/// patterns work correctly without race conditions.
pub fn function_stream_pulses(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let count_actor = arguments[0].clone();

    // Helper to generate pulses from a count value
    let make_pulses = {
        let function_call_id = function_call_id.clone();
        let construct_context = construct_context.clone();
        move |count_value: &Value| -> Vec<Value> {
            let pulse_count = match count_value {
                Value::Number(num, _) => num.number() as u64,
                _ => 0,
            };
            (1..=pulse_count)
                .map(|n| {
                    Number::new_value(
                        ConstructInfo::new(
                            function_call_id.with_child_id(format!("pulse_{}", n)),
                            None,
                            "Stream/pulses result",
                        ),
                        construct_context.clone(),
                        ValueIdempotencyKey::new(),
                        n as f64,
                    )
                })
                .collect()
        }
    };

    // Subscribe to count actor and generate pulses for each count value
    count_actor.stream_sync().flat_map(move |v| stream::iter(make_pulses(&v)))
}

/// Stream/debounce(duration) -> Stream<Value>
/// Emits the most recent value after the input stream stops emitting for the specified duration.
/// When a new value arrives, it resets the timer. Only when the timer expires (no new values
/// for `duration`), the most recent value is emitted.
/// When `duration` changes, the debounce timer is updated with the new duration.
///
/// # Implementation
/// Uses `stream::unfold()` for a pure demand-driven stream (no Task spawn).
pub fn function_stream_debounce(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let stream_actor = arguments[0].clone();
    let duration_actor = arguments[1].clone();

    // Helper to extract milliseconds from Duration tagged object
    fn extract_duration_ms(value: &Value) -> f64 {
        let duration_object = value.clone().expect_tagged_object("Duration");
        if let Some(seconds) = duration_object.variable("seconds") {
            let mut sub = seconds.value_actor().stream_sync();
            if let Some(value) = sub.next().now_or_never().flatten() {
                return value.expect_number().number() * 1000.0;
            }
        }
        if let Some(ms) = duration_object.variable("ms") {
            let mut sub = ms.value_actor().stream_sync();
            if let Some(value) = sub.next().now_or_never().flatten() {
                return value.expect_number().number();
            }
        }
        if let Some(milliseconds) = duration_object.variable("milliseconds") {
            let mut sub = milliseconds.value_actor().stream_sync();
            if let Some(value) = sub.next().now_or_never().flatten() {
                return value.expect_number().number();
            }
        }
        0.0
    }

    // State type for unfold
    type FusedSub = stream::Fuse<LocalBoxStream<'static, Value>>;
    type DebounceState = (Option<FusedSub>, Option<FusedSub>, Arc<ValueActor>, Arc<ValueActor>, Option<Value>, f64);

    let initial_state: DebounceState = (
        None, // input_stream - deferred
        None, // duration_stream - deferred
        stream_actor,
        duration_actor,
        None, // pending_value
        0.0,  // current_duration_ms
    );

    stream::unfold(initial_state, |state| async move {
        let (input_opt, duration_opt, stream_actor, duration_actor, mut pending, mut duration_ms) = state;

        // Subscribe on first iteration
        let mut input_stream = match input_opt {
            Some(s) => s,
            None => stream_actor.clone().stream().await.boxed_local().fuse(),
        };
        let mut duration_stream = match duration_opt {
            Some(s) => s,
            None => duration_actor.clone().stream().await.boxed_local().fuse(),
        };

        loop {
            if pending.is_some() && duration_ms > 0.0 {
                // Have pending value and valid duration - race timer vs new input
                let mut timer = Box::pin(Timer::sleep(duration_ms.round().max(0.0).min(u32::MAX as f64) as u32).fuse());

                select! {
                    new_value = input_stream.next() => {
                        match new_value {
                            Some(value) => {
                                // New value - update pending, timer restarts on next loop
                                pending = Some(value);
                            }
                            None => {
                                // Input ended - emit pending and finish
                                if let Some(value) = pending.take() {
                                    return Some((value, (Some(input_stream), Some(duration_stream), stream_actor, duration_actor, None, duration_ms)));
                                }
                                return None;
                            }
                        }
                    }
                    new_duration = duration_stream.next() => {
                        if let Some(duration_value) = new_duration {
                            duration_ms = extract_duration_ms(&duration_value);
                        }
                        // Continue with updated duration
                    }
                    _ = timer.as_mut() => {
                        // Timer expired - emit pending
                        if let Some(value) = pending.take() {
                            return Some((value, (Some(input_stream), Some(duration_stream), stream_actor, duration_actor, None, duration_ms)));
                        }
                    }
                }
            } else {
                // No pending or no duration - wait for input or duration
                select! {
                    new_value = input_stream.next() => {
                        match new_value {
                            Some(value) => {
                                pending = Some(value);
                            }
                            None => return None, // Input ended
                        }
                    }
                    new_duration = duration_stream.next() => {
                        if let Some(duration_value) = new_duration {
                            duration_ms = extract_duration_ms(&duration_value);
                        }
                    }
                }
            }
        }
    })
}

// --- Directory functions ---

/// Directory/entries(path) -> List<Text>
/// Returns a list of file/directory names in the given directory
pub fn function_directory_entries(
    arguments: Arc<Vec<Arc<ValueActor>>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let path_actor = arguments[0].clone();
    path_actor.stream_sync().then(move |value| {
        let construct_context = construct_context.clone();
        let function_call_id = function_call_id.clone();
        let actor_context = actor_context.clone();
        async move {
            let path = match &value {
                Value::Text(text, _) => text.text().to_string(),
                _ => String::new(),
            };
            let entries = construct_context.virtual_fs.list_directory(&path).await;
            let entry_actors: Vec<Arc<ValueActor>> = entries
                .into_iter()
                .enumerate()
                .map(|(i, entry)| {
                    Text::new_arc_value_actor(
                        ConstructInfo::new(
                            function_call_id.with_child_id(i as u32),
                            None,
                            "Directory/entries item",
                        ),
                        construct_context.clone(),
                        ValueIdempotencyKey::new(),
                        actor_context.clone(),
                        entry,
                    )
                })
                .collect();
            List::new_value(
                ConstructInfo::new(function_call_id.with_child_id(0), None, "Directory/entries"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                actor_context.clone(),
                entry_actors,
            )
        }
    })
}
