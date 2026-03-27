use crate::host_view::{HostViewNode, HostViewTree};
use crate::lower::{ButtonHoverProgram, ButtonHoverToClickProgram, SwitchHoldProgram};
use boon_scene::{EventPortId, NodeId, UiEvent, UiEventKind, UiFact, UiFactKind};

const ROOT_PADDING: &str = "20px";
const COLUMN_GAP: &str = "20px";
const ROW_GAP: &str = "10px";
const BUTTON_PADDING: &str = "20px 10px";
const BUTTON_RADIUS: &str = "5px";
const BUTTON_TEXT_COLOR: &str = "white";
const BUTTON_BASE_COLOR: &str = "oklch(0.25 0 0)";
const HOVER_ACTIVE_COLOR: &str = "oklch(0.35 0.1 250)";
const HOVER_ACTIVE_OUTLINE: &str = "2px solid oklch(0.6 0.2 250)";
const CLICK_ACTIVE_COLOR: &str = "oklch(0.35 0.1 25)";
const CLICK_ACTIVE_OUTLINE: &str = "2px solid oklch(0.6 0.25 25)";
const ITEM_A_COLOR: &str = "oklch(0.35 0.1 120)";
const ITEM_B_COLOR: &str = "oklch(0.35 0.1 240)";
const TOGGLE_COLOR: &str = "oklch(0.3 0 0)";

#[derive(Debug)]
pub struct ButtonHoverState {
    program: ButtonHoverProgram,
    ui: ButtonHoverUi,
    hovered: [bool; 3],
}

#[derive(Debug)]
struct ButtonHoverUi {
    root: NodeId,
    label: NodeId,
    row: NodeId,
    buttons: [NodeId; 3],
}

#[derive(Debug)]
pub struct ButtonHoverToClickState {
    program: ButtonHoverToClickProgram,
    ui: ButtonHoverToClickUi,
    clicked: [bool; 3],
}

#[derive(Debug)]
struct ButtonHoverToClickUi {
    root: NodeId,
    label: NodeId,
    row: NodeId,
    buttons: [ToggleButtonUi; 3],
    state_label: NodeId,
}

#[derive(Debug, Clone, Copy)]
struct ToggleButtonUi {
    node: NodeId,
    click_port: EventPortId,
}

#[derive(Debug)]
pub struct SwitchHoldState {
    program: SwitchHoldProgram,
    ui: SwitchHoldUi,
    show_item_a: bool,
    click_counts: [u32; 2],
}

#[derive(Debug)]
struct SwitchHoldUi {
    root: NodeId,
    active_label: NodeId,
    toggle_button: ToggleButtonUi,
    count_label: NodeId,
    buttons_row: NodeId,
    item_a_button: ToggleButtonUi,
    item_b_button: ToggleButtonUi,
    footer_label: NodeId,
}

impl ButtonHoverState {
    pub fn new(program: ButtonHoverProgram) -> Self {
        Self {
            program,
            ui: ButtonHoverUi {
                root: NodeId::new(),
                label: NodeId::new(),
                row: NodeId::new(),
                buttons: [NodeId::new(), NodeId::new(), NodeId::new()],
            },
            hovered: [false; 3],
        }
    }

    pub fn handle_fact(&mut self, fact: &UiFact) -> bool {
        let UiFactKind::Hovered(hovered) = fact.kind else {
            return false;
        };
        for (index, button) in self.ui.buttons.iter().enumerate() {
            if fact.id == *button && self.hovered[index] != hovered {
                self.hovered[index] = hovered;
                return true;
            }
        }
        false
    }

    pub fn view_tree(&self) -> HostViewTree {
        HostViewTree::from_root(
            HostViewNode::element(self.ui.root, "div")
                .with_style("display", "flex")
                .with_style("flex-direction", "column")
                .with_style("gap", COLUMN_GAP)
                .with_style("padding", ROOT_PADDING)
                .with_children(vec![
                    HostViewNode::element(self.ui.label, "div")
                        .with_text(self.program.prompt.clone()),
                    HostViewNode::element(self.ui.row, "div")
                        .with_style("display", "flex")
                        .with_style("flex-direction", "row")
                        .with_style("gap", ROW_GAP)
                        .with_children(
                            self.program
                                .button_labels
                                .iter()
                                .zip(self.ui.buttons)
                                .zip(self.hovered)
                                .map(|((label, node), hovered)| hover_button(node, label, hovered))
                                .collect(),
                        ),
                ]),
        )
    }
}

impl ButtonHoverToClickState {
    pub fn new(program: ButtonHoverToClickProgram) -> Self {
        Self {
            program,
            ui: ButtonHoverToClickUi {
                root: NodeId::new(),
                label: NodeId::new(),
                row: NodeId::new(),
                buttons: [
                    ToggleButtonUi {
                        node: NodeId::new(),
                        click_port: EventPortId::new(),
                    },
                    ToggleButtonUi {
                        node: NodeId::new(),
                        click_port: EventPortId::new(),
                    },
                    ToggleButtonUi {
                        node: NodeId::new(),
                        click_port: EventPortId::new(),
                    },
                ],
                state_label: NodeId::new(),
            },
            clicked: [false; 3],
        }
    }

    pub fn handle_event(&mut self, event: &UiEvent) -> bool {
        if event.kind != UiEventKind::Click {
            return false;
        }
        for (index, button) in self.ui.buttons.iter().enumerate() {
            if event.target == button.click_port {
                self.clicked[index] = !self.clicked[index];
                return true;
            }
        }
        false
    }

    pub fn view_tree(&self) -> HostViewTree {
        HostViewTree::from_root(
            HostViewNode::element(self.ui.root, "div")
                .with_style("display", "flex")
                .with_style("flex-direction", "column")
                .with_style("gap", COLUMN_GAP)
                .with_style("padding", ROOT_PADDING)
                .with_children(vec![
                    HostViewNode::element(self.ui.label, "div")
                        .with_text(self.program.prompt.clone()),
                    HostViewNode::element(self.ui.row, "div")
                        .with_style("display", "flex")
                        .with_style("flex-direction", "row")
                        .with_style("gap", ROW_GAP)
                        .with_children(
                            self.program
                                .button_labels
                                .iter()
                                .zip(self.ui.buttons)
                                .zip(self.clicked)
                                .map(|((label, button), clicked)| {
                                    toggle_button(
                                        button.node,
                                        label,
                                        button.click_port,
                                        clicked,
                                        CLICK_ACTIVE_COLOR,
                                        CLICK_ACTIVE_OUTLINE,
                                    )
                                })
                                .collect(),
                        ),
                    HostViewNode::element(self.ui.state_label, "div")
                        .with_text(self.state_label_text()),
                ]),
        )
    }

    fn state_label_text(&self) -> String {
        format!(
            "{} A: {}, B: {}, C: {}",
            self.program.state_prefix,
            bool_text(self.clicked[0]),
            bool_text(self.clicked[1]),
            bool_text(self.clicked[2]),
        )
    }
}

impl SwitchHoldState {
    pub fn new(program: SwitchHoldProgram) -> Self {
        Self {
            program,
            ui: SwitchHoldUi {
                root: NodeId::new(),
                active_label: NodeId::new(),
                toggle_button: ToggleButtonUi {
                    node: NodeId::new(),
                    click_port: EventPortId::new(),
                },
                count_label: NodeId::new(),
                buttons_row: NodeId::new(),
                item_a_button: ToggleButtonUi {
                    node: NodeId::new(),
                    click_port: EventPortId::new(),
                },
                item_b_button: ToggleButtonUi {
                    node: NodeId::new(),
                    click_port: EventPortId::new(),
                },
                footer_label: NodeId::new(),
            },
            show_item_a: true,
            click_counts: [0, 0],
        }
    }

    pub fn handle_event(&mut self, event: &UiEvent) -> bool {
        if event.kind != UiEventKind::Click {
            return false;
        }
        if event.target == self.ui.toggle_button.click_port {
            self.show_item_a = !self.show_item_a;
            return true;
        }
        if event.target == self.ui.item_a_button.click_port && self.show_item_a {
            self.click_counts[0] += 1;
            return true;
        }
        if event.target == self.ui.item_b_button.click_port && !self.show_item_a {
            self.click_counts[1] += 1;
            return true;
        }
        false
    }

    pub fn view_tree(&self) -> HostViewTree {
        HostViewTree::from_root(
            HostViewNode::element(self.ui.root, "div")
                .with_style("display", "flex")
                .with_style("flex-direction", "column")
                .with_style("gap", COLUMN_GAP)
                .with_style("padding", ROOT_PADDING)
                .with_children(vec![
                    HostViewNode::element(self.ui.active_label, "div")
                        .with_text(self.active_label_text()),
                    static_button(
                        self.ui.toggle_button.node,
                        &self.program.toggle_label,
                        self.ui.toggle_button.click_port,
                        TOGGLE_COLOR,
                    ),
                    HostViewNode::element(self.ui.count_label, "div")
                        .with_text(self.count_label_text()),
                    HostViewNode::element(self.ui.buttons_row, "div")
                        .with_style("display", "flex")
                        .with_style("flex-direction", "row")
                        .with_style("gap", ROW_GAP)
                        .with_children(vec![
                            static_button(
                                self.ui.item_a_button.node,
                                &self.program.item_button_labels[0],
                                self.ui.item_a_button.click_port,
                                ITEM_A_COLOR,
                            ),
                            static_button(
                                self.ui.item_b_button.node,
                                &self.program.item_button_labels[1],
                                self.ui.item_b_button.click_port,
                                ITEM_B_COLOR,
                            ),
                        ]),
                    HostViewNode::element(self.ui.footer_label, "div")
                        .with_text(self.program.footer_hint.clone()),
                ]),
        )
    }

    fn active_label_text(&self) -> String {
        format!(
            "{}{}",
            self.program.active_prefix,
            if self.show_item_a { "Item A" } else { "Item B" }
        )
    }

    fn count_label_text(&self) -> String {
        if self.show_item_a {
            format!("Item A clicks: {}", self.click_counts[0])
        } else {
            format!("Item B clicks: {}", self.click_counts[1])
        }
    }
}

fn bool_text(value: bool) -> &'static str {
    if value { "True" } else { "False" }
}

fn hover_button(node: NodeId, label: &str, hovered: bool) -> HostViewNode {
    let mut button = base_button(node, label).with_style(
        "background",
        if hovered {
            HOVER_ACTIVE_COLOR
        } else {
            BUTTON_BASE_COLOR
        },
    );
    if hovered {
        button = button.with_style("outline", HOVER_ACTIVE_OUTLINE);
    } else {
        button = button.with_style("outline", "none");
    }
    button
}

fn toggle_button(
    node: NodeId,
    label: &str,
    click_port: EventPortId,
    active: bool,
    active_color: &str,
    active_outline: &str,
) -> HostViewNode {
    let mut button = base_button(node, label)
        .with_event_port(click_port, UiEventKind::Click)
        .with_style(
            "background",
            if active {
                active_color
            } else {
                BUTTON_BASE_COLOR
            },
        );
    if active {
        button = button.with_style("outline", active_outline);
    } else {
        button = button.with_style("outline", "none");
    }
    button
}

fn static_button(
    node: NodeId,
    label: &str,
    click_port: EventPortId,
    background: &str,
) -> HostViewNode {
    base_button(node, label)
        .with_event_port(click_port, UiEventKind::Click)
        .with_style("background", background)
}

fn base_button(node: NodeId, label: &str) -> HostViewNode {
    HostViewNode::element(node, "button")
        .with_text(label.to_string())
        .with_style("padding", BUTTON_PADDING)
        .with_style("border-radius", BUTTON_RADIUS)
        .with_style("color", BUTTON_TEXT_COLOR)
        .with_style("border", "none")
}
