use crate::host_view_preview::HostViewPreviewApp;
use crate::interactive_preview::{InteractivePreview, render_interactive_preview};
use crate::lower::{PagesProgram, try_lower_pages};
use boon::platform::browser::kernel::KernelValue;
use boon::zoon::*;
use boon_renderer_zoon::FakeRenderState;
use boon_scene::{RenderRoot, UiEventBatch, UiEventKind};
use std::collections::BTreeMap;

const HOME_ROUTE: &str = "/";
const ABOUT_ROUTE: &str = "/about";
const CONTACT_ROUTE: &str = "/contact";

pub struct PagesPreview {
    program: PagesProgram,
    current_route: String,
    app: HostViewPreviewApp,
}

impl PagesPreview {
    pub fn new(source: &str) -> Result<Self, String> {
        let program = try_lower_pages(source)?;
        let current_route = current_browser_route();
        let app = HostViewPreviewApp::new(
            program.host_view.clone(),
            sink_values_for_route(&program, &current_route),
        );
        Ok(Self {
            program,
            current_route,
            app,
        })
    }

    #[must_use]
    pub fn app(&self) -> &HostViewPreviewApp {
        &self.app
    }

    #[must_use]
    pub fn preview_text(&mut self) -> String {
        self.app.preview_text()
    }

    fn set_route(&mut self, route: &str) -> bool {
        let normalized = normalize_route(route);
        if normalized == self.current_route {
            return false;
        }
        self.current_route = normalized.clone();
        push_route_to_browser(&normalized);
        for (sink, value) in sink_values_for_route(&self.program, &normalized) {
            self.app.set_sink_value(sink, value);
        }
        true
    }
}

impl InteractivePreview for PagesPreview {
    fn dispatch_ui_events(&mut self, batch: UiEventBatch) -> bool {
        let home_port = self
            .app
            .event_port_for_source(self.program.nav_press_ports[0]);
        let about_port = self
            .app
            .event_port_for_source(self.program.nav_press_ports[1]);
        let contact_port = self
            .app
            .event_port_for_source(self.program.nav_press_ports[2]);

        for event in batch.events {
            if event.kind != UiEventKind::Click {
                continue;
            }
            if Some(event.target) == home_port {
                return self.set_route(HOME_ROUTE);
            }
            if Some(event.target) == about_port {
                return self.set_route(ABOUT_ROUTE);
            }
            if Some(event.target) == contact_port {
                return self.set_route(CONTACT_ROUTE);
            }
        }

        false
    }

    fn dispatch_ui_facts(&mut self, _batch: boon_scene::UiFactBatch) -> bool {
        false
    }

    fn render_snapshot(&mut self) -> (RenderRoot, FakeRenderState) {
        let (root, state) = self.app.render_snapshot();
        (RenderRoot::UiTree(root), state)
    }
}

pub fn render_pages_preview(preview: PagesPreview) -> impl Element {
    render_interactive_preview(preview)
}

fn sink_values_for_route(
    program: &PagesProgram,
    route: &str,
) -> BTreeMap<crate::ir::SinkPortId, KernelValue> {
    let normalized = normalize_route(route);
    let (title, description) = match normalized.as_str() {
        HOME_ROUTE => (
            "Welcome Home",
            "This is the home page. Use the navigation above to explore.",
        ),
        ABOUT_ROUTE => (
            "About",
            "A multi-page Boon app demonstrating Router/route and Router/go_to.",
        ),
        CONTACT_ROUTE => (
            "Contact",
            "Get in touch! URL-driven state and navigation demo.",
        ),
        _ => (
            "404 - Not Found",
            "The page you're looking for doesn't exist.",
        ),
    };

    BTreeMap::from([
        (program.title_sink, KernelValue::from(title)),
        (program.description_sink, KernelValue::from(description)),
        (
            program.nav_active_sinks[0],
            KernelValue::Bool(normalized.as_str() == HOME_ROUTE),
        ),
        (
            program.nav_active_sinks[1],
            KernelValue::Bool(normalized.as_str() == ABOUT_ROUTE),
        ),
        (
            program.nav_active_sinks[2],
            KernelValue::Bool(normalized.as_str() == CONTACT_ROUTE),
        ),
    ])
}

fn normalize_route(route: &str) -> String {
    match route {
        "" | HOME_ROUTE => HOME_ROUTE.to_string(),
        ABOUT_ROUTE => ABOUT_ROUTE.to_string(),
        CONTACT_ROUTE => CONTACT_ROUTE.to_string(),
        other if other.starts_with('/') => other.to_string(),
        other => format!("/{other}"),
    }
}

fn current_browser_route() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            if let Ok(pathname) = window.location().pathname() {
                return normalize_route(&pathname);
            }
        }
    }

    HOME_ROUTE.to_string()
}

fn push_route_to_browser(_route: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            if let Ok(history) = window.history() {
                let search = window.location().search().unwrap_or_default();
                let target = format!("{}{}", normalize_route(_route), search);
                let _ =
                    history.push_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&target));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boon_scene::{UiEvent, UiEventKind};

    #[test]
    fn pages_preview_switches_route_content_on_click() {
        let source = include_str!("../../../playground/frontend/src/examples/pages/pages.bn");
        let mut preview = PagesPreview::new(source).expect("pages preview");
        assert!(preview.preview_text().contains("Welcome Home"));
        let _ = preview.render_snapshot();
        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.nav_press_ports[1])
                    .expect("about port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(preview.preview_text().contains("A multi-page Boon app"));

        preview.dispatch_ui_events(UiEventBatch {
            events: vec![UiEvent {
                target: preview
                    .app()
                    .event_port_for_source(preview.program.nav_press_ports[2])
                    .expect("contact port"),
                kind: UiEventKind::Click,
                payload: None,
            }],
        });
        assert!(
            preview
                .preview_text()
                .contains("URL-driven state and navigation demo.")
        );
    }

    #[test]
    fn normalize_route_maps_known_paths() {
        assert_eq!(normalize_route(""), "/");
        assert_eq!(normalize_route("/about"), "/about");
        assert_eq!(normalize_route("contact"), "/contact");
    }
}
