//! Regression test for the editor keeping the navigation keys.
//!
//! egui decides at `begin_pass` - before any application code runs - whether an
//! arrow key moves the caret or moves *focus* to a neighbouring widget. The
//! decision is made from the focused widget's [`egui::EventFilter`], which
//! defaults to "I want none of these keys".
//!
//! A hand-drawn editor therefore has to claim them explicitly, exactly as
//! `TextEdit` does. Without that, pressing Right in the left pane walked focus
//! onto the merge arrows instead of moving the caret.
//!
//! This drives `egui::Context` directly rather than going through the app,
//! because the input has to arrive as *raw* input to exercise the focus system
//! at all - anything pushed later in the frame is already too late.

use egui::{Event, EventFilter, Id, Key, Modifiers, RawInput, Sense, Vec2};

/// What the editor pane asks for: everything except Escape, which must stay
/// with egui so it can still close the find bar.
const EDITOR_FILTER: EventFilter = EventFilter {
    tab: true,
    horizontal_arrows: true,
    vertical_arrows: true,
    escape: false,
};

fn key(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

/// A miniature of the real layout: a focusable "pane" with a clickable
/// "merge arrow" to its right, both inside a horizontal row.
struct Harness {
    ctx: egui::Context,
    pane: Option<Id>,
    button: Option<Id>,
    claim: bool,
    frames: u32,
}

impl Harness {
    fn new(claim: bool) -> Self {
        Self {
            ctx: egui::Context::default(),
            pane: None,
            button: None,
            claim,
            frames: 0,
        }
    }

    /// Run one frame with the given events.
    fn frame(&mut self, events: Vec<Event>) {
        let input = RawInput {
            events,
            ..Default::default()
        };
        let claim = self.claim;
        let first_frame = self.frames == 0;
        self.frames += 1;
        let mut pane = None;
        let mut button = None;

        let mut out = self.ctx.run_ui(input, |ui| {
            ui.horizontal(|ui| {
                let (_rect, response) =
                    ui.allocate_exact_size(Vec2::new(120.0, 60.0), Sense::click_and_drag());
                pane = Some(response.id);

                // Take focus on the first frame only, as the app does at
                // startup. Re-grabbing it every frame would undo Escape.
                if first_frame {
                    response.request_focus();
                }
                if claim && response.has_focus() {
                    ui.memory_mut(|m| m.set_focus_lock_filter(response.id, EDITOR_FILTER));
                }

                button = Some(ui.button("merge").id);
            });
        });
        // Nothing renders these in a headless run, and `TexturesDelta` panics
        // on drop if they are left unhandled.
        out.textures_delta.clear();

        self.pane = pane;
        self.button = button;
    }

    fn focused(&self) -> Option<Id> {
        self.ctx.memory(|m| m.focused())
    }

    /// Settle focus. `set_focus_lock_filter` only takes effect once the widget
    /// has held focus for a full frame, so a couple of quiet frames are needed
    /// before the filter is live - the same warm-up a real `TextEdit` has.
    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(Vec::new());
        }
        assert_eq!(self.focused(), self.pane, "the pane should start focused");
    }
}

#[test]
fn without_the_filter_an_arrow_key_steals_focus() {
    // This is the bug, pinned so it cannot come back silently.
    let mut h = Harness::new(false);
    h.settle();

    h.frame(vec![key(Key::ArrowRight)]);
    assert_ne!(
        h.focused(),
        h.pane,
        "egui is expected to move focus away when the widget claims nothing"
    );
}

#[test]
fn the_editor_filter_keeps_horizontal_arrows() {
    let mut h = Harness::new(true);
    h.settle();

    for _ in 0..5 {
        h.frame(vec![key(Key::ArrowRight)]);
        assert_eq!(h.focused(), h.pane, "Right must not move focus");
    }
    for _ in 0..5 {
        h.frame(vec![key(Key::ArrowLeft)]);
        assert_eq!(h.focused(), h.pane, "Left must not move focus");
    }
}

#[test]
fn the_editor_filter_keeps_vertical_arrows() {
    let mut h = Harness::new(true);
    h.settle();

    for k in [Key::ArrowUp, Key::ArrowDown, Key::ArrowUp, Key::ArrowDown] {
        h.frame(vec![key(k)]);
        assert_eq!(h.focused(), h.pane, "{k:?} must not move focus");
    }
}

#[test]
fn the_editor_filter_keeps_tab() {
    // Tab indents the selection, so it must not tab away to the next widget.
    let mut h = Harness::new(true);
    h.settle();

    h.frame(vec![key(Key::Tab)]);
    assert_eq!(h.focused(), h.pane, "Tab must not move focus");
}

#[test]
fn escape_still_releases_focus() {
    // Deliberately *not* claimed: Escape closing the find bar and dropping
    // focus is the behaviour we want to keep.
    let mut h = Harness::new(true);
    h.settle();

    h.frame(vec![key(Key::Escape)]);
    assert_ne!(h.focused(), h.pane, "Escape should still surrender focus");
}

/// The arrow keys must still reach the widget as ordinary events - claiming
/// them stops focus navigation, it does not consume them.
#[test]
fn claimed_arrows_are_still_delivered_as_events() {
    let mut h = Harness::new(true);
    h.settle();

    let mut seen = 0usize;
    let input = RawInput {
        events: vec![key(Key::ArrowRight)],
        ..Default::default()
    };
    let mut out = h.ctx.run_ui(input, |ui| {
        ui.horizontal(|ui| {
            let (_rect, response) =
                ui.allocate_exact_size(Vec2::new(120.0, 60.0), Sense::click_and_drag());
            if response.has_focus() {
                ui.memory_mut(|m| m.set_focus_lock_filter(response.id, EDITOR_FILTER));
                seen = ui.input(|i| {
                    i.events
                        .iter()
                        .filter(|e| matches!(e, Event::Key { key: Key::ArrowRight, .. }))
                        .count()
                });
            }
            let _ = ui.button("merge");
        });
    });
    out.textures_delta.clear();
    assert_eq!(seen, 1, "the caret handler still needs to see the keypress");
}
