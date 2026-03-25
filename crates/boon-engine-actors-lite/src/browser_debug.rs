#[cfg(target_arch = "wasm32")]
pub fn clear_debug_marker() {
    use boon::zoon::{js_sys, wasm_bindgen::JsValue, web_sys};

    let Some(window) = web_sys::window() else {
        return;
    };

    let key = JsValue::from_str("__boonActorsLiteDebug");
    let debug = js_sys::Object::new();
    let history = js_sys::Array::new();
    let _ = js_sys::Reflect::set(&debug, &JsValue::from_str("history"), &history);
    let _ = js_sys::Reflect::set(&debug, &JsValue::from_str("last"), &JsValue::NULL);
    let _ = js_sys::Reflect::set(&window, &key, &debug);

    if let Some(document) = window.document() {
        if let Some(body) = document.body() {
            let _ = body.remove_attribute("data-boon-actorslite-debug");
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn clear_debug_marker() {}

#[cfg(target_arch = "wasm32")]
pub fn set_debug_marker(step: &str) {
    use boon::zoon::{js_sys, wasm_bindgen::JsCast, wasm_bindgen::JsValue, web_sys};

    let Some(window) = web_sys::window() else {
        return;
    };

    let key = JsValue::from_str("__boonActorsLiteDebug");
    let debug = js_sys::Reflect::get(&window, &key)
        .ok()
        .filter(|value| value.is_object())
        .unwrap_or_else(|| {
            let object = js_sys::Object::new();
            let history = js_sys::Array::new();
            let _ = js_sys::Reflect::set(&object, &JsValue::from_str("history"), &history);
            object.into()
        });

    let history = js_sys::Reflect::get(&debug, &JsValue::from_str("history"))
        .ok()
        .and_then(|value| value.dyn_into::<js_sys::Array>().ok())
        .unwrap_or_else(js_sys::Array::new);

    let entry = format!("{} @ {:.0}", step, js_sys::Date::now());
    history.push(&JsValue::from_str(&entry));
    while history.length() > 32 {
        let _ = history.shift();
    }

    let _ = js_sys::Reflect::set(&debug, &JsValue::from_str("history"), &history);
    let _ = js_sys::Reflect::set(&debug, &JsValue::from_str("last"), &JsValue::from_str(step));
    let _ = js_sys::Reflect::set(&window, &key, &debug);

    if let Some(document) = window.document() {
        if let Some(body) = document.body() {
            let _ = body.set_attribute("data-boon-actorslite-debug", step);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn set_debug_marker(_step: &str) {}
