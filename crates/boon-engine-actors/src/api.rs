use std::future;
use std::sync::Arc;

use zoon::Timer;
use zoon::futures_channel::mpsc;
use zoon::futures_util::{
    FutureExt, SinkExt, pin_mut, select,
    stream::{self, LocalBoxStream, Stream, StreamExt},
};
use zoon::{Closure, JsCast, JsValue, SendWrapper, UnwrapThrowExt, history, window};
use zoon::{Deserialize, Serialize, serde};

use crate::engine::*;

use boon::parser::PersistenceId;

// @TODO make sure Values are deduplicated everywhere it makes sense

fn event_actor_from_constant_element(
    argument_element: &ActorHandle,
    scope_id: ScopeId,
) -> ActorHandle {
    argument_element
        .current_value()
        .ok()
        .and_then(|element_value| {
            element_value
                .expect_object()
                .variable("event")
                .map(|event_variable| event_variable.value_actor())
        })
        .unwrap_or_else(|| create_actor_forwarding(PersistenceId::new(), scope_id))
}

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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_root] = arguments.as_slice() else {
        panic!("Unexpected argument count")
    };
    let scoped_id = function_call_persistence_id;
    stream::once(future::ready(Object::new_value(
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
            "root_element",
            argument_root.clone(),
            scoped_id.with_child_index(1),
            actor_context.scope.clone(),
        )],
    )))
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [
        argument_element,
        argument_direction,
        argument_gap,
        argument_style,
        argument_items,
    ] = arguments.as_slice()
    else {
        panic!(
            "Element/stripe requires 5 arguments, got {}",
            arguments.len()
        );
    };
    let scoped_id = function_call_persistence_id;
    stream::once(future::ready(TaggedObject::new_value(
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
    )))
}

/// ```text
/// Element/container(
///     element<[tag?: Tag]>
///     style<[]>
///     child<INTO_ELEMENT>
/// ) -> ELEMENT_CONTAINER
/// ```
pub fn function_element_container(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_child] = arguments.as_slice() else {
        panic!("Element/container expects 3 arguments")
    };
    let scoped_id = function_call_persistence_id;
    stream::once(future::ready(TaggedObject::new_value(
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
    )))
}

/// ```text
/// Element/stack(
///     element<[tag?: Tag]>
///     style<[]>
///     layers<List<INTO_ELEMENT>>
/// ) -> ELEMENT_STACK
/// ```
pub fn function_element_stack(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_layers] = arguments.as_slice() else {
        panic!(
            "Element/stack requires 3 arguments, got {}",
            arguments.len()
        );
    };
    let scoped_id = function_call_persistence_id;
    stream::once(future::ready(TaggedObject::new_value(
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
    )))
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_label] = arguments.as_slice() else {
        panic!("Unexpected argument count")
    };
    let scoped_id = function_call_persistence_id;
    let event_actor = event_actor_from_constant_element(argument_element, actor_context.scope_id());

    stream::once(future::ready(TaggedObject::new_value(
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
    )))
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [
        argument_element,
        argument_style,
        argument_label,
        argument_text,
        argument_placeholder,
        argument_focus,
    ] = arguments.as_slice()
    else {
        panic!("Element/text_input expects 6 arguments")
    };
    let scoped_id = function_call_persistence_id;
    let event_actor = event_actor_from_constant_element(argument_element, actor_context.scope_id());

    stream::once(future::ready(TaggedObject::new_value(
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
    )))
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [
        argument_element,
        argument_style,
        argument_label,
        argument_checked,
        argument_icon,
    ] = arguments.as_slice()
    else {
        panic!("Element/checkbox expects 5 arguments")
    };
    let scoped_id = function_call_persistence_id;
    let event_actor = event_actor_from_constant_element(argument_element, actor_context.scope_id());

    stream::once(future::ready(TaggedObject::new_value(
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
    )))
}

/// ```text
/// Element/slider(
///     element<[event?<[change?: LINK]>]>
///     style<[]>
///     label<Hidden[text: Text]>
///     value<Number>
///     min<Number>
///     max<Number>
///     step<Number>
/// ) -> ELEMENT_SLIDER
/// ```
pub fn function_element_slider(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [
        argument_element,
        argument_style,
        argument_label,
        argument_value,
        argument_min,
        argument_max,
        argument_step,
    ] = arguments.as_slice()
    else {
        panic!("Element/slider expects 7 arguments")
    };
    let scoped_id = function_call_persistence_id;
    let event_actor = event_actor_from_constant_element(argument_element, actor_context.scope_id());

    stream::once(future::ready(TaggedObject::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/slider(..) -> ElementSlider[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementSlider",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "ElementSlider[element]",
                ),
                "element",
                argument_element.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(11),
                    None,
                    "ElementSlider[event]",
                ),
                "event",
                event_actor,
                scoped_id.with_child_index(11),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "ElementSlider[settings]",
                ),
                "settings",
                Object::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(3),
                        None,
                        "ElementSlider[settings: [..]]",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    [
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(4),
                                None,
                                "ElementSlider[settings: [style]]",
                            ),
                            "style",
                            argument_style.clone(),
                            scoped_id.with_child_index(4),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(5),
                                None,
                                "ElementSlider[settings: [label]]",
                            ),
                            "label",
                            argument_label.clone(),
                            scoped_id.with_child_index(5),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(6),
                                None,
                                "ElementSlider[settings: [value]]",
                            ),
                            "value",
                            argument_value.clone(),
                            scoped_id.with_child_index(6),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(7),
                                None,
                                "ElementSlider[settings: [min]]",
                            ),
                            "min",
                            argument_min.clone(),
                            scoped_id.with_child_index(7),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(8),
                                None,
                                "ElementSlider[settings: [max]]",
                            ),
                            "max",
                            argument_max.clone(),
                            scoped_id.with_child_index(8),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(9),
                                None,
                                "ElementSlider[settings: [step]]",
                            ),
                            "step",
                            argument_step.clone(),
                            scoped_id.with_child_index(9),
                            actor_context.scope.clone(),
                        ),
                    ],
                ),
                scoped_id.with_child_index(2),
                actor_context.scope,
            ),
        ],
    )))
}

/// ```text
/// Element/select(
///     element<[event?<[change?: LINK]>]>
///     style<[]>
///     label<Hidden[text: Text]>
///     options<LIST { [value: Text, label: Text] }>
///     selected<Text>
/// ) -> ELEMENT_SELECT
/// ```
pub fn function_element_select(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [
        argument_element,
        argument_style,
        argument_label,
        argument_options,
        argument_selected,
    ] = arguments.as_slice()
    else {
        panic!("Element/select expects 5 arguments")
    };
    let scoped_id = function_call_persistence_id;
    let event_actor = event_actor_from_constant_element(argument_element, actor_context.scope_id());

    stream::once(future::ready(TaggedObject::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/select(..) -> ElementSelect[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementSelect",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "ElementSelect[element]",
                ),
                "element",
                argument_element.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(9),
                    None,
                    "ElementSelect[event]",
                ),
                "event",
                event_actor,
                scoped_id.with_child_index(9),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "ElementSelect[settings]",
                ),
                "settings",
                Object::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(3),
                        None,
                        "ElementSelect[settings: [..]]",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    [
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(4),
                                None,
                                "ElementSelect[settings: [style]]",
                            ),
                            "style",
                            argument_style.clone(),
                            scoped_id.with_child_index(4),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(5),
                                None,
                                "ElementSelect[settings: [label]]",
                            ),
                            "label",
                            argument_label.clone(),
                            scoped_id.with_child_index(5),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(6),
                                None,
                                "ElementSelect[settings: [options]]",
                            ),
                            "options",
                            argument_options.clone(),
                            scoped_id.with_child_index(6),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(7),
                                None,
                                "ElementSelect[settings: [selected]]",
                            ),
                            "selected",
                            argument_selected.clone(),
                            scoped_id.with_child_index(7),
                            actor_context.scope.clone(),
                        ),
                    ],
                ),
                scoped_id.with_child_index(2),
                actor_context.scope,
            ),
        ],
    )))
}

/// ```text
/// Element/svg(
///     element<[event?<[click?: LINK]>]>
///     style<[width?: N, height?: N]>
///     children<List<INTO_ELEMENT>>
/// ) -> ELEMENT_SVG
/// ```
pub fn function_element_svg(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_children] = arguments.as_slice() else {
        panic!("Element/svg expects 3 arguments")
    };
    let scoped_id = function_call_persistence_id;

    let event_actor = event_actor_from_constant_element(argument_element, actor_context.scope_id());

    stream::once(future::ready(TaggedObject::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/svg(..) -> ElementSvg[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementSvg",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "ElementSvg[element]",
                ),
                "element",
                argument_element.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(function_call_id.with_child_id(7), None, "ElementSvg[event]"),
                "event",
                event_actor,
                scoped_id.with_child_index(7),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "ElementSvg[settings]",
                ),
                "settings",
                Object::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(3),
                        None,
                        "ElementSvg[settings: [..]]",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    [
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(4),
                                None,
                                "ElementSvg[settings: [style]]",
                            ),
                            "style",
                            argument_style.clone(),
                            scoped_id.with_child_index(4),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(5),
                                None,
                                "ElementSvg[settings: [children]]",
                            ),
                            "children",
                            argument_children.clone(),
                            scoped_id.with_child_index(5),
                            actor_context.scope.clone(),
                        ),
                    ],
                ),
                scoped_id.with_child_index(2),
                actor_context.scope,
            ),
        ],
    )))
}

/// ```text
/// Element/svg_circle(
///     element<[event?<[click?: LINK, context_menu?: LINK]>]>
///     cx<Number>
///     cy<Number>
///     r<Number>
///     style<[fill?: Text, stroke?: Text, stroke_width?: N]>
/// ) -> ELEMENT_SVG_CIRCLE
/// ```
pub fn function_element_svg_circle(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [
        argument_element,
        argument_cx,
        argument_cy,
        argument_r,
        argument_style,
    ] = arguments.as_slice()
    else {
        panic!("Element/svg_circle expects 5 arguments")
    };
    let scoped_id = function_call_persistence_id;

    let event_actor = event_actor_from_constant_element(argument_element, actor_context.scope_id());

    stream::once(future::ready(TaggedObject::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/svg_circle(..) -> ElementSvgCircle[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "ElementSvgCircle",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "ElementSvgCircle[element]",
                ),
                "element",
                argument_element.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(9),
                    None,
                    "ElementSvgCircle[event]",
                ),
                "event",
                event_actor,
                scoped_id.with_child_index(9),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "ElementSvgCircle[settings]",
                ),
                "settings",
                Object::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(3),
                        None,
                        "ElementSvgCircle[settings: [..]]",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    [
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(4),
                                None,
                                "ElementSvgCircle[settings: [cx]]",
                            ),
                            "cx",
                            argument_cx.clone(),
                            scoped_id.with_child_index(4),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(5),
                                None,
                                "ElementSvgCircle[settings: [cy]]",
                            ),
                            "cy",
                            argument_cy.clone(),
                            scoped_id.with_child_index(5),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(6),
                                None,
                                "ElementSvgCircle[settings: [r]]",
                            ),
                            "r",
                            argument_r.clone(),
                            scoped_id.with_child_index(6),
                            actor_context.scope.clone(),
                        ),
                        Variable::new_arc(
                            ConstructInfo::new(
                                function_call_id.with_child_id(7),
                                None,
                                "ElementSvgCircle[settings: [style]]",
                            ),
                            "style",
                            argument_style.clone(),
                            scoped_id.with_child_index(7),
                            actor_context.scope.clone(),
                        ),
                    ],
                ),
                scoped_id.with_child_index(2),
                actor_context.scope,
            ),
        ],
    )))
}

/// ```text
/// Element/label(
///     element<[event?<[double_click?: LINK]>, hovered?: LINK, nearby_element?: ...]>
///     style<[]>
///     label<Text | Element>
/// ) -> ELEMENT_LABEL
/// ```
pub fn function_element_label(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_label] = arguments.as_slice() else {
        panic!("Element/label expects 3 arguments")
    };
    let scoped_id = function_call_persistence_id;

    let event_actor = event_actor_from_constant_element(argument_element, actor_context.scope_id());

    stream::once(future::ready(TaggedObject::new_value(
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
    )))
}

/// ```text
/// Element/paragraph(
///     element<[]>
///     style<[]>
///     contents<List<Text | Link | ...>>
/// ) -> ELEMENT_PARAGRAPH
/// ```
pub fn function_element_paragraph(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_element, argument_style, argument_contents] = arguments.as_slice() else {
        panic!("Element/paragraph expects 3 arguments")
    };
    let scoped_id = function_call_persistence_id;
    stream::once(future::ready(TaggedObject::new_value(
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
    )))
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [
        argument_element,
        argument_style,
        argument_label,
        argument_to,
        argument_new_tab,
    ] = arguments.as_slice()
    else {
        panic!("Element/link expects 5 arguments")
    };
    let scoped_id = function_call_persistence_id;
    stream::once(future::ready(TaggedObject::new_value(
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
    )))
}

// @TODO refactor
/// ```text
/// Math/sum(increment<Number>) -> Number
/// ``````
pub fn function_math_sum(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
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
    .chain(
        stream::once(async move {
            argument_increment_for_chain.stream().map(|value| State {
                input_value_idempotency_key: Some(value.idempotency_key()),
                sum: value.expect_number().number(),
                output_value_idempotency_key: None,
            })
        })
        .flatten(),
    )
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
                    storage.save_state(function_call_persistence_id, &state);
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
                    function_call_id.with_child_id(format!("Math/sum result v.{result_version}")),
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

/// Math/round(number) -> Number
pub fn function_math_round(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_number] = arguments.as_slice() else {
        panic!("Math/round expects 1 argument")
    };
    argument_number.clone().stream().map(move |value| {
        let number = match &value {
            Value::Number(n, _) => n.number(),
            _ => panic!("Math/round expects a Number value"),
        };
        Number::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, "Math/round result"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            number.round(),
        )
    })
}

/// Math/min(a, b) -> Number
pub fn function_math_min(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_a, argument_b] = arguments.as_slice() else {
        panic!("Math/min expects 2 arguments")
    };
    enum Input {
        A(f64),
        B(f64),
    }
    let a_stream = argument_a.clone().stream().map(|v| match &v {
        Value::Number(n, _) => Input::A(n.number()),
        _ => panic!("Math/min expects Number arguments"),
    });
    let b_stream = argument_b.clone().stream().map(|v| match &v {
        Value::Number(n, _) => Input::B(n.number()),
        _ => panic!("Math/min expects Number arguments"),
    });
    stream::select(a_stream, b_stream)
        .scan(
            (None::<f64>, None::<f64>),
            move |(last_a, last_b), input| {
                match input {
                    Input::A(val) => *last_a = Some(val),
                    Input::B(val) => *last_b = Some(val),
                }
                if let (Some(a), Some(b)) = (*last_a, *last_b) {
                    future::ready(Some(Some(Number::new_value(
                        ConstructInfo::new(
                            function_call_id.with_child_id(0),
                            None,
                            "Math/min result",
                        ),
                        construct_context.clone(),
                        ValueIdempotencyKey::new(),
                        a.min(b),
                    ))))
                } else {
                    future::ready(Some(None))
                }
            },
        )
        .filter_map(future::ready)
}

/// Math/max(a, b) -> Number
pub fn function_math_max(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_a, argument_b] = arguments.as_slice() else {
        panic!("Math/max expects 2 arguments")
    };
    enum Input {
        A(f64),
        B(f64),
    }
    let a_stream = argument_a.clone().stream().map(|v| match &v {
        Value::Number(n, _) => Input::A(n.number()),
        _ => panic!("Math/max expects Number arguments"),
    });
    let b_stream = argument_b.clone().stream().map(|v| match &v {
        Value::Number(n, _) => Input::B(n.number()),
        _ => panic!("Math/max expects Number arguments"),
    });
    stream::select(a_stream, b_stream)
        .scan(
            (None::<f64>, None::<f64>),
            move |(last_a, last_b), input| {
                match input {
                    Input::A(val) => *last_a = Some(val),
                    Input::B(val) => *last_b = Some(val),
                }
                if let (Some(a), Some(b)) = (*last_a, *last_b) {
                    future::ready(Some(Some(Number::new_value(
                        ConstructInfo::new(
                            function_call_id.with_child_id(0),
                            None,
                            "Math/max result",
                        ),
                        construct_context.clone(),
                        ValueIdempotencyKey::new(),
                        a.max(b),
                    ))))
                } else {
                    future::ready(Some(None))
                }
            },
        )
        .filter_map(future::ready)
}

// @TODO remember configuration?
/// ```text
/// Timer/interval(duration<Duration[seconds<Number> | milliseconds<Number>]>) -> []
/// ```
pub fn function_timer_interval(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_duration] = arguments.as_slice() else {
        panic!("Unexpected argument count")
    };
    let argument_duration_for_stream = argument_duration.clone();
    let duration_stream = duration_actor_current_and_future_ms(argument_duration_for_stream)
        .scan(None::<u32>, |last_ms, milliseconds| {
            let normalized_ms = milliseconds.round().max(0.0).min(u32::MAX as f64) as u32;
            if *last_ms == Some(normalized_ms) {
                future::ready(Some(None))
            } else {
                *last_ms = Some(normalized_ms);
                future::ready(Some(Some(normalized_ms)))
            }
        })
        .filter_map(future::ready);

    let (tick_sender, tick_receiver) = mpsc::channel::<Value>(16);

    let driver = zoon::Task::start_droppable(async move {
        let mut tick_sender = tick_sender;
        let mut duration_stream = Box::pin(duration_stream.fuse());
        let mut current_milliseconds = None::<u32>;
        let mut result_version = 0u64;

        loop {
            let Some(milliseconds) = current_milliseconds else {
                current_milliseconds = duration_stream.as_mut().next().await;
                if current_milliseconds.is_none() {
                    break;
                }
                continue;
            };

            let mut duration_stream_ref = duration_stream.as_mut();
            let next_duration = duration_stream_ref.next().fuse();
            let interval_sleep = Timer::sleep(milliseconds).fuse();
            pin_mut!(next_duration);
            pin_mut!(interval_sleep);

            select! {
                next_milliseconds = next_duration => {
                    match next_milliseconds {
                        Some(next_milliseconds) => current_milliseconds = Some(next_milliseconds),
                        None => break,
                    }
                }
                _ = interval_sleep => {
                    let output_value = Object::new_value(
                        ConstructInfo::new(
                            function_call_id.with_child_id(format!("Timer/interval result v.{result_version}")),
                            None,
                            "Timer/interval(.. ) -> [..]",
                        ),
                        construct_context.clone(),
                        ValueIdempotencyKey::new(),
                        [],
                    );
                    if tick_sender.send(output_value).await.is_err() {
                        break;
                    }
                    result_version += 1;
                }
            }
        }
    });

    stream::unfold(
        (tick_receiver, driver),
        |(mut receiver, driver)| async move {
            receiver
                .next()
                .await
                .map(|value| (value, (receiver, driver)))
        },
    )
}

// --- Text functions ---

/// Text/empty constant
pub fn function_text_empty(
    _arguments: Arc<Vec<ActorHandle>>,
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
    _arguments: Arc<Vec<ActorHandle>>,
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text] = arguments.as_slice() else {
        panic!("Text/trim expects 1 argument")
    };
    argument_text.clone().stream().map(move |value| {
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
/// Deduplicated: only emits when the result actually changes
pub fn function_text_is_empty(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text] = arguments.as_slice() else {
        panic!("Text/is_empty expects 1 argument")
    };
    argument_text
        .clone()
        .stream()
        .scan(None::<bool>, move |last_result, value| {
            let text = match &value {
                Value::Text(t, _) => t.text(),
                _ => panic!("Text/is_empty expects a Text value"),
            };
            let current_result = text.is_empty();

            // Only emit when result actually changes
            if *last_result != Some(current_result) {
                *last_result = Some(current_result);
                let tag = if current_result { "True" } else { "False" };
                future::ready(Some(Some(Tag::new_value(
                    ConstructInfo::new(
                        function_call_id.with_child_id(0),
                        None,
                        "Text/is_empty result",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    tag.to_string(),
                ))))
            } else {
                future::ready(Some(None))
            }
        })
        .filter_map(future::ready)
}

/// Text/is_not_empty(text) -> Tag (True/False)
/// Deduplicated: only emits when the result actually changes
pub fn function_text_is_not_empty(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text] = arguments.as_slice() else {
        panic!("Text/is_not_empty expects 1 argument")
    };
    argument_text
        .clone()
        .stream()
        .scan(None::<bool>, move |last_result, value| {
            let text = match &value {
                Value::Text(t, _) => t.text(),
                _ => panic!("Text/is_not_empty expects a Text value"),
            };
            let current_result = !text.is_empty();

            // Only emit when result actually changes
            if *last_result != Some(current_result) {
                *last_result = Some(current_result);
                let tag = if current_result { "True" } else { "False" };
                future::ready(Some(Some(Tag::new_value(
                    ConstructInfo::new(
                        function_call_id.with_child_id(0),
                        None,
                        "Text/is_not_empty result",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    tag.to_string(),
                ))))
            } else {
                future::ready(Some(None))
            }
        })
        .filter_map(future::ready)
}

/// Text/to_number(text) -> Number | NaN tag
/// Parses text to f64. Returns Number on success, NaN tag on failure.
pub fn function_text_to_number(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text] = arguments.as_slice() else {
        panic!("Text/to_number expects 1 argument")
    };
    argument_text.clone().stream().map(move |value| {
        let text = match &value {
            Value::Text(t, _) => t.text(),
            _ => panic!("Text/to_number expects a Text value"),
        };
        match text.trim().parse::<f64>() {
            Ok(number) => Number::new_value(
                ConstructInfo::new(
                    function_call_id.with_child_id(0),
                    None,
                    "Text/to_number result",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                number,
            ),
            Err(_) => Tag::new_value(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "Text/to_number NaN",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                "NaN".to_string(),
            ),
        }
    })
}

/// Text/starts_with(text, prefix) -> Tag (True/False)
/// Deduplicated: only emits when the result actually changes
pub fn function_text_starts_with(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text, argument_prefix] = arguments.as_slice() else {
        panic!("Text/starts_with expects 2 arguments")
    };
    enum Input {
        Text(String),
        Prefix(String),
    }
    let text_stream = text_actor_current_and_future_strings(argument_text.clone()).map(Input::Text);
    let prefix_stream =
        text_actor_current_and_future_strings(argument_prefix.clone()).map(Input::Prefix);
    stream::select(text_stream, prefix_stream)
        .scan(
            (None::<String>, None::<String>, None::<bool>),
            move |(last_text, last_prefix, last_result), input| {
                match input {
                    Input::Text(t) => *last_text = Some(t),
                    Input::Prefix(p) => *last_prefix = Some(p),
                }
                // Only compute when both inputs have arrived
                if let (Some(text), Some(prefix)) = (last_text.as_ref(), last_prefix.as_ref()) {
                    let current_result = text.starts_with(prefix.as_str());
                    if *last_result != Some(current_result) {
                        *last_result = Some(current_result);
                        let tag = if current_result { "True" } else { "False" };
                        future::ready(Some(Some(Tag::new_value(
                            ConstructInfo::new(
                                function_call_id.with_child_id(0),
                                None,
                                "Text/starts_with result",
                            ),
                            construct_context.clone(),
                            ValueIdempotencyKey::new(),
                            tag.to_string(),
                        ))))
                    } else {
                        future::ready(Some(None))
                    }
                } else {
                    // Don't emit until both inputs are available
                    future::ready(Some(None))
                }
            },
        )
        .filter_map(future::ready)
}

// --- Bool functions ---

/// Bool/not(value) -> Tag (True/False)
pub fn function_bool_not(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_value] = arguments.as_slice() else {
        panic!("Bool/not expects 1 argument")
    };
    argument_value.clone().stream().map(move |value| {
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let argument_value = arguments[0].clone();
    let argument_when = arguments[1].clone();

    // Two independent streams merged via select:
    // - value_stream: pipe input (initial bool + updates from toggle-all)
    // - when_stream: toggle trigger (individual checkbox click)
    enum Msg {
        SetValue(bool),
        Toggle,
    }

    let value_stream = argument_value.stream().filter_map(|value| {
        future::ready(match &value {
            Value::Tag(tag, _) => Some(Msg::SetValue(tag.tag() == "True")),
            _ => None,
        })
    });

    let when_stream = argument_when.stream().map(|_| Msg::Toggle);

    stream::select(value_stream, when_stream).scan(None::<bool>, move |state, msg| {
        match msg {
            Msg::SetValue(v) => *state = Some(v),
            Msg::Toggle => {
                let current = state.unwrap_or(false);
                *state = Some(!current);
            }
        }
        let is_true = state.unwrap_or(false);
        let result_tag = if is_true { "True" } else { "False" };
        future::ready(Some(Tag::new_value(
            ConstructInfo::new(
                function_call_id.with_child_id(0),
                None,
                "Bool/toggle result",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            result_tag.to_string(),
        )))
    })
}

/// Bool/or(this, that) -> Tag (True/False)
/// Returns True if either this or that is True
pub fn function_bool_or(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let this_actor = arguments[0].clone();
    let that_actor = arguments[1].clone();

    // Combine both boolean streams using select
    let this_stream = this_actor.stream().map(|v| (true, v));
    let that_stream = that_actor.stream().map(|v| (false, v));

    stream::select(this_stream, that_stream)
        .scan(
            (None::<bool>, None::<bool>),
            move |state, (is_this, value)| {
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
            },
        )
        .filter_map(move |(this_bool, that_bool)| {
            let construct_context = construct_context.clone();
            let function_call_id = function_call_id.clone();
            future::ready(match (this_bool, that_bool) {
                (Some(a), Some(b)) => {
                    let result = a || b;
                    let tag = if result { "True" } else { "False" };
                    Some(Tag::new_value(
                        ConstructInfo::new(
                            function_call_id.with_child_id(0),
                            None,
                            "Bool/or result",
                        ),
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

fn list_actor_stream(list_actor: ActorHandle) -> LocalBoxStream<'static, Arc<List>> {
    list_actor
        .stream()
        .filter_map(move |value| {
            future::ready(match &value {
                Value::List(list, _) => Some(list.clone()),
                _ => None,
            })
        })
        .boxed_local()
}

fn current_list_items_stream_from_actor(
    list_actor: ActorHandle,
) -> LocalBoxStream<'static, Vec<ActorHandle>> {
    switch_map(list_actor_stream(list_actor), current_list_items_stream)
}

fn list_change_stream_from_actor(list_actor: ActorHandle) -> LocalBoxStream<'static, ListChange> {
    switch_map(list_actor_stream(list_actor), |list| {
        list.stream().boxed_local()
    })
}

fn selected_list_item_stream(
    list_actor: ActorHandle,
    index_actor: ActorHandle,
) -> LocalBoxStream<'static, Option<ActorHandle>> {
    enum Input {
        Items(Vec<ActorHandle>),
        Index(Option<usize>),
    }

    let items_stream = current_list_items_stream_from_actor(list_actor).map(Input::Items);
    let index_stream = number_actor_current_and_future_numbers(index_actor).map(|number| {
        Input::Index(if number >= 1.0 {
            Some((number as usize) - 1)
        } else {
            None
        })
    });

    let selection_stream = stream::select(items_stream, index_stream)
        .scan(
            (None::<Vec<ActorHandle>>, None::<Option<usize>>),
            |(items, index), input| {
                match input {
                    Input::Items(next_items) => *items = Some(next_items),
                    Input::Index(next_index) => *index = Some(next_index),
                }
                future::ready(Some(match (items.as_ref(), *index) {
                    (Some(current_items), Some(current_index)) => {
                        Some(current_index.and_then(|idx| current_items.get(idx).cloned()))
                    }
                    _ => None,
                }))
            },
        )
        .filter_map(future::ready);

    dedupe_selected_actor_stream(selection_stream)
}

fn list_length_stream(list_actor: ActorHandle) -> LocalBoxStream<'static, usize> {
    current_list_items_stream_from_actor(list_actor)
        .scan(None::<usize>, move |last_len, items| {
            let current_len = items.len();
            if *last_len == Some(current_len) {
                future::ready(Some(None))
            } else {
                *last_len = Some(current_len);
                future::ready(Some(Some(current_len)))
            }
        })
        .filter_map(future::ready)
        .boxed_local()
}

fn current_list_items_stream(list: Arc<List>) -> LocalBoxStream<'static, Vec<ActorHandle>> {
    list.stream()
        .scan(
            (Vec::<ActorHandle>::new(), None::<Vec<ActorId>>),
            |(items, last_key), change| {
                change.apply_to_vec(items);
                let current_key = items.iter().map(|item| item.actor_id()).collect::<Vec<_>>();
                if last_key.as_ref() == Some(&current_key) {
                    future::ready(Some(None))
                } else {
                    *last_key = Some(current_key);
                    future::ready(Some(Some(items.clone())))
                }
            },
        )
        .filter_map(future::ready)
        .boxed_local()
}

/// List/is_empty() -> Tag (True/False)
/// Checks if the piped list is empty
/// Deduplicated: only emits when the result actually changes
pub fn function_list_is_empty(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    list_length_stream(arguments[0].clone()).map(move |current_len| {
        let tag = if current_len == 0 { "True" } else { "False" };
        Tag::new_value(
            ConstructInfo::new(
                function_call_id.with_child_id(0),
                None,
                "List/is_empty result",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            tag.to_string(),
        )
    })
}

/// List/count -> Number
/// Returns the count of items in the list
/// Deduplicated: only emits when the count actually changes
pub fn function_list_count(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    list_length_stream(arguments[0].clone()).map(move |current_len| {
        Number::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, "List/count result"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            current_len as f64,
        )
    })
}

/// List/is_not_empty() -> Tag (True/False)
/// Checks if the piped list is not empty (inverse of List/is_empty)
/// Deduplicated: only emits when the result actually changes
pub fn function_list_is_not_empty(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    list_length_stream(arguments[0].clone()).map(move |current_len| {
        let tag = if current_len == 0 { "False" } else { "True" };
        Tag::new_value(
            ConstructInfo::new(
                function_call_id.with_child_id(0),
                None,
                "List/is_not_empty result",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            tag.to_string(),
        )
    })
}

/// List/append(item: value) -> List
/// Appends an item to the list when the item stream produces a value
pub fn function_list_append(
    arguments: Arc<Vec<ActorHandle>>,
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
        return list_actor.stream().left_stream();
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
        let actor_context_for_append = actor_context.clone();

        // Tag changes with their source so we can ensure proper ordering
        enum TaggedChange {
            FromList(ListChange),
            FromAppend(ListChange),
        }

        let list_changes =
            list_change_stream_from_actor(list_actor.clone()).map(TaggedChange::FromList);

        let append_changes = item_actor.clone().stream().map(move |value| {
            // When item stream produces a value, create a new constant ValueActor
            // containing that specific value and push it.
            // Note: SKIP is not a Value - it's a stream behavior where streams end without
            // producing values. If item is SKIP, this map closure is never called.
            let new_item_actor = create_constant_actor(
                PersistenceId::new(),
                value,
                actor_context_for_append.scope_id(),
            );
            TaggedChange::FromAppend(ListChange::Push {
                item: new_item_actor,
            })
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
                },
            )
            .flat_map(|changes| stream::iter(changes))
    };

    let list = List::new_with_change_stream(
        ConstructInfo::new(
            function_call_id.with_child_id(ulid::Ulid::new().to_string()),
            None,
            "List/append result",
        ),
        actor_context,
        change_stream,
        (list_actor, item_actor),
    );

    stream::once(future::ready(Value::List(
        Arc::new(list),
        ValueMetadata::new(ValueIdempotencyKey::new()),
    )))
    .right_stream()
}

/// List/clear(on: stream) -> List
/// Clears all items from the list when the trigger stream emits any value
pub fn function_list_clear(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    // arguments[0] = the list (piped)
    // arguments[1] = the trigger stream (on: xxx)
    let list_actor = arguments[0].clone();

    // If trigger argument is not provided, just forward the list unchanged
    if arguments.len() < 2 {
        return list_actor.stream().left_stream();
    }

    let trigger_actor = arguments[1].clone();

    // Similar pattern to List/append:
    // 1. Forward all changes from the original list
    // 2. When trigger fires, emit Clear AND clear recorded calls storage
    let change_stream = {
        enum TaggedChange {
            FromList(ListChange),
            Clear,
        }

        let list_changes = current_list_items_stream_from_actor(list_actor.clone())
            .map(|items| ListChange::Replace {
                items: Arc::from(items),
            })
            .map(TaggedChange::FromList);

        // When trigger stream emits any value, emit Clear
        let clear_changes = trigger_actor
            .clone()
            .stream()
            .map(|_value| TaggedChange::Clear);

        // Merge both streams, use scan for proper ordering
        stream::select(list_changes, clear_changes)
            .scan((None::<Vec<ActorHandle>>, false), |state, tagged_change| {
                let (tracked_items, has_pending_clear) = state;

                let changes_to_emit = match tagged_change {
                    TaggedChange::FromList(ListChange::Replace { items }) => {
                        *tracked_items = Some(items.to_vec());
                        if *has_pending_clear {
                            *has_pending_clear = false;
                            if let Some(current_items) = tracked_items.as_mut() {
                                clear_source_storage_for_items(current_items);
                                current_items.clear();
                            }
                            vec![ListChange::Replace { items }, ListChange::Clear]
                        } else {
                            vec![ListChange::Replace { items }]
                        }
                    }
                    TaggedChange::FromList(_) => {
                        unreachable!("list snapshot feed only emits Replace")
                    }
                    TaggedChange::Clear => {
                        if let Some(current_items) = tracked_items.as_mut() {
                            clear_source_storage_for_items(current_items);
                            current_items.clear();
                            vec![ListChange::Clear]
                        } else {
                            // Buffer the clear until first list change arrives
                            *has_pending_clear = true;
                            vec![]
                        }
                    }
                };

                future::ready(Some(changes_to_emit))
            })
            .flat_map(|changes| stream::iter(changes))
    };

    let list = List::new_with_change_stream_and_persistence(
        ConstructInfo::new(function_call_id.with_child_id(0), None, "List/clear result"),
        construct_context,
        actor_context,
        change_stream,
        (list_actor, trigger_actor),
        function_call_persistence_id,
    );

    stream::once(future::ready(Value::List(
        Arc::new(list),
        ValueMetadata::new(ValueIdempotencyKey::new()),
    )))
    .right_stream()
}

/// Clear recorded calls storage for all items that have source origins.
/// Called when List/clear triggers to ensure items don't restore on next Run.
fn clear_source_storage_for_items(items: &[ActorHandle]) {
    use std::collections::HashSet;
    use zoon::{WebStorage, local_storage};

    // Collect unique source storage keys
    let mut source_keys: HashSet<String> = HashSet::new();
    for item in items {
        if let Some(origin) = item.list_item_origin() {
            source_keys.insert(origin.source_storage_key.clone());
        }
    }

    // Clear each source storage key
    for key in source_keys {
        if LOG_DEBUG {
            zoon::println!("[DEBUG] List/clear: Clearing source storage key: {}", key);
        }
        local_storage().remove(&key);
    }
}

/// List/last() -> Value
/// Returns the current value of the last item in the list.
/// Re-emits whenever the last item changes (list grows/shrinks or item value updates).
/// Emits nothing (stream pending) when the list is empty.
pub fn function_list_last(
    arguments: Arc<Vec<ActorHandle>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let list_actor = arguments[0].clone();
    // Get the last item's ActorHandle, re-emitting whenever the list changes
    let last_item_stream = dedupe_selected_actor_stream(
        current_list_items_stream_from_actor(list_actor).map(|items| items.last().cloned()),
    );
    // switch_map: when the last item changes identity, cancel old subscription and start new
    switch_map(last_item_stream, |item_opt| match item_opt {
        Some(actor) => actor.current_or_future_stream(),
        None => stream::empty().boxed_local(),
    })
}

/// List/remove_last() -> List
/// Removes the last item from the list when triggered (piped value is the trigger).
/// Returns the modified list.
pub fn function_list_remove_last(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    // arguments[0] = the list (piped)
    // arguments[1] = the trigger stream
    let list_actor = arguments[0].clone();

    if arguments.len() < 2 {
        return list_actor.stream().left_stream();
    }

    let trigger_actor = arguments[1].clone();

    let change_stream = {
        enum TaggedChange {
            FromList(ListChange),
            RemoveLast,
        }

        let list_changes = current_list_items_stream_from_actor(list_actor.clone())
            .map(|items| ListChange::Replace {
                items: Arc::from(items),
            })
            .map(TaggedChange::FromList);

        let remove_changes = trigger_actor
            .clone()
            .stream()
            .map(|_value| TaggedChange::RemoveLast);

        stream::select(list_changes, remove_changes)
            .scan((None::<Vec<ActorHandle>>, false), |state, tagged_change| {
                let (tracked_items, has_pending_remove) = state;

                let change_to_emit = match tagged_change {
                    TaggedChange::FromList(ListChange::Replace { items }) => {
                        *tracked_items = Some(items.to_vec());
                        if *has_pending_remove {
                            *has_pending_remove = false;
                            if let Some(current_items) = tracked_items.as_mut() {
                                if !current_items.is_empty() {
                                    current_items.pop();
                                    Some(vec![ListChange::Replace { items }, ListChange::Pop])
                                } else {
                                    Some(vec![ListChange::Replace { items }])
                                }
                            } else {
                                Some(vec![ListChange::Replace { items }])
                            }
                        } else {
                            Some(vec![ListChange::Replace { items }])
                        }
                    }
                    TaggedChange::FromList(_) => {
                        unreachable!("list snapshot feed only emits Replace")
                    }
                    TaggedChange::RemoveLast => {
                        if tracked_items.is_none() {
                            *has_pending_remove = true;
                            None
                        } else {
                            tracked_items.as_mut().and_then(|current_items| {
                                if !current_items.is_empty() {
                                    current_items.pop();
                                    Some(vec![ListChange::Pop])
                                } else {
                                    None
                                }
                            })
                        }
                    }
                };

                future::ready(Some(change_to_emit))
            })
            .filter_map(future::ready)
            .flat_map(stream::iter)
    };

    let list = List::new_with_change_stream_and_persistence(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "List/remove_last result list",
        ),
        construct_context,
        actor_context,
        change_stream,
        (list_actor, trigger_actor),
        function_call_persistence_id,
    );

    stream::once(future::ready(Value::List(
        Arc::new(list),
        ValueMetadata::new(ValueIdempotencyKey::new()),
    )))
    .right_stream()
}

/// List/latest() -> Value
/// Merges a list of streams, emitting whenever any stream produces a value
/// Returns the value from the stream that most recently produced
pub fn function_list_latest(
    arguments: Arc<Vec<ActorHandle>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let list_actor = arguments[0].clone();

    let items_stream = current_list_items_stream_from_actor(list_actor);

    switch_map(items_stream, move |items| {
        if items.is_empty() {
            stream::empty().boxed_local()
        } else {
            let streams: Vec<_> = items.into_iter().map(|item| item.stream()).collect();
            stream::select_all(streams)
                .ready_chunks(16)
                .flat_map(|mut chunk| {
                    chunk.sort_by_key(|value| value.emission_seq());
                    stream::iter(chunk)
                })
                .boxed_local()
        }
    })
}

// --- Router functions ---

/// Get the current URL pathname from the browser
fn get_current_pathname() -> String {
    window()
        .location()
        .pathname()
        .unwrap_or_else(|_| "/".to_string())
}

/// Router/route() -> Text
/// Returns the current route/URL path as a reactive stream
/// Updates whenever the URL changes (via popstate event)
pub fn function_router_route(
    _arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    // Create a bounded channel for route changes (8 should be plenty for navigation)
    let (mut route_sender, route_receiver) = mpsc::channel::<String>(8);

    // Send initial route
    let initial_path = get_current_pathname();
    if LOG_DEBUG {
        zoon::println!("[ROUTER] Initial route: '{}'", initial_path);
    }
    if let Err(e) = route_sender.try_send(initial_path) {
        if LOG_DEBUG {
            zoon::println!("[ROUTER] Failed to send initial route: {e}");
        }
    }

    // Set up popstate listener for browser back/forward navigation
    let popstate_closure: Closure<dyn Fn()> = Closure::new({
        let route_sender = route_sender.clone();
        move || {
            let path = get_current_pathname();
            if let Err(e) = route_sender.clone().try_send(path) {
                if LOG_DEBUG {
                    zoon::println!("[ROUTER] Failed to send popstate route: {e}");
                }
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
        if LOG_DEBUG {
            zoon::println!("[ROUTER] Emitting route: '{}'", path);
        }
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let route_actor = arguments[0].clone();

    route_actor.stream().map(move |value| {
        let route = match &value {
            Value::Text(text, _) => text.text().to_string(),
            _ => "/".to_string(),
        };
        if LOG_DEBUG {
            zoon::println!("[ROUTER] go_to called with route: '{}'", route);
        }

        // Navigate using browser history API
        if route.starts_with('/') {
            history()
                .push_state_with_url(&JsValue::NULL, "", Some(&route))
                .unwrap_throw();

            // Notify route listeners about the change
            ROUTE_SENDER.with(|cell| {
                if let Some(sender) = cell.borrow_mut().as_mut() {
                    if let Err(e) = sender.try_send(route) {
                        if LOG_DEBUG {
                            zoon::println!("[ROUTER] Failed to send go_to route: {e}");
                        }
                    }
                }
            });
        }

        Object::new_value(
            ConstructInfo::new(
                function_call_id.with_child_id(0),
                None,
                "Router/go_to result",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            [],
        )
    })
}

// --- Ulid functions ---

/// Ulid/generate() -> Text
pub fn function_ulid_generate(
    _arguments: Arc<Vec<ActorHandle>>,
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

use std::future::Future;
use std::pin::Pin;

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
                    let field_value =
                        resolve_actor_value_for_log(variable.value_actor(), timeout_ms).await;
                    fields.push(format!("{}: {}", name, field_value));
                }
                format!("[{}]", fields.join(", "))
            }
            Value::TaggedObject(tagged, _) => {
                let mut fields = Vec::new();
                for variable in tagged.variables() {
                    let name = variable.name().to_string();
                    let field_value =
                        resolve_actor_value_for_log(variable.value_actor(), timeout_ms).await;
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
            Value::Flushed(inner, _) => format!(
                "Flushed[{}]",
                resolve_value_for_log(*inner, timeout_ms).await
            ),
        }
    })
}

/// Async function to get value from a ValueActor for logging with timeout.
/// Returns `?` if no value arrives within the timeout.
fn resolve_actor_value_for_log(
    actor: ActorHandle,
    timeout_ms: u32,
) -> Pin<Box<dyn Future<Output = String>>> {
    Box::pin(async move {
        match actor.current_value() {
            Ok(value) => resolve_value_for_log(value, timeout_ms).await,
            Err(CurrentValueError::ActorDropped) => "?".to_string(),
            Err(CurrentValueError::NoValueYet) => {
                let timeout = Timer::sleep(timeout_ms);
                select! {
                    value = actor.value().fuse() => {
                        match value {
                            Ok(value) => resolve_value_for_log(value, timeout_ms).await,
                            Err(_) => "?".to_string(),
                        }
                    }
                    _ = timeout.fuse() => {
                        "?".to_string()
                    }
                }
            }
        }
    })
}

async fn actor_current_value_or_wait(actor: &ActorHandle) -> Option<Value> {
    match actor.current_value() {
        Ok(value) => Some(value),
        Err(CurrentValueError::NoValueYet) => actor.value().await.ok(),
        Err(CurrentValueError::ActorDropped) => None,
    }
}

fn extract_duration_ms_from_value_now(value: &Value) -> f64 {
    let duration_object = value.clone().expect_tagged_object("Duration");
    if let Some(seconds) = duration_object.variable("seconds") {
        if let Ok(value) = seconds.value_actor().current_value() {
            return value.expect_number().number() * 1000.0;
        }
    }
    if let Some(ms) = duration_object.variable("ms") {
        if let Ok(value) = ms.value_actor().current_value() {
            return value.expect_number().number();
        }
    }
    if let Some(milliseconds) = duration_object.variable("milliseconds") {
        if let Ok(value) = milliseconds.value_actor().current_value() {
            return value.expect_number().number();
        }
    }
    0.0
}

fn duration_value_current_and_future_ms(value: Value) -> LocalBoxStream<'static, f64> {
    let duration_object = value.expect_tagged_object("Duration");
    if let Some(seconds) = duration_object.variable("seconds") {
        seconds
            .value_actor()
            .current_or_future_stream()
            .map(|value| value.expect_number().number() * 1000.0)
            .boxed_local()
    } else if let Some(milliseconds) = duration_object.variable("milliseconds") {
        milliseconds
            .value_actor()
            .current_or_future_stream()
            .map(|value| value.expect_number().number())
            .boxed_local()
    } else {
        panic!("Failed to get property 'seconds' or 'milliseconds' from tagged object 'Duration'");
    }
}

fn duration_actor_current_and_future_ms(
    duration_actor: ActorHandle,
) -> LocalBoxStream<'static, f64> {
    duration_actor
        .current_or_future_stream()
        .map(duration_value_current_and_future_ms)
        .flatten()
        .boxed_local()
}

fn count_actor_current_and_future_values(
    count_actor: ActorHandle,
) -> LocalBoxStream<'static, Value> {
    count_actor.current_or_future_stream()
}

fn text_actor_current_and_future_strings(actor: ActorHandle) -> LocalBoxStream<'static, String> {
    actor
        .current_or_future_stream()
        .map(|value| match &value {
            Value::Text(text, _) => text.text().to_string(),
            _ => panic!("expected Text value"),
        })
        .boxed_local()
}

fn number_actor_current_and_future_numbers(actor: ActorHandle) -> LocalBoxStream<'static, f64> {
    actor
        .current_or_future_stream()
        .map(|value| match &value {
            Value::Number(number, _) => number.number(),
            _ => panic!("expected Number value"),
        })
        .boxed_local()
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
async fn extract_log_options_from_with(with_actor: ActorHandle) -> LogOptions {
    let mut options = LogOptions::default();

    // Get the 'with' object value
    let with_value = match actor_current_value_or_wait(&with_actor).await {
        Some(v) => v,
        None => return options,
    };

    // Check if it's an Object and extract the fields
    if let Value::Object(obj, _) = with_value {
        // Extract label
        if let Some(label_var) = obj.variable("label") {
            if let Some(label_value) = actor_current_value_or_wait(&label_var.value_actor()).await {
                options.label = Some(resolve_value_for_log(label_value, options.timeout_ms).await);
            }
        }

        // Extract timeout from Duration[seconds: N] or Duration[milliseconds: N]
        if let Some(timeout_var) = obj.variable("timeout") {
            if let Some(timeout_value) =
                actor_current_value_or_wait(&timeout_var.value_actor()).await
            {
                if let Value::TaggedObject(tagged, _) = timeout_value {
                    if tagged.tag() == "Duration" {
                        if let Some(seconds_var) = tagged.variable("seconds") {
                            if let Some(Value::Number(num, _)) =
                                actor_current_value_or_wait(&seconds_var.value_actor()).await
                            {
                                options.timeout_ms =
                                    (num.number() * 1000.0).max(0.0).min(u32::MAX as f64) as u32;
                            }
                        } else if let Some(milliseconds_var) = tagged.variable("milliseconds") {
                            if let Some(Value::Number(num, _)) =
                                actor_current_value_or_wait(&milliseconds_var.value_actor()).await
                            {
                                options.timeout_ms =
                                    num.number().max(0.0).min(u32::MAX as f64) as u32;
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
    arguments: Arc<Vec<ActorHandle>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let value_actor = arguments[0].clone();
    let with_actor = arguments.get(1).cloned();

    // Create a bounded channel for log requests - actor model compliant pattern
    let (log_sender, log_receiver) = mpsc::channel::<(Value, Option<ActorHandle>)>(16);

    // Create a retained task that processes log messages
    let log_actor = zoon::Task::start_droppable(async move {
        let mut receiver = log_receiver;
        while let Some((value, with_actor)) = receiver.next().await {
            // Extract options (label and timeout) from 'with' object
            let options = if let Some(with) = with_actor {
                extract_log_options_from_with(with).await
            } else {
                LogOptions::default()
            };
            // Resolve value with the configured timeout
            let value_str = resolve_value_for_log_with_timeout(value, options.timeout_ms).await;
            // Log with or without label
            match options.label {
                Some(label) if !label.is_empty() => {
                    zoon::println!("[INFO] {}: {}", label, value_str)
                }
                _ => zoon::println!("[INFO] {}", value_str),
            }
        }
    });

    value_actor.stream().then(move |value| {
        let mut sender = log_sender.clone();
        let with_actor = with_actor.clone();
        let _log_actor = &log_actor;
        async move {
            // Send log request to the actor (backpressure if channel full)
            if sender.send((value.clone(), with_actor)).await.is_err() {
                zoon::eprintln!("[Log/info] Failed to send log request - receiver dropped");
            }
            value
        }
    })
}

/// Log/error(value: T) -> T
/// Log/error(value: T, with: [label: Text, timeout: Duration]) -> T
/// Logs an error message to the console and passes through the input value.
/// Output format: `[ERROR] {label}: {value}` or `[ERROR] {value}`
/// The 'with' parameter accepts:
/// - label: Text label for the error message
/// - timeout: Duration[milliseconds: N] or Duration[seconds: N] for nested value resolution
pub fn function_log_error(
    arguments: Arc<Vec<ActorHandle>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let value_actor = arguments[0].clone();
    let with_actor = arguments.get(1).cloned();

    // Create a bounded channel for log requests - actor model compliant pattern
    let (log_sender, log_receiver) = mpsc::channel::<(Value, Option<ActorHandle>)>(16);

    // Create a retained task that processes log messages
    let log_actor = zoon::Task::start_droppable(async move {
        let mut receiver = log_receiver;
        while let Some((value, with_actor)) = receiver.next().await {
            // Extract options (label and timeout) from 'with' object
            let options = if let Some(with) = with_actor {
                extract_log_options_from_with(with).await
            } else {
                LogOptions::default()
            };
            // Resolve value with the configured timeout
            let value_str = resolve_value_for_log_with_timeout(value, options.timeout_ms).await;
            // Log with or without label
            match options.label {
                Some(label) if !label.is_empty() => {
                    zoon::eprintln!("[ERROR] {}: {}", label, value_str)
                }
                _ => zoon::eprintln!("[ERROR] {}", value_str),
            }
        }
    });

    value_actor.stream().then(move |value| {
        let mut sender = log_sender.clone();
        let with_actor = with_actor.clone();
        let _log_actor = &log_actor;
        async move {
            // Send log request to the actor (backpressure if channel full)
            if sender.send((value.clone(), with_actor)).await.is_err() {
                zoon::eprintln!("[Log/error] Failed to send log request - receiver dropped");
            }
            value
        }
    })
}

// --- Build functions ---

/// Build/succeed() -> Tag (Success)
/// Returns a successful build result
pub fn function_build_succeed(
    _arguments: Arc<Vec<ActorHandle>>,
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let error_actor = arguments[0].clone();
    error_actor.stream().map(move |value| {
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

// --- Element/text and Element/block ---

/// Element/text(element?, style, text) -> ElementText[settings[element, style, text]]
/// Simple text element with optional tag and styling.
pub fn function_element_text(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let (argument_element, argument_style, argument_text) = match arguments.as_slice() {
        [element, style, text] => (Some(element), style, text),
        [style, text] => (None, style, text),
        _ => panic!("Element/text expects 2 or 3 arguments"),
    };
    let scoped_id = function_call_persistence_id;

    let mut vars: Vec<Arc<Variable>> = Vec::new();

    if let Some(argument_element) = argument_element {
        vars.push(Variable::new_arc(
            ConstructInfo::new(
                function_call_id.with_child_id(1),
                None,
                "ElementText[element]",
            ),
            "element",
            argument_element.clone(),
            scoped_id.with_child_index(1),
            actor_context.scope.clone(),
        ));
    }

    vars.push(Variable::new_arc(
        ConstructInfo::new(
            function_call_id.with_child_id(2),
            None,
            "ElementText[settings]",
        ),
        "settings",
        Object::new_arc_value_actor(
            ConstructInfo::new(
                function_call_id.with_child_id(3),
                None,
                "ElementText[settings: [..]]",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context.clone(),
            [
                Variable::new_arc(
                    ConstructInfo::new(
                        function_call_id.with_child_id(4),
                        None,
                        "ElementText[settings: [style]]",
                    ),
                    "style",
                    argument_style.clone(),
                    scoped_id.with_child_index(4),
                    actor_context.scope.clone(),
                ),
                Variable::new_arc(
                    ConstructInfo::new(
                        function_call_id.with_child_id(5),
                        None,
                        "ElementText[settings: [text]]",
                    ),
                    "text",
                    argument_text.clone(),
                    scoped_id.with_child_index(5),
                    actor_context.scope.clone(),
                ),
            ],
        ),
        scoped_id.with_child_index(2),
        actor_context.scope,
    ));

    stream::once(future::ready(TaggedObject::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/text(..) -> ElementText[..]",
        ),
        construct_context,
        ValueIdempotencyKey::new(),
        "ElementText",
        vars,
    )))
}

/// Element/block(element?, style, child) -> ElementBlock[settings[element, style, child]]
/// Generic block element with a single child.
pub fn function_element_block(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let (argument_element, argument_style, argument_child) = match arguments.as_slice() {
        [element, style, child] => (Some(element), style, child),
        [style, child] => (None, style, child),
        _ => panic!("Element/block expects 2 or 3 arguments"),
    };
    let scoped_id = function_call_persistence_id;

    let mut vars: Vec<Arc<Variable>> = Vec::new();

    if let Some(argument_element) = argument_element {
        vars.push(Variable::new_arc(
            ConstructInfo::new(
                function_call_id.with_child_id(1),
                None,
                "ElementBlock[element]",
            ),
            "element",
            argument_element.clone(),
            scoped_id.with_child_index(1),
            actor_context.scope.clone(),
        ));
    }

    vars.push(Variable::new_arc(
        ConstructInfo::new(
            function_call_id.with_child_id(2),
            None,
            "ElementBlock[settings]",
        ),
        "settings",
        Object::new_arc_value_actor(
            ConstructInfo::new(
                function_call_id.with_child_id(3),
                None,
                "ElementBlock[settings: [..]]",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context.clone(),
            [
                Variable::new_arc(
                    ConstructInfo::new(
                        function_call_id.with_child_id(4),
                        None,
                        "ElementBlock[settings: [style]]",
                    ),
                    "style",
                    argument_style.clone(),
                    scoped_id.with_child_index(4),
                    actor_context.scope.clone(),
                ),
                Variable::new_arc(
                    ConstructInfo::new(
                        function_call_id.with_child_id(5),
                        None,
                        "ElementBlock[settings: [child]]",
                    ),
                    "child",
                    argument_child.clone(),
                    scoped_id.with_child_index(5),
                    actor_context.scope.clone(),
                ),
            ],
        ),
        scoped_id.with_child_index(2),
        actor_context.scope,
    ));

    stream::once(future::ready(TaggedObject::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Element/block(..) -> ElementBlock[..]",
        ),
        construct_context,
        ValueIdempotencyKey::new(),
        "ElementBlock",
        vars,
    )))
}

// --- Scene functions ---

/// Scene/new(root, lights?, geometry?) -> [root_element, lights?, geometry?]
/// Creates a new scene for DOM rendering. Accepts 1 or 3 arguments.
/// With 1 argument: just root element (backward compatible).
/// With 3 arguments: root element, lights array, and geometry config.
pub fn function_scene_new(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let scoped_id = function_call_persistence_id;

    let (argument_root, argument_lights, argument_geometry) = match arguments.as_slice() {
        [root] => (root, None, None),
        [root, lights, geometry] => (root, Some(lights), Some(geometry)),
        _ => panic!("Scene/new expects 1 or 3 arguments"),
    };

    let mut vars: Vec<Arc<Variable>> = Vec::new();

    vars.push(Variable::new_arc(
        ConstructInfo::new(
            function_call_id.with_child_id(1),
            None,
            "Scene/new(..) -> [root_element]",
        ),
        "root_element",
        argument_root.clone(),
        scoped_id.with_child_index(1),
        actor_context.scope.clone(),
    ));

    if let Some(argument_lights) = argument_lights {
        vars.push(Variable::new_arc(
            ConstructInfo::new(
                function_call_id.with_child_id(2),
                None,
                "Scene/new(..) -> [lights]",
            ),
            "lights",
            argument_lights.clone(),
            scoped_id.with_child_index(2),
            actor_context.scope.clone(),
        ));
    }

    if let Some(argument_geometry) = argument_geometry {
        vars.push(Variable::new_arc(
            ConstructInfo::new(
                function_call_id.with_child_id(3),
                None,
                "Scene/new(..) -> [geometry]",
            ),
            "geometry",
            argument_geometry.clone(),
            scoped_id.with_child_index(3),
            actor_context.scope.clone(),
        ));
    }

    stream::once(future::ready(Object::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Scene/new(..) -> []",
        ),
        construct_context,
        ValueIdempotencyKey::new(),
        vars,
    )))
}

// --- Light functions ---

/// Light/directional(azimuth, altitude, spread, intensity, color) -> DirectionalLight[...]
/// Pure data constructor for directional light configuration.
pub fn function_light_directional(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [
        arg_azimuth,
        arg_altitude,
        arg_spread,
        arg_intensity,
        arg_color,
    ] = arguments.as_slice()
    else {
        panic!("Light/directional expects 5 arguments")
    };
    let scoped_id = function_call_persistence_id;

    stream::once(future::ready(TaggedObject::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Light/directional(..) -> DirectionalLight[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "DirectionalLight",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "DirectionalLight[azimuth]",
                ),
                "azimuth",
                arg_azimuth.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "DirectionalLight[altitude]",
                ),
                "altitude",
                arg_altitude.clone(),
                scoped_id.with_child_index(2),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(3),
                    None,
                    "DirectionalLight[spread]",
                ),
                "spread",
                arg_spread.clone(),
                scoped_id.with_child_index(3),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(4),
                    None,
                    "DirectionalLight[intensity]",
                ),
                "intensity",
                arg_intensity.clone(),
                scoped_id.with_child_index(4),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(5),
                    None,
                    "DirectionalLight[color]",
                ),
                "color",
                arg_color.clone(),
                scoped_id.with_child_index(5),
                actor_context.scope,
            ),
        ],
    )))
}

/// Light/ambient(intensity, color) -> AmbientLight[...]
/// Pure data constructor for ambient light configuration.
pub fn function_light_ambient(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [arg_intensity, arg_color] = arguments.as_slice() else {
        panic!("Light/ambient expects 2 arguments")
    };
    let scoped_id = function_call_persistence_id;

    stream::once(future::ready(TaggedObject::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Light/ambient(..) -> AmbientLight[..]",
        ),
        construct_context.clone(),
        ValueIdempotencyKey::new(),
        "AmbientLight",
        [
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(1),
                    None,
                    "AmbientLight[intensity]",
                ),
                "intensity",
                arg_intensity.clone(),
                scoped_id.with_child_index(1),
                actor_context.scope.clone(),
            ),
            Variable::new_arc(
                ConstructInfo::new(
                    function_call_id.with_child_id(2),
                    None,
                    "AmbientLight[color]",
                ),
                "color",
                arg_color.clone(),
                scoped_id.with_child_index(2),
                actor_context.scope,
            ),
        ],
    )))
}

// --- Theme functions ---

/// Theme/background_color() -> Text
/// Returns the current theme background color (stub)
pub fn function_theme_background_color(
    _arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    stream::once(future::ready(Text::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Theme/background_color",
        ),
        construct_context,
        ValueIdempotencyKey::new(),
        "#ffffff".to_string(),
    )))
}

/// Theme/text_color() -> Text
/// Returns the current theme text color (stub)
pub fn function_theme_text_color(
    _arguments: Arc<Vec<ActorHandle>>,
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
    _arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    stream::once(future::ready(Text::new_value(
        ConstructInfo::new(
            function_call_id.with_child_id(0),
            None,
            "Theme/accent_color",
        ),
        construct_context,
        ValueIdempotencyKey::new(),
        "#0066cc".to_string(),
    )))
}

// --- File functions ---

/// File/read_text(path) -> Text
/// Reads text content from a file at the given path
pub fn function_file_read_text(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let path_actor = arguments[0].clone();
    path_actor.stream().then(move |value| {
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let path_actor = arguments[0].clone();
    let content_actor = arguments[1].clone();

    let construct_context_clone = construct_context.clone();
    path_actor.stream().flat_map(move |path_value| {
        let path = match &path_value {
            Value::Text(text, _) => text.text().to_string(),
            _ => String::new(),
        };
        let function_call_id = function_call_id.clone();
        let construct_context = construct_context_clone.clone();
        content_actor.clone().stream().map(move |content_value| {
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
    arguments: Arc<Vec<ActorHandle>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let stream_actor = arguments[0].clone();
    let count_actor = arguments[1].clone();

    // State type for unfold
    type FusedSub = stream::Fuse<LocalBoxStream<'static, Value>>;
    type InitialState = (
        Option<FusedSub>,
        Option<FusedSub>,
        ActorHandle,
        ActorHandle,
        usize,
        usize,
        bool,
        Vec<Value>,
    );

    // Defer subscription to inside async unfold
    let initial_state: InitialState = (
        None, // stream_sub - will be initialized in unfold
        None, // count_sub - will be initialized in unfold
        stream_actor,
        count_actor,
        0,          // current_skip_count
        0,          // skipped
        false,      // count_received
        Vec::new(), // buffered_values
    );

    stream::unfold(initial_state, |state| async move {
        let (
            stream_sub_opt,
            count_sub_opt,
            stream_actor,
            count_actor,
            mut skip_count,
            mut skipped,
            mut count_received,
            mut buffer,
        ) = state;

        // Subscribe on first iteration
        let mut stream_sub = match stream_sub_opt {
            Some(s) => s,
            None => stream_actor.clone().stream().boxed_local().fuse(),
        };
        let mut count_sub = match count_sub_opt {
            Some(s) => s,
            None => count_actor_current_and_future_values(count_actor.clone()).fuse(),
        };

        loop {
            // If we have buffered values and count is received, process buffer first
            if count_received && !buffer.is_empty() {
                let buffered = buffer.remove(0);
                if skipped < skip_count {
                    skipped += 1;
                    // Continue loop to process next buffered value
                } else {
                    return Some((
                        buffered,
                        (
                            Some(stream_sub),
                            Some(count_sub),
                            stream_actor,
                            count_actor,
                            skip_count,
                            skipped,
                            count_received,
                            buffer,
                        ),
                    ));
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
    arguments: Arc<Vec<ActorHandle>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let stream_actor = arguments[0].clone();
    let count_actor = arguments[1].clone();

    // State type for unfold
    type FusedSub = stream::Fuse<LocalBoxStream<'static, Value>>;
    type TakeState = (
        Option<FusedSub>,
        Option<FusedSub>,
        ActorHandle,
        ActorHandle,
        usize,
        usize,
        bool,
        Vec<Value>,
    );

    // Defer subscription to inside async unfold
    let initial_state: TakeState = (
        None, // stream_sub - will be initialized in unfold
        None, // count_sub - will be initialized in unfold
        stream_actor,
        count_actor,
        0,          // current_take_count
        0,          // taken
        false,      // count_received
        Vec::new(), // buffered_values
    );

    stream::unfold(initial_state, |state| async move {
        let (
            stream_sub_opt,
            count_sub_opt,
            stream_actor,
            count_actor,
            mut take_count,
            mut taken,
            mut count_received,
            mut buffer,
        ) = state;

        // Subscribe on first iteration
        let mut stream_sub = match stream_sub_opt {
            Some(s) => s,
            None => stream_actor.clone().stream().boxed_local().fuse(),
        };
        let mut count_sub = match count_sub_opt {
            Some(s) => s,
            None => count_actor_current_and_future_values(count_actor.clone()).fuse(),
        };

        loop {
            // If we have buffered values and count is received, process buffer first
            if count_received && !buffer.is_empty() {
                let buffered = buffer.remove(0);
                if taken < take_count {
                    taken += 1;
                    return Some((
                        buffered,
                        (
                            Some(stream_sub),
                            Some(count_sub),
                            stream_actor,
                            count_actor,
                            take_count,
                            taken,
                            count_received,
                            buffer,
                        ),
                    ));
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
    arguments: Arc<Vec<ActorHandle>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let stream_actor = arguments[0].clone();

    stream_actor
        .stream()
        .scan(None::<ValueIdempotencyKey>, move |last_key, value| {
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
    arguments: Arc<Vec<ActorHandle>>,
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
    count_actor_current_and_future_values(count_actor)
        .flat_map(move |v| stream::iter(make_pulses(&v)))
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
    arguments: Arc<Vec<ActorHandle>>,
    _function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let stream_actor = arguments[0].clone();
    let duration_actor = arguments[1].clone();

    // State type for unfold
    type InputSub = stream::Fuse<LocalBoxStream<'static, Value>>;
    type DurationSub = stream::Fuse<LocalBoxStream<'static, f64>>;
    type DebounceState = (
        Option<InputSub>,
        Option<DurationSub>,
        ActorHandle,
        ActorHandle,
        Option<Value>,
        f64,
    );

    let initial_state: DebounceState = (
        None, // input_stream - deferred
        None, // duration_stream - deferred
        stream_actor,
        duration_actor,
        None, // pending_value
        0.0,  // current_duration_ms
    );

    stream::unfold(initial_state, |state| async move {
        let (input_opt, duration_opt, stream_actor, duration_actor, mut pending, mut duration_ms) =
            state;

        // Subscribe on first iteration
        let mut input_stream = match input_opt {
            Some(s) => s,
            None => stream_actor.clone().stream().boxed_local().fuse(),
        };
        let mut duration_stream = match duration_opt {
            Some(s) => s,
            None => duration_actor_current_and_future_ms(duration_actor.clone()).fuse(),
        };

        loop {
            if pending.is_some() && duration_ms > 0.0 {
                // Have pending value and valid duration - race timer vs new input
                let mut timer = Box::pin(
                    Timer::sleep(duration_ms.round().max(0.0).min(u32::MAX as f64) as u32).fuse(),
                );

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
                        if let Some(next_duration_ms) = new_duration {
                            duration_ms = next_duration_ms;
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
                        if let Some(next_duration_ms) = new_duration {
                            duration_ms = next_duration_ms;
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
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let path_actor = arguments[0].clone();
    path_actor.stream().then(move |value| {
        let construct_context = construct_context.clone();
        let function_call_id = function_call_id.clone();
        let actor_context = actor_context.clone();
        async move {
            let path = match &value {
                Value::Text(text, _) => text.text().to_string(),
                _ => String::new(),
            };
            let entries = construct_context.virtual_fs.list_directory(&path).await;
            let entry_actors: Vec<ActorHandle> = entries
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

// --- Text functions (Cells spreadsheet) ---

/// Text/length(text) -> Number
/// Returns the number of characters in the text
pub fn function_text_length(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text] = arguments.as_slice() else {
        panic!("Text/length expects 1 argument")
    };
    argument_text.clone().stream().map(move |value| {
        let text = match &value {
            Value::Text(t, _) => t.text(),
            _ => panic!("Text/length expects a Text value"),
        };
        Number::new_value(
            ConstructInfo::new(
                function_call_id.with_child_id(0),
                None,
                "Text/length result",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            text.chars().count() as f64,
        )
    })
}

/// Text/char_at(text, index) -> Text
/// Returns the character at the given index as a single-character text
pub fn function_text_char_at(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text, argument_index] = arguments.as_slice() else {
        panic!("Text/char_at expects 2 arguments")
    };
    enum Input {
        Text(String),
        Index(f64),
    }
    let text_stream = text_actor_current_and_future_strings(argument_text.clone()).map(Input::Text);
    let index_stream =
        number_actor_current_and_future_numbers(argument_index.clone()).map(Input::Index);
    stream::select(text_stream, index_stream)
        .scan(
            (None::<String>, None::<f64>),
            move |(last_text, last_index), input| {
                match input {
                    Input::Text(t) => *last_text = Some(t),
                    Input::Index(i) => *last_index = Some(i),
                }
                if let (Some(text), Some(index)) = (last_text.as_ref(), *last_index) {
                    let idx = index as usize;
                    let ch = text
                        .chars()
                        .nth(idx)
                        .map(|c| c.to_string())
                        .unwrap_or_default();
                    future::ready(Some(Some(Text::new_value(
                        ConstructInfo::new(
                            function_call_id.with_child_id(0),
                            None,
                            "Text/char_at result",
                        ),
                        construct_context.clone(),
                        ValueIdempotencyKey::new(),
                        ch,
                    ))))
                } else {
                    future::ready(Some(None))
                }
            },
        )
        .filter_map(future::ready)
}

/// Text/find(text, search) -> Number
/// Returns the character index of the first occurrence of search in text, or -1 if not found
pub fn function_text_find(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text, argument_search] = arguments.as_slice() else {
        panic!("Text/find expects 2 arguments")
    };
    enum Input {
        Text(String),
        Search(String),
    }
    let text_stream = text_actor_current_and_future_strings(argument_text.clone()).map(Input::Text);
    let search_stream =
        text_actor_current_and_future_strings(argument_search.clone()).map(Input::Search);
    stream::select(text_stream, search_stream)
        .scan(
            (None::<String>, None::<String>),
            move |(last_text, last_search), input| {
                match input {
                    Input::Text(t) => *last_text = Some(t),
                    Input::Search(s) => *last_search = Some(s),
                }
                if let (Some(text), Some(search)) = (last_text.as_ref(), last_search.as_ref()) {
                    // Find byte offset, then convert to char index
                    let result = match text.find(search.as_str()) {
                        Some(byte_offset) => text[..byte_offset].chars().count() as f64,
                        None => -1.0,
                    };
                    future::ready(Some(Some(Number::new_value(
                        ConstructInfo::new(
                            function_call_id.with_child_id(0),
                            None,
                            "Text/find result",
                        ),
                        construct_context.clone(),
                        ValueIdempotencyKey::new(),
                        result,
                    ))))
                } else {
                    future::ready(Some(None))
                }
            },
        )
        .filter_map(future::ready)
}

/// Text/find_closing(text, open, close, start) -> Number
/// Scans text from position `start` forward, counting nesting depth.
/// Each `open` char increments depth, each `close` char decrements depth.
/// Returns position of matching closing delimiter when depth reaches 0, or -1 if not found.
pub fn function_text_find_closing(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text, argument_open, argument_close, argument_start] = arguments.as_slice()
    else {
        panic!("Text/find_closing expects 4 arguments")
    };
    enum Input {
        Text(String),
        Open(String),
        Close(String),
        Start(f64),
    }
    let text_stream = argument_text.clone().stream().map(|v| match &v {
        Value::Text(t, _) => Input::Text(t.text().to_string()),
        _ => panic!("Text/find_closing expects Text for first argument"),
    });
    let open_stream = argument_open.clone().stream().map(|v| match &v {
        Value::Text(t, _) => Input::Open(t.text().to_string()),
        _ => panic!("Text/find_closing expects Text for open argument"),
    });
    let close_stream = argument_close.clone().stream().map(|v| match &v {
        Value::Text(t, _) => Input::Close(t.text().to_string()),
        _ => panic!("Text/find_closing expects Text for close argument"),
    });
    let start_stream = argument_start.clone().stream().map(|v| match &v {
        Value::Number(n, _) => Input::Start(n.number()),
        _ => panic!("Text/find_closing expects Number for start argument"),
    });
    stream::select(
        stream::select(text_stream, open_stream),
        stream::select(close_stream, start_stream),
    )
    .scan(
        (None::<String>, None::<String>, None::<String>, None::<f64>),
        move |(last_text, last_open, last_close, last_start), input| {
            match input {
                Input::Text(t) => *last_text = Some(t),
                Input::Open(o) => *last_open = Some(o),
                Input::Close(c) => *last_close = Some(c),
                Input::Start(s) => *last_start = Some(s),
            }
            if let (Some(text), Some(open), Some(close), Some(start)) = (
                last_text.as_ref(),
                last_open.as_ref(),
                last_close.as_ref(),
                *last_start,
            ) {
                let start_idx = start as usize;
                let open_char = open.chars().next().unwrap_or('(');
                let close_char = close.chars().next().unwrap_or(')');
                let mut depth: i32 = 0;
                let mut result = -1.0f64;
                for (i, ch) in text.chars().enumerate() {
                    if i < start_idx {
                        continue;
                    }
                    if ch == open_char {
                        depth += 1;
                    } else if ch == close_char {
                        depth -= 1;
                        if depth == 0 {
                            result = i as f64;
                            break;
                        }
                    }
                }
                future::ready(Some(Some(Number::new_value(
                    ConstructInfo::new(
                        function_call_id.with_child_id(0),
                        None,
                        "Text/find_closing result",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    result,
                ))))
            } else {
                future::ready(Some(None))
            }
        },
    )
    .filter_map(future::ready)
}

/// Text/substring(text, start, length) -> Text
/// Returns a substring starting at char index `start` with `length` characters
pub fn function_text_substring(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text, argument_start, argument_length] = arguments.as_slice() else {
        panic!("Text/substring expects 3 arguments")
    };
    enum Input {
        Text(String),
        Start(f64),
        Length(f64),
    }
    let text_stream = argument_text.clone().stream().map(|v| match &v {
        Value::Text(t, _) => Input::Text(t.text().to_string()),
        _ => panic!("Text/substring expects Text for first argument"),
    });
    let start_stream = argument_start.clone().stream().map(|v| match &v {
        Value::Number(n, _) => Input::Start(n.number()),
        _ => panic!("Text/substring expects Number for start argument"),
    });
    let length_stream = argument_length.clone().stream().map(|v| match &v {
        Value::Number(n, _) => Input::Length(n.number()),
        _ => panic!("Text/substring expects Number for length argument"),
    });
    stream::select(text_stream, stream::select(start_stream, length_stream))
        .scan(
            (None::<String>, None::<f64>, None::<f64>),
            move |(last_text, last_start, last_length), input| {
                match input {
                    Input::Text(t) => *last_text = Some(t),
                    Input::Start(s) => *last_start = Some(s),
                    Input::Length(l) => *last_length = Some(l),
                }
                if let (Some(text), Some(start), Some(length)) =
                    (last_text.as_ref(), *last_start, *last_length)
                {
                    let start_idx = (start as isize).max(0) as usize;
                    let len = (length as isize).max(0) as usize;
                    let result: String = text.chars().skip(start_idx).take(len).collect();
                    future::ready(Some(Some(Text::new_value(
                        ConstructInfo::new(
                            function_call_id.with_child_id(0),
                            None,
                            "Text/substring result",
                        ),
                        construct_context.clone(),
                        ValueIdempotencyKey::new(),
                        result,
                    ))))
                } else {
                    future::ready(Some(None))
                }
            },
        )
        .filter_map(future::ready)
}

/// Text/to_uppercase(text) -> Text
pub fn function_text_to_uppercase(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text] = arguments.as_slice() else {
        panic!("Text/to_uppercase expects 1 argument")
    };
    argument_text.clone().stream().map(move |value| {
        let text = match &value {
            Value::Text(t, _) => t.text(),
            _ => panic!("Text/to_uppercase expects a Text value"),
        };
        Text::new_value(
            ConstructInfo::new(
                function_call_id.with_child_id(0),
                None,
                "Text/to_uppercase result",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            text.to_uppercase(),
        )
    })
}

/// Text/char_code(text) -> Number
/// Returns the Unicode code point of the first character
pub fn function_text_char_code(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_text] = arguments.as_slice() else {
        panic!("Text/char_code expects 1 argument")
    };
    argument_text.clone().stream().map(move |value| {
        let text = match &value {
            Value::Text(t, _) => t.text(),
            _ => panic!("Text/char_code expects a Text value"),
        };
        let code = text.chars().next().map(|c| c as u32 as f64).unwrap_or(0.0);
        Number::new_value(
            ConstructInfo::new(
                function_call_id.with_child_id(0),
                None,
                "Text/char_code result",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            code,
        )
    })
}

/// Text/from_char_code(number) -> Text
/// Creates a single-character text from a Unicode code point
pub fn function_text_from_char_code(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_code] = arguments.as_slice() else {
        panic!("Text/from_char_code expects 1 argument")
    };
    argument_code.clone().stream().map(move |value| {
        let code = match &value {
            Value::Number(n, _) => n.number(),
            _ => panic!("Text/from_char_code expects a Number value"),
        };
        let ch = char::from_u32(code as u32).unwrap_or('\0');
        Text::new_value(
            ConstructInfo::new(
                function_call_id.with_child_id(0),
                None,
                "Text/from_char_code result",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            ch.to_string(),
        )
    })
}

// --- List functions (Cells spreadsheet) ---

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectedActorKey {
    Missing,
    Actor(ActorId),
}

fn dedupe_selected_actor_stream<S>(source: S) -> LocalBoxStream<'static, Option<ActorHandle>>
where
    S: Stream<Item = Option<ActorHandle>> + 'static,
{
    source
        .scan(None::<SelectedActorKey>, |last_key, selection| {
            let current_key = selection
                .as_ref()
                .map_or(SelectedActorKey::Missing, |actor| {
                    SelectedActorKey::Actor(actor.actor_id())
                });
            if *last_key == Some(current_key) {
                future::ready(Some(None))
            } else {
                *last_key = Some(current_key);
                future::ready(Some(Some(selection)))
            }
        })
        .filter_map(future::ready)
        .boxed_local()
}

/// List/get(list, index) -> Value
/// Returns the item at the given 1-based index in the list.
/// Index 1 = first element, index 2 = second, etc.
/// Re-emits whenever the item at that index changes.
pub fn function_list_get(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    _construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_list, argument_index] = arguments.as_slice() else {
        panic!("List/get expects 2 arguments, got {}", arguments.len())
    };
    let list_actor = argument_list.clone();
    let index_actor = argument_index.clone();

    let construct_context_for_oob = _construct_context.clone();
    switch_map(
        selected_list_item_stream(list_actor, index_actor),
        move |item_opt| {
            let function_call_id = function_call_id.clone();
            let construct_context = construct_context_for_oob.clone();
            match item_opt {
                Some(actor) => actor.current_or_future_stream(),
                None => stream::once(future::ready(Tag::new_value(
                    ConstructInfo::new(
                        function_call_id.with_child_id(0),
                        None,
                        "List/get OutOfBounds",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    "OutOfBounds".to_string(),
                )))
                .boxed_local(),
            }
        },
    )
}

/// List/range(from, to) -> List {from, from+1, ..., to}
/// Creates a static list of numbers from `from` to `to` (inclusive).
pub fn function_list_range(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_from, argument_to] = arguments.as_slice() else {
        panic!("List/range expects 2 arguments, got {}", arguments.len())
    };
    let argument_from = argument_from.clone();
    let argument_to = argument_to.clone();

    enum Input {
        From(f64),
        To(f64),
    }
    let from_stream = argument_from.stream().filter_map(|value| {
        future::ready(match value {
            Value::Number(n, _) => Some(Input::From(n.number())),
            _ => None,
        })
    });
    let to_stream = argument_to.stream().filter_map(|value| {
        future::ready(match value {
            Value::Number(n, _) => Some(Input::To(n.number())),
            _ => None,
        })
    });
    let combined = stream::select(from_stream, to_stream)
        .scan(
            (None::<f64>, None::<f64>),
            move |(last_from, last_to), input| {
                match input {
                    Input::From(f) => *last_from = Some(f),
                    Input::To(t) => *last_to = Some(t),
                }
                future::ready(Some((*last_from, *last_to)))
            },
        )
        .filter_map(|(from_opt, to_opt)| {
            future::ready(match (from_opt, to_opt) {
                (Some(from), Some(to)) => Some((from, to)),
                _ => None,
            })
        });

    switch_map(combined, move |(from, to)| {
        let from_i = from as i64;
        let to_i = to as i64;
        let items: Vec<ActorHandle> = (from_i..=to_i)
            .enumerate()
            .map(|(i, n)| {
                Number::new_arc_value_actor(
                    ConstructInfo::new(
                        function_call_id.with_child_id(i as u32),
                        None,
                        "List/range item",
                    ),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    actor_context.clone(),
                    n as f64,
                )
            })
            .collect();
        stream::once(future::ready(List::new_value(
            ConstructInfo::new(
                function_call_id.with_child_id("list"),
                None,
                "List/range result",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context.clone(),
            items,
        )))
    })
}

/// List/sum(list) -> Number
/// Returns the sum of all Number items in the list. Empty list → 0.
/// Re-emits whenever any item value changes.
fn list_number_aggregate_stream<F>(
    items: Vec<ActorHandle>,
    function_call_id: ConstructId,
    construct_context: ConstructContext,
    empty_description: &'static str,
    result_description: &'static str,
    initial_value: f64,
    aggregate: F,
) -> LocalBoxStream<'static, Value>
where
    F: Fn(&[f64]) -> f64 + Copy + 'static,
{
    if items.is_empty() {
        return stream::once(future::ready(Number::new_value(
            ConstructInfo::new(function_call_id.with_child_id(0), None, empty_description),
            construct_context,
            ValueIdempotencyKey::new(),
            initial_value,
        )))
        .boxed_local();
    }

    let item_count = items.len();
    let streams: Vec<_> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            Box::pin(
                item.clone()
                    .current_or_future_stream()
                    .map(move |value| (i, value)),
            ) as Pin<Box<dyn Stream<Item = (usize, Value)>>>
        })
        .collect();

    stream::select_all(streams)
        .scan(
            (
                vec![initial_value; item_count],
                vec![false; item_count],
                item_count,
            ),
            move |(values, initialized, pending_count), (i, value)| {
                if let Value::Number(number, _) = &value {
                    values[i] = number.number();
                }
                if !initialized[i] {
                    initialized[i] = true;
                    *pending_count = pending_count.saturating_sub(1);
                }
                if *pending_count > 0 {
                    return future::ready(Some(None));
                }
                future::ready(Some(Some(Number::new_value(
                    ConstructInfo::new(function_call_id.with_child_id(0), None, result_description),
                    construct_context.clone(),
                    ValueIdempotencyKey::new(),
                    aggregate(values),
                ))))
            },
        )
        .filter_map(future::ready)
        .boxed_local()
}

pub fn function_list_sum(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let list_actor = arguments[0].clone();
    current_list_items_stream_from_actor(list_actor).flat_map(move |items| {
        list_number_aggregate_stream(
            items,
            function_call_id.clone(),
            construct_context.clone(),
            "List/sum empty",
            "List/sum result",
            0.0,
            |values| values.iter().sum(),
        )
    })
}

/// List/product(list) -> Number
/// Returns the product of all Number items in the list. Empty list → 1.
/// Re-emits whenever any item value changes.
pub fn function_list_product(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let list_actor = arguments[0].clone();
    current_list_items_stream_from_actor(list_actor).flat_map(move |items| {
        list_number_aggregate_stream(
            items,
            function_call_id.clone(),
            construct_context.clone(),
            "List/product empty",
            "List/product result",
            1.0,
            |values| values.iter().product(),
        )
    })
}

// --- Math functions (Cells spreadsheet) ---

/// Math/modulo(a, divisor) -> Number
/// Returns a % divisor (remainder)
pub fn function_math_modulo(
    arguments: Arc<Vec<ActorHandle>>,
    function_call_id: ConstructId,
    _function_call_persistence_id: PersistenceId,
    construct_context: ConstructContext,
    _actor_context: ActorContext,
) -> impl Stream<Item = Value> {
    let [argument_a, argument_divisor] = arguments.as_slice() else {
        panic!("Math/modulo expects 2 arguments")
    };
    enum Input {
        A(f64),
        Divisor(f64),
    }
    let a_stream = argument_a.clone().stream().map(|v| match &v {
        Value::Number(n, _) => Input::A(n.number()),
        _ => panic!("Math/modulo expects Number arguments"),
    });
    let divisor_stream = argument_divisor.clone().stream().map(|v| match &v {
        Value::Number(n, _) => Input::Divisor(n.number()),
        _ => panic!("Math/modulo expects Number arguments"),
    });
    stream::select(a_stream, divisor_stream)
        .scan(
            (None::<f64>, None::<f64>),
            move |(last_a, last_divisor), input| {
                match input {
                    Input::A(val) => *last_a = Some(val),
                    Input::Divisor(val) => *last_divisor = Some(val),
                }
                if let (Some(a), Some(divisor)) = (*last_a, *last_divisor) {
                    future::ready(Some(Some(Number::new_value(
                        ConstructInfo::new(
                            function_call_id.with_child_id(0),
                            None,
                            "Math/modulo result",
                        ),
                        construct_context.clone(),
                        ValueIdempotencyKey::new(),
                        a % divisor,
                    ))))
                } else {
                    future::ready(Some(None))
                }
            },
        )
        .filter_map(future::ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        struct ThreadWake(std::thread::Thread);

        impl Wake for ThreadWake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);

        loop {
            match future.as_mut().poll(&mut cx) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::park_timeout(std::time::Duration::from_millis(10)),
            }
        }
    }

    fn poll_once<F: std::future::Future>(future: F) -> Poll<F::Output> {
        struct ThreadWake(std::thread::Thread);

        impl Wake for ThreadWake {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }

        let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        future.as_mut().poll(&mut cx)
    }

    fn test_construct_context() -> ConstructContext {
        ConstructContext {
            construct_storage: Arc::new(ConstructStorage::new("")),
            virtual_fs: VirtualFilesystem::new(),
            bridge_scope_id: None,
            scene_ctx: None,
        }
    }

    fn test_actor_context() -> ActorContext {
        ActorContext {
            registry_scope_id: Some(create_registry_scope(None)),
            scope: boon::parser::Scope::Root,
            parameters: Arc::new(HashMap::new()),
            ..Default::default()
        }
    }

    fn number_value(value: f64, construct_context: ConstructContext) -> Value {
        Number::new_value(
            ConstructInfo::new(
                format!("test.number.{value}"),
                None,
                format!("test number {value}"),
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            value,
        )
    }

    #[test]
    fn event_actor_from_constant_element_reuses_nested_event_actor_directly() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let event_actor = create_constant_actor(
            PersistenceId::new(),
            Text::new_value(
                ConstructInfo::new("test.event.value", None, "test event value"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                "event",
            ),
            scope_id,
        );

        let element_actor = create_constant_actor(
            PersistenceId::new(),
            Object::new_value(
                ConstructInfo::new("test.element.value", None, "test element value"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                [Variable::new_arc(
                    ConstructInfo::new("test.element.event", None, "test element event"),
                    "event",
                    event_actor.clone(),
                    PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            ),
            scope_id,
        );

        let derived_event_actor = event_actor_from_constant_element(&element_actor, scope_id);
        assert_eq!(
            derived_event_actor.actor_id(),
            event_actor.actor_id(),
            "constant element wrappers should reuse the nested event actor directly"
        );

        let missing_event_actor = create_constant_actor(
            PersistenceId::new(),
            Object::new_value(
                ConstructInfo::new("test.element.no_event", None, "test element without event"),
                construct_context,
                ValueIdempotencyKey::new(),
                [],
            ),
            scope_id,
        );
        let fallback_event_actor =
            event_actor_from_constant_element(&missing_event_actor, scope_id);
        assert!(
            fallback_event_actor.current_value().is_err(),
            "missing event field should fall back to an empty direct actor"
        );
    }

    #[test]
    fn resolve_actor_value_for_log_uses_ready_current_value() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let actor = create_constant_actor(
            PersistenceId::new(),
            Text::new_value(
                ConstructInfo::new("test.log.ready_text", None, "test log ready text"),
                construct_context,
                ValueIdempotencyKey::new(),
                "ready",
            ),
            scope_id,
        );

        let rendered = block_on(resolve_actor_value_for_log(actor, 1));
        assert_eq!(rendered, "ready");
    }

    #[test]
    fn extract_log_options_from_with_reads_ready_fields_directly() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let label_actor = create_constant_actor(
            PersistenceId::new(),
            Text::new_value(
                ConstructInfo::new("test.log.with.label", None, "test log with label"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                "label",
            ),
            scope_id,
        );
        let timeout_actor = create_constant_actor(
            PersistenceId::new(),
            TaggedObject::new_value(
                ConstructInfo::new("test.log.with.timeout", None, "test log with timeout"),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                "Duration",
                [Variable::new_arc(
                    ConstructInfo::new(
                        "test.log.with.timeout.ms",
                        None,
                        "test log with timeout ms",
                    ),
                    "milliseconds",
                    create_constant_actor(
                        PersistenceId::new(),
                        number_value(250.0, construct_context.clone()),
                        scope_id,
                    ),
                    PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            ),
            scope_id,
        );
        let with_actor = create_constant_actor(
            PersistenceId::new(),
            Object::new_value(
                ConstructInfo::new("test.log.with", None, "test log with"),
                construct_context,
                ValueIdempotencyKey::new(),
                [
                    Variable::new_arc(
                        ConstructInfo::new("test.log.with.label.var", None, "test label var"),
                        "label",
                        label_actor,
                        PersistenceId::new(),
                        actor_context.scope.clone(),
                    ),
                    Variable::new_arc(
                        ConstructInfo::new("test.log.with.timeout.var", None, "test timeout var"),
                        "timeout",
                        timeout_actor,
                        PersistenceId::new(),
                        actor_context.scope.clone(),
                    ),
                ],
            ),
            scope_id,
        );

        let options = block_on(extract_log_options_from_with(with_actor));
        assert_eq!(options.label.as_deref(), Some("label"));
        assert_eq!(options.timeout_ms, 250);
    }

    #[test]
    fn extract_log_options_from_with_skips_stale_buffered_history_for_nested_fields() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let label_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        label_actor.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.log.with.stale_label.old",
                None,
                "test log stale label old",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            "old-label",
        ));
        label_actor.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.log.with.stale_label.current",
                None,
                "test log stale label current",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            "current-label",
        ));

        let milliseconds_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        milliseconds_actor.store_value_directly(number_value(25.0, construct_context.clone()));
        milliseconds_actor.store_value_directly(number_value(250.0, construct_context.clone()));

        let timeout_actor = create_constant_actor(
            PersistenceId::new(),
            TaggedObject::new_value(
                ConstructInfo::new(
                    "test.log.with.stale_timeout",
                    None,
                    "test log stale timeout",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                "Duration",
                [Variable::new_arc(
                    ConstructInfo::new(
                        "test.log.with.stale_timeout.ms",
                        None,
                        "test log stale timeout ms",
                    ),
                    "milliseconds",
                    milliseconds_actor,
                    PersistenceId::new(),
                    actor_context.scope.clone(),
                )],
            ),
            scope_id,
        );
        let with_actor = create_constant_actor(
            PersistenceId::new(),
            Object::new_value(
                ConstructInfo::new("test.log.with.stale", None, "test log stale with"),
                construct_context,
                ValueIdempotencyKey::new(),
                [
                    Variable::new_arc(
                        ConstructInfo::new(
                            "test.log.with.stale.label.var",
                            None,
                            "test stale label var",
                        ),
                        "label",
                        label_actor,
                        PersistenceId::new(),
                        actor_context.scope.clone(),
                    ),
                    Variable::new_arc(
                        ConstructInfo::new(
                            "test.log.with.stale.timeout.var",
                            None,
                            "test stale timeout var",
                        ),
                        "timeout",
                        timeout_actor,
                        PersistenceId::new(),
                        actor_context.scope.clone(),
                    ),
                ],
            ),
            scope_id,
        );

        let options = block_on(extract_log_options_from_with(with_actor));
        assert_eq!(options.label.as_deref(), Some("current-label"));
        assert_eq!(options.timeout_ms, 250);
    }

    #[test]
    fn extract_duration_ms_from_value_now_uses_current_nested_field_without_replaying_history() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let milliseconds_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        milliseconds_actor.store_value_directly(number_value(25.0, construct_context.clone()));
        milliseconds_actor.store_value_directly(number_value(250.0, construct_context.clone()));

        let duration_value = TaggedObject::new_value(
            ConstructInfo::new(
                "test.duration.extract.current",
                None,
                "test duration extract current",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            "Duration",
            [Variable::new_arc(
                ConstructInfo::new(
                    "test.duration.extract.current.ms",
                    None,
                    "test duration extract current ms",
                ),
                "milliseconds",
                milliseconds_actor,
                PersistenceId::new(),
                actor_context.scope.clone(),
            )],
        );

        assert_eq!(extract_duration_ms_from_value_now(&duration_value), 250.0);
    }

    #[test]
    fn duration_value_current_and_future_ms_uses_current_nested_field_without_replaying_history() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let milliseconds_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        milliseconds_actor.store_value_directly(number_value(25.0, construct_context.clone()));
        milliseconds_actor.store_value_directly(number_value(250.0, construct_context.clone()));

        let duration_value = TaggedObject::new_value(
            ConstructInfo::new(
                "test.duration.stream.current",
                None,
                "test duration stream current",
            ),
            construct_context,
            ValueIdempotencyKey::new(),
            "Duration",
            [Variable::new_arc(
                ConstructInfo::new(
                    "test.duration.stream.current.ms",
                    None,
                    "test duration stream current ms",
                ),
                "milliseconds",
                milliseconds_actor,
                PersistenceId::new(),
                actor_context.scope.clone(),
            )],
        );

        let mut stream = std::pin::pin!(duration_value_current_and_future_ms(duration_value));
        let Poll::Ready(Some(value)) = poll_once(stream.as_mut().next()) else {
            panic!("duration stream helper should emit current nested duration immediately");
        };
        assert_eq!(value, 250.0);
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "duration stream helper should skip stale buffered history and wait for future updates"
        );
    }

    #[test]
    fn duration_actor_current_and_future_ms_uses_current_duration_without_replaying_history() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let old_duration = TaggedObject::new_value(
            ConstructInfo::new("test.duration.actor.old", None, "test duration actor old"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            "Duration",
            [Variable::new_arc(
                ConstructInfo::new(
                    "test.duration.actor.old.ms",
                    None,
                    "test duration actor old ms",
                ),
                "milliseconds",
                create_constant_actor(
                    PersistenceId::new(),
                    number_value(25.0, construct_context.clone()),
                    scope_id,
                ),
                PersistenceId::new(),
                actor_context.scope.clone(),
            )],
        );
        let current_duration = TaggedObject::new_value(
            ConstructInfo::new(
                "test.duration.actor.current",
                None,
                "test duration actor current",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            "Duration",
            [Variable::new_arc(
                ConstructInfo::new(
                    "test.duration.actor.current.ms",
                    None,
                    "test duration actor current ms",
                ),
                "milliseconds",
                create_constant_actor(
                    PersistenceId::new(),
                    number_value(250.0, construct_context.clone()),
                    scope_id,
                ),
                PersistenceId::new(),
                actor_context.scope.clone(),
            )],
        );

        let duration_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        duration_actor.store_value_directly(old_duration);
        duration_actor.store_value_directly(current_duration);

        let mut stream = std::pin::pin!(duration_actor_current_and_future_ms(duration_actor));
        let Poll::Ready(Some(value)) = poll_once(stream.as_mut().next()) else {
            panic!("duration actor helper should emit current duration immediately");
        };
        assert_eq!(value, 250.0);
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "duration actor helper should skip stale buffered duration history and wait for future updates"
        );
    }

    #[test]
    fn count_actor_current_and_future_values_uses_current_count_without_replaying_history() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let count_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        count_actor.store_value_directly(number_value(1.0, construct_context.clone()));
        count_actor.store_value_directly(number_value(3.0, construct_context));

        let mut stream = std::pin::pin!(count_actor_current_and_future_values(count_actor));
        let Poll::Ready(Some(Value::Number(number, _))) = poll_once(stream.as_mut().next()) else {
            panic!("count helper should emit current count immediately");
        };
        assert_eq!(number.number(), 3.0);
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "count helper should skip stale buffered count history and wait for future updates"
        );
    }

    #[test]
    fn stream_pulses_uses_current_count_without_replaying_history() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let count_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        count_actor.store_value_directly(number_value(1.0, construct_context.clone()));
        count_actor.store_value_directly(number_value(3.0, construct_context.clone()));

        let stream = function_stream_pulses(
            Arc::new(vec![count_actor]),
            ConstructId::new("test.stream.pulses"),
            PersistenceId::new(),
            construct_context,
            actor_context,
        );
        let mut stream = std::pin::pin!(stream);

        let Poll::Ready(Some(first)) = poll_once(stream.as_mut().next()) else {
            panic!("pulses stream should emit the first pulse from the current count");
        };
        let Poll::Ready(Some(second)) = poll_once(stream.as_mut().next()) else {
            panic!("pulses stream should emit the second pulse from the current count");
        };
        let Poll::Ready(Some(third)) = poll_once(stream.as_mut().next()) else {
            panic!("pulses stream should emit the third pulse from the current count");
        };

        assert_eq!(number_from_value(first), 1.0);
        assert_eq!(number_from_value(second), 2.0);
        assert_eq!(number_from_value(third), 3.0);
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "pulses stream should skip stale buffered count history and wait for future updates"
        );
    }

    #[test]
    fn selected_list_item_stream_uses_current_index_without_replaying_history() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let first = create_constant_actor(
            PersistenceId::new(),
            Text::new_value(
                ConstructInfo::new(
                    "test.list.selection.first",
                    None,
                    "test list selection first",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                "first",
            ),
            scope_id,
        );
        let second = create_constant_actor(
            PersistenceId::new(),
            Text::new_value(
                ConstructInfo::new(
                    "test.list.selection.second",
                    None,
                    "test list selection second",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                "second",
            ),
            scope_id,
        );
        let third = create_constant_actor(
            PersistenceId::new(),
            Text::new_value(
                ConstructInfo::new(
                    "test.list.selection.third",
                    None,
                    "test list selection third",
                ),
                construct_context.clone(),
                ValueIdempotencyKey::new(),
                "third",
            ),
            scope_id,
        );

        let list_actor = create_constant_actor(
            PersistenceId::new(),
            List::new_value(
                ConstructInfo::new("test.list.selection.list", None, "test list selection list"),
                construct_context,
                ValueIdempotencyKey::new(),
                actor_context.clone(),
                vec![first, second, third],
            ),
            scope_id,
        );

        let index_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        index_actor.store_value_directly(number_value(1.0, test_construct_context()));
        index_actor.store_value_directly(number_value(3.0, test_construct_context()));

        let mut stream = std::pin::pin!(selected_list_item_stream(list_actor, index_actor));
        let Poll::Ready(Some(Some(actor))) = poll_once(stream.as_mut().next()) else {
            panic!("selected list item stream should emit the current indexed item");
        };
        let Value::Text(text, _) = actor
            .current_value()
            .expect("selected item should be ready")
        else {
            panic!("selected list item should stay text");
        };
        assert_eq!(text.text(), "third");
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "selected list item stream should skip stale buffered index history and wait for future updates"
        );
    }

    #[test]
    fn text_find_uses_current_arguments_without_replaying_history() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let text_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        text_actor.store_value_directly(Text::new_value(
            ConstructInfo::new("test.text.find.old_text", None, "test text find old text"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            "old text",
        ));
        text_actor.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.text.find.current_text",
                None,
                "test text find current text",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            "hello current",
        ));

        let search_actor = create_actor_forwarding(PersistenceId::new(), scope_id);
        search_actor.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.text.find.old_search",
                None,
                "test text find old search",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            "old",
        ));
        search_actor.store_value_directly(Text::new_value(
            ConstructInfo::new(
                "test.text.find.current_search",
                None,
                "test text find current search",
            ),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            "current",
        ));

        let stream = function_text_find(
            Arc::new(vec![text_actor, search_actor]),
            ConstructId::new("test.text.find"),
            PersistenceId::new(),
            construct_context,
            actor_context,
        );
        let mut stream = std::pin::pin!(stream);

        let Poll::Ready(Some(value)) = poll_once(stream.as_mut().next()) else {
            panic!("Text/find should emit from current arguments immediately");
        };
        assert_eq!(number_from_value(value), 6.0);
        assert!(
            matches!(poll_once(stream.as_mut().next()), Poll::Pending),
            "Text/find should skip stale buffered argument history and wait for future updates"
        );
    }

    fn number_from_value(value: Value) -> f64 {
        match value {
            Value::Number(number, _) => number.number(),
            other => panic!("Expected number, got {}", other.construct_info()),
        }
    }

    #[test]
    #[ignore = "requires wasm/js runtime; host lib tests still touch js-sys statics"]
    fn list_product_waits_for_all_items_before_emitting() {
        let construct_context = test_construct_context();
        let actor_context = test_actor_context();
        let scope_id = actor_context.scope_id();

        let first_item = create_constant_actor(
            PersistenceId::new(),
            number_value(2.0, construct_context.clone()),
            scope_id,
        );
        let second_item = create_actor_forwarding(PersistenceId::new(), scope_id);

        let list_value = List::new_value(
            ConstructInfo::new("test.list.product", None, "test list product"),
            construct_context.clone(),
            ValueIdempotencyKey::new(),
            actor_context.clone(),
            vec![first_item, second_item.clone()],
        );
        let list_actor = create_constant_actor(PersistenceId::new(), list_value, scope_id);

        let mut stream = std::pin::pin!(function_list_product(
            Arc::new(vec![list_actor]),
            "test.function.list_product".to_string().into(),
            PersistenceId::new(),
            construct_context.clone(),
            actor_context.clone(),
        ));

        assert!(matches!(poll_once(stream.as_mut().next()), Poll::Pending));

        second_item.store_value_directly(number_value(3.0, construct_context));

        let first_value = block_on(stream.as_mut().next()).expect("expected product value");
        assert_eq!(number_from_value(first_value), 6.0);
    }
}
