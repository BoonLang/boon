//! Debug test for path evaluation

use path_a::Engine;
use shared::ast::AstBuilder;
use shared::test_harness::{text, TestEngine, Value};

#[test]
fn simple_path_value() {
    // Test: new_todo_input.value extraction
    // new_todo_input: LINK
    // result: new_todo_input.value
    let mut b = AstBuilder::new();
    let input_link = b.link(None);
    let input_var = b.var("new_todo_input");
    let value_path = b.path(input_var, "value");

    let program = b.build_program(vec![
        ("new_todo_input", input_link),
        ("result", value_path),
    ]);

    let mut engine = TestEngine::<Engine>::new(&program);

    // Before event, result should be Skip
    let result = engine.read("result");
    println!("Before event: result = {:?}", result);

    // After inject, check new_todo_input value
    engine.inject_event("new_todo_input.submit", text("Hello"));

    let input_val = engine.read("new_todo_input");
    println!("new_todo_input = {:?}", input_val);

    let result = engine.read("result");
    println!("After event: result = {:?}", result);

    // The result should be the value field
    assert_eq!(result, text("Hello"));
}

#[test]
fn object_with_path_in_then() {
    // Simplified todo pattern:
    // input: LINK
    // todos: [] |> HOLD state {
    //     input.submit |> THEN {
    //         state |> List/append({ text: input.value })
    //     }
    // }
    let mut b = AstBuilder::new();

    // input: LINK
    let input_link = b.link(None);

    // input.value
    let input_var = b.var("input");
    let input_value = b.path(input_var, "value");

    // { text: input.value }
    let item_obj = b.object(vec![("text", input_value)]);

    // state |> List/append(item_obj)
    let state_var = b.var("state");
    let append = b.list_append(state_var, item_obj);

    // input.submit
    let input_var2 = b.var("input");
    let input_submit = b.path(input_var2, "submit");

    // input.submit |> THEN { append }
    let then_expr = b.then(input_submit, append);

    // [] |> HOLD state { then }
    let empty_list = b.list(vec![]);
    let hold = b.hold(empty_list, "state", then_expr);

    let program = b.build_program(vec![
        ("input", input_link),
        ("todos", hold),
    ]);

    let mut engine = TestEngine::<Engine>::new(&program);

    // Initially empty
    let todos = engine.read("todos");
    println!("Initial todos: {:?}", todos);
    assert!(matches!(todos, Value::List(ref v) if v.is_empty()));

    // Add item
    engine.inject_event("input.submit", text("Hello"));

    let todos = engine.read("todos");
    println!("After add: {:?}", todos);

    match todos {
        Value::List(items) => {
            assert_eq!(items.len(), 1);
            if let Value::Object(obj) = &items[0] {
                println!("Item: {:?}", obj);
                assert_eq!(obj.get("text"), Some(&text("Hello")));
            } else {
                panic!("Expected object, got {:?}", items[0]);
            }
        }
        _ => panic!("Expected list, got {:?}", todos),
    }
}
