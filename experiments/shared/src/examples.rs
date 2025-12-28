//! Example programs for testing the engines.
//! These are constructed programmatically to avoid needing the full parser.

use crate::ast::{AstBuilder, Program};

/// Build a simple counter program:
/// ```boon
/// count: 0 |> HOLD state {
///     button.click |> THEN { state + 1 }
/// }
/// ```
pub fn counter_program() -> Program {
    let mut b = AstBuilder::new();

    // button.click |> THEN { state + 1 }
    let button_var = b.var("button");
    let button_click = b.path(button_var, "click");
    let state_var = b.var("state");
    let one = b.int(1);
    let increment = b.call("add", vec![state_var, one]);
    let then_expr = b.then(button_click, increment);

    // 0 |> HOLD state { ... }
    let zero = b.int(0);
    let hold = b.hold(zero, "state", then_expr);

    // button: LINK
    let button = b.link(None);

    b.build_program(vec![("button", button), ("count", hold)])
}

/// Build a simple interval counter:
/// ```boon
/// count: 0 |> HOLD state {
///     1000 |> Stream/interval() |> THEN { state + 1 }
/// }
/// ```
pub fn interval_program() -> Program {
    let mut b = AstBuilder::new();

    // 1000 |> Stream/interval() |> THEN { state + 1 }
    let ms = b.int(1000);
    let interval = b.pipe(ms, "Stream/interval", vec![]);
    let state_var = b.var("state");
    let one = b.int(1);
    let increment = b.call("add", vec![state_var, one]);
    let then_expr = b.then(interval, increment);

    // 0 |> HOLD state { ... }
    let zero = b.int(0);
    let hold = b.hold(zero, "state", then_expr);

    b.build_program(vec![("count", hold)])
}

/// Build a shopping list program:
/// ```boon
/// items: [] |> HOLD state {
///     input.submit |> THEN {
///         state |> List/append(input.value)
///     }
/// }
/// ```
pub fn shopping_list_program() -> Program {
    let mut b = AstBuilder::new();

    // input.submit |> THEN { state |> List/append(input.value) }
    let input_var = b.var("input");
    let input_submit = b.path(input_var, "submit");
    let input_var2 = b.var("input");
    let input_value = b.path(input_var2, "value");
    let state_var = b.var("state");
    let append = b.pipe(state_var, "List/append", vec![input_value]);
    let then_expr = b.then(input_submit, append);

    // [] |> HOLD state { ... }
    let empty_list = b.list(vec![]);
    let hold = b.hold(empty_list, "state", then_expr);

    // input: LINK
    let input = b.link(None);

    b.build_program(vec![("input", input), ("items", hold)])
}

/// Build a TodoMVC program that demonstrates the toggle-all bug:
/// ```boon
/// toggle_all: LINK
/// new_todo_input: LINK
///
/// todos: [] |> HOLD state {
///     new_todo_input.submit |> THEN {
///         state |> List/append({
///             text: new_todo_input.value,
///             completed: False |> HOLD completed_state {
///                 LATEST {
///                     todo_checkbox.click |> THEN { completed_state |> Bool/not() }
///                     toggle_all.click |> THEN { all_completed |> Bool/not() }
///                 }
///             },
///             todo_checkbox: LINK
///         })
///     }
/// }
///
/// all_completed: todos |> List/every(item => item.completed)
/// ```
pub fn todo_mvc_program() -> Program {
    let mut b = AstBuilder::new();

    // toggle_all: LINK
    let toggle_all = b.link(None);

    // new_todo_input: LINK
    let new_todo_input = b.link(None);

    // The todo item template with the critical external dependency
    // completed: False |> HOLD completed_state { LATEST { ... } }

    // todo_checkbox.click |> THEN { completed_state |> Bool/not() }
    let todo_checkbox_var = b.var("todo_checkbox");
    let checkbox_click = b.path(todo_checkbox_var, "click");
    let completed_state_var = b.var("completed_state");
    let toggle_self = b.pipe(completed_state_var, "Bool/not", vec![]);
    let checkbox_then = b.then(checkbox_click, toggle_self);

    // toggle_all.click |> THEN { all_completed |> Bool/not() }
    // THIS IS THE EXTERNAL DEPENDENCY that causes the bug
    let toggle_all_var = b.var("toggle_all");
    let toggle_all_click = b.path(toggle_all_var, "click");
    let all_completed_var = b.var("all_completed");
    let toggle_from_all = b.pipe(all_completed_var, "Bool/not", vec![]);
    let toggle_all_then = b.then(toggle_all_click, toggle_from_all);

    // LATEST { checkbox_then, toggle_all_then }
    let latest = b.latest(vec![checkbox_then, toggle_all_then]);

    // False |> HOLD completed_state { latest }
    let false_val = b.bool(false);
    let completed_hold = b.hold(false_val, "completed_state", latest);

    // todo_checkbox: LINK
    let todo_checkbox = b.link(None);

    // { text: new_todo_input.value, completed: ..., todo_checkbox: LINK }
    let new_todo_input_var = b.var("new_todo_input");
    let input_value = b.path(new_todo_input_var, "value");
    let todo_item = b.object(vec![
        ("text", input_value),
        ("completed", completed_hold),
        ("todo_checkbox", todo_checkbox),
    ]);

    // state |> List/append(todo_item)
    let state_var = b.var("state");
    let append = b.list_append(state_var, todo_item);

    // new_todo_input.submit |> THEN { append }
    let new_todo_input_var2 = b.var("new_todo_input");
    let input_submit = b.path(new_todo_input_var2, "submit");
    let add_todo_then = b.then(input_submit, append);

    // [] |> HOLD state { add_todo_then }
    let empty_list = b.list(vec![]);
    let todos_hold = b.hold(empty_list, "state", add_todo_then);

    // all_completed: todos |> List/every(item => item.completed)
    let item_var = b.var("item");
    let item_completed = b.path(item_var, "completed");
    let todos_var = b.var("todos");
    let all_completed = b.pipe(todos_var, "List/every", vec![item_completed]);

    b.build_program(vec![
        ("toggle_all", toggle_all),
        ("new_todo_input", new_todo_input),
        ("todos", todos_hold),
        ("all_completed", all_completed),
    ])
}

/// Build a list append test program:
/// ```boon
/// button: LINK
/// items: [] |> HOLD state {
///     button.click |> THEN {
///         state |> List/append(List/len(state))
///     }
/// }
/// ```
pub fn list_append_program() -> Program {
    let mut b = AstBuilder::new();

    // button: LINK
    let button = b.link(None);

    // List/len(state)
    let state_var = b.var("state");
    let len = b.call("List/len", vec![state_var]);

    // state |> List/append(len)
    let state_var2 = b.var("state");
    let append = b.list_append(state_var2, len);

    // button.click |> THEN { append }
    let button_var = b.var("button");
    let button_click = b.path(button_var, "click");
    let then_expr = b.then(button_click, append);

    // [] |> HOLD state { then_expr }
    let empty_list = b.list(vec![]);
    let hold = b.hold(empty_list, "state", then_expr);

    b.build_program(vec![("button", button), ("items", hold)])
}

/// Build a list clear test program:
/// ```boon
/// add: LINK
/// clear: LINK
/// items: [] |> HOLD state {
///     LATEST {
///         add.click |> THEN { state |> List/append(List/len(state)) }
///         clear.click |> THEN { state |> List/clear() }
///     }
/// }
/// ```
pub fn list_clear_program() -> Program {
    let mut b = AstBuilder::new();

    // add: LINK
    let add = b.link(None);

    // clear: LINK
    let clear = b.link(None);

    // List/len(state)
    let state_var = b.var("state");
    let len = b.call("List/len", vec![state_var]);

    // state |> List/append(len) - using pipe to builtin function
    let state_var2 = b.var("state");
    let append = b.pipe(state_var2, "List/append", vec![len]);

    // add.click |> THEN { append }
    let add_var = b.var("add");
    let add_click = b.path(add_var, "click");
    let add_then = b.then(add_click, append);

    // state |> List/clear() - using pipe to builtin function
    let state_var3 = b.var("state");
    let list_clear = b.pipe(state_var3, "List/clear", vec![]);

    // clear.click |> THEN { list_clear }
    let clear_var = b.var("clear");
    let clear_click = b.path(clear_var, "click");
    let clear_then = b.then(clear_click, list_clear);

    // LATEST { add_then, clear_then }
    let latest = b.latest(vec![add_then, clear_then]);

    // [] |> HOLD state { latest }
    let empty_list = b.list(vec![]);
    let hold = b.hold(empty_list, "state", latest);

    b.build_program(vec![("add", add), ("clear", clear), ("items", hold)])
}

/// Build a list remove test program:
/// ```boon
/// add: LINK
/// remove: LINK
/// items: [] |> HOLD state {
///     LATEST {
///         add.click |> THEN { state |> List/append({id: List/len(state)}) }
///         remove.click |> THEN { state |> List/remove(0) }
///     }
/// }
/// ```
pub fn list_remove_program() -> Program {
    let mut b = AstBuilder::new();

    // add: LINK
    let add = b.link(None);

    // remove: LINK
    let remove = b.link(None);

    // List/len(state)
    let state_var = b.var("state");
    let len = b.call("List/len", vec![state_var]);

    // { id: List/len(state) }
    let item = b.object(vec![("id", len)]);

    // state |> List/append(item) - using pipe to builtin function
    let state_var2 = b.var("state");
    let append = b.pipe(state_var2, "List/append", vec![item]);

    // add.click |> THEN { append }
    let add_var = b.var("add");
    let add_click = b.path(add_var, "click");
    let add_then = b.then(add_click, append);

    // state |> List/remove(0) - using pipe to builtin function
    let state_var3 = b.var("state");
    let zero = b.int(0);
    let list_remove = b.pipe(state_var3, "List/remove", vec![zero]);

    // remove.click |> THEN { list_remove }
    let remove_var = b.var("remove");
    let remove_click = b.path(remove_var, "click");
    let remove_then = b.then(remove_click, list_remove);

    // LATEST { add_then, remove_then }
    let latest = b.latest(vec![add_then, remove_then]);

    // [] |> HOLD state { latest }
    let empty_list = b.list(vec![]);
    let hold = b.hold(empty_list, "state", latest);

    b.build_program(vec![("add", add), ("remove", remove), ("items", hold)])
}

/// Build a list retain test program:
/// ```boon
/// add: LINK
/// clear_completed: LINK
/// items: [] |> HOLD state {
///     LATEST {
///         add.click |> THEN { state |> List/append({completed: False}) }
///         clear_completed.click |> THEN {
///             state |> List/retain(item => item.completed |> Bool/not())
///         }
///     }
/// }
/// ```
pub fn list_retain_program() -> Program {
    let mut b = AstBuilder::new();

    // add: LINK
    let add = b.link(None);

    // clear_completed: LINK
    let clear_completed = b.link(None);

    // { completed: False }
    let false_val = b.bool(false);
    let item = b.object(vec![("completed", false_val)]);

    // state |> List/append(item) - using pipe to builtin function
    let state_var = b.var("state");
    let append = b.pipe(state_var, "List/append", vec![item]);

    // add.click |> THEN { append }
    let add_var = b.var("add");
    let add_click = b.path(add_var, "click");
    let add_then = b.then(add_click, append);

    // item.completed |> Bool/not()
    let item_var = b.var("item");
    let item_completed = b.path(item_var, "completed");
    let not_completed = b.pipe(item_completed, "Bool/not", vec![]);

    // state |> List/retain(item => not_completed) - uses ExprKind::ListRetain for predicate
    let state_var2 = b.var("state");
    let list_retain = b.list_retain(state_var2, "item", not_completed);

    // clear_completed.click |> THEN { list_retain }
    let clear_var = b.var("clear_completed");
    let clear_click = b.path(clear_var, "click");
    let clear_then = b.then(clear_click, list_retain);

    // LATEST { add_then, clear_then }
    let latest = b.latest(vec![add_then, clear_then]);

    // [] |> HOLD state { latest }
    let empty_list = b.list(vec![]);
    let hold = b.hold(empty_list, "state", latest);

    b.build_program(vec![("add", add), ("clear_completed", clear_completed), ("items", hold)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter_program_structure() {
        let program = counter_program();
        assert_eq!(program.bindings.len(), 2);
        assert_eq!(program.bindings[0].0, "button");
        assert_eq!(program.bindings[1].0, "count");
    }

    #[test]
    fn test_todo_mvc_program_structure() {
        let program = todo_mvc_program();
        assert_eq!(program.bindings.len(), 4);
        assert_eq!(program.bindings[0].0, "toggle_all");
        assert_eq!(program.bindings[1].0, "new_todo_input");
        assert_eq!(program.bindings[2].0, "todos");
        assert_eq!(program.bindings[3].0, "all_completed");
    }

    #[test]
    fn test_list_clear_program_structure() {
        let program = list_clear_program();
        assert_eq!(program.bindings.len(), 3);
        assert_eq!(program.bindings[0].0, "add");
        assert_eq!(program.bindings[1].0, "clear");
        assert_eq!(program.bindings[2].0, "items");
    }

    #[test]
    fn test_list_remove_program_structure() {
        let program = list_remove_program();
        assert_eq!(program.bindings.len(), 3);
        assert_eq!(program.bindings[0].0, "add");
        assert_eq!(program.bindings[1].0, "remove");
        assert_eq!(program.bindings[2].0, "items");
    }

    #[test]
    fn test_list_retain_program_structure() {
        let program = list_retain_program();
        assert_eq!(program.bindings.len(), 3);
        assert_eq!(program.bindings[0].0, "add");
        assert_eq!(program.bindings[1].0, "clear_completed");
        assert_eq!(program.bindings[2].0, "items");
    }
}
