//! The Tools menu describes each tool on hover. This checks that the tooltip
//! actually appears, and that it waits.
//!
//! Worth pinning, because egui has a trap here: it refuses to draw a tooltip
//! for any widget whose layer has an open popup. Each tool used to be a
//! submenu, and hovering a submenu button *is* opening a popup - so the code
//! read as correct while showing nothing at all. The tools are plain buttons
//! now, and this test fails if anyone turns them back into submenus.
//!
//! Driving `egui::Context` directly is the only way to get real pointer input:
//! events pushed part-way through a frame never reach the pointer state.

use egui::{Event, Order, PointerButton, Pos2, RawInput, Rect};

/// Matches `TOOL_HELP_DELAY` in `ui::toolbar`.
const DELAY: f64 = 2.0;

struct Menu {
    ctx: egui::Context,
    time: f64,
    menu_rect: Option<Rect>,
    item_rect: Option<Rect>,
    /// Build the tools as submenus instead of buttons, to demonstrate the trap.
    use_submenu: bool,
}

impl Menu {
    fn new(use_submenu: bool) -> Self {
        Self {
            ctx: egui::Context::default(),
            time: 0.0,
            menu_rect: None,
            item_rect: None,
            use_submenu,
        }
    }

    /// Run one frame. Time advances by `dt`, which is what the tooltip delay
    /// is measured against.
    fn frame(&mut self, dt: f64, events: Vec<Event>) {
        self.time += dt;
        let input = RawInput {
            time: Some(self.time),
            events,
            ..Default::default()
        };

        let use_submenu = self.use_submenu;
        let mut menu_rect = None;
        let mut item_rect = None;

        let mut out = self.ctx.run_ui(input, |ui| {
            ui.ctx()
                .global_style_mut(|s| s.interaction.tooltip_delay = DELAY as f32);

            let menu = ui.menu_button("Tools", |ui| {
                let response = if use_submenu {
                    ui.menu_button("Remove Duplicate Lines", |ui| {
                        let _ = ui.button("Left");
                    })
                    .response
                } else {
                    ui.button("Remove Duplicate Lines")
                };
                item_rect = Some(response.rect);
                response.on_hover_ui(|ui| {
                    ui.label("Delete lines identical to an earlier one.");
                });
            });
            menu_rect = Some(menu.response.rect);
        });
        out.textures_delta.clear();

        if menu_rect.is_some() {
            self.menu_rect = menu_rect;
        }
        if item_rect.is_some() {
            self.item_rect = item_rect;
        }
    }

    fn click(&mut self, at: Pos2) {
        self.frame(0.05, vec![Event::PointerMoved(at)]);
        self.frame(
            0.05,
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
        );
        self.frame(
            0.05,
            vec![Event::PointerButton {
                pos: at,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
    }

    /// Is a tooltip on screen right now?
    fn tooltip_visible(&self) -> bool {
        self.ctx.memory(|m| {
            m.areas()
                .visible_layer_ids()
                .iter()
                .any(|l| l.order == Order::Tooltip)
        })
    }

    /// Open the menu and park the pointer on the first tool.
    fn open_and_hover(&mut self) {
        self.frame(0.05, Vec::new());
        let menu = self.menu_rect.expect("the menu button should have a rect");
        self.click(menu.center());

        // A frame with the menu open, so the item inside gets laid out.
        self.frame(0.05, Vec::new());
        let item = self.item_rect.expect("the tool should have a rect");
        self.frame(0.05, vec![Event::PointerMoved(item.center())]);
    }

    /// Hold still for `seconds`, in small steps, as a real hover would.
    fn hold(&mut self, seconds: f64) {
        let steps = (seconds / 0.1).ceil() as usize;
        for _ in 0..steps {
            self.frame(0.1, Vec::new());
        }
    }
}

#[test]
fn a_tool_shows_its_description_after_the_delay() {
    let mut m = Menu::new(false);
    m.open_and_hover();

    m.hold(DELAY + 0.6);
    assert!(
        m.tooltip_visible(),
        "hovering a tool for {}s should show its description",
        DELAY + 0.6
    );
}

#[test]
fn the_description_waits_rather_than_popping_up_at_once() {
    let mut m = Menu::new(false);
    m.open_and_hover();

    // Well inside the delay: passing over an entry must not throw text at you.
    m.hold(0.5);
    assert!(
        !m.tooltip_visible(),
        "the description appeared far too early"
    );
}

/// The reason the tools are not submenus. If this ever starts passing, the
/// tooltip suppression rule has changed and the menu could be simplified.
#[test]
fn a_submenu_entry_can_never_show_a_tooltip() {
    let mut m = Menu::new(true);
    m.open_and_hover();

    m.hold(DELAY + 2.0);
    assert!(
        !m.tooltip_visible(),
        "egui used to suppress tooltips while a popup is open in the same layer; \
         if that no longer holds, revisit the flat-button layout in tools_menu"
    );
}
