//! Verification harness: render the real app for a few frames, ask egui for its
//! own framebuffer, and write it out as a BMP.
//!
//! OS-level screen capture (BitBlt, PrintWindow) cannot read back an
//! OpenGL-composited window - it returns a blank surface - so this asks the
//! renderer directly instead. Useful for checking the UI from a script or CI.
//!
//! ```text
//! cargo run --bin shot -- out.bmp left.txt right.txt
//! ```

use std::io::Write;

use duibi::app::DuiBi;

struct Shooter {
    inner: DuiBi,
    frame_no: u32,
    out: String,
    requested: bool,
    /// Keystrokes to inject, as `(frame, event)`. Lets a scripted run exercise
    /// the find bar and caret movement, which otherwise need a human.
    script: Vec<(u32, egui::Event)>,
}

impl eframe::App for Shooter {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.frame_no += 1;

        // Injected before the app runs, so it sees them as ordinary input.
        let now = self.frame_no;
        let due: Vec<egui::Event> = self
            .script
            .iter()
            .filter(|(f, _)| *f == now)
            .map(|(_, e)| e.clone())
            .collect();
        if !due.is_empty() {
            ctx.input_mut(|i| i.events.extend(due));
        }

        self.inner.ui(ui, frame);
        ctx.request_repaint();

        // Give layout, fonts and the debounced comparison time to settle.
        if self.frame_no == 90 && !self.requested {
            self.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }

        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            write_bmp(&self.out, &image).expect("failed to write the screenshot");
            eprintln!("SHOT {}x{} -> {}", image.width(), image.height(), self.out);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// Minimal 24-bit BMP writer, so this needs no image encoder.
fn write_bmp(path: &str, img: &egui::ColorImage) -> std::io::Result<()> {
    let (w, h) = (img.width(), img.height());
    let row_pad = (4 - (w * 3) % 4) % 4;
    let data_len = (w * 3 + row_pad) * h;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);

    f.write_all(b"BM")?;
    f.write_all(&((54 + data_len) as u32).to_le_bytes())?;
    f.write_all(&0u32.to_le_bytes())?;
    f.write_all(&54u32.to_le_bytes())?;
    f.write_all(&40u32.to_le_bytes())?;
    f.write_all(&(w as i32).to_le_bytes())?;
    f.write_all(&(h as i32).to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&24u16.to_le_bytes())?;
    for _ in 0..6 {
        f.write_all(&0u32.to_le_bytes())?;
    }
    // BMP rows run bottom-up.
    for y in (0..h).rev() {
        for x in 0..w {
            let p = img[(x, y)];
            f.write_all(&[p.b(), p.g(), p.r()])?;
        }
        f.write_all(&vec![0u8; row_pad])?;
    }
    Ok(())
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).cloned().unwrap_or_else(|| "shot.bmp".into());
    let left = args.get(2).cloned();
    let right = args.get(3).cloned();

    // Optional 5th argument: a scripted interaction to exercise before the
    // screenshot is taken.
    //
    // Only keyboard events are useful here. Pointer events would have to be in
    // the *raw* input to update egui's pointer state, and injecting them at
    // this point is too late - which is also why focus behaviour is covered by
    // `tests/keyboard_focus.rs` instead of by a screenshot.
    let key = |k: egui::Key, modifiers: egui::Modifiers| egui::Event::Key {
        key: k,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    let script: Vec<(u32, egui::Event)> = match args.get(4).map(String::as_str) {
        Some("find") => vec![
            (5, key(egui::Key::F, egui::Modifiers::COMMAND)),
            (12, egui::Event::Text("Metrics".to_owned())),
        ],
        Some("down") => {
            // Down through a short line and out the other side: the caret must
            // come back at the column it started in.
            let mut v = vec![(5, key(egui::Key::ArrowRight, egui::Modifiers::NONE)); 1];
            for i in 0..14 {
                v.push((6 + i, key(egui::Key::ArrowRight, egui::Modifiers::NONE)));
            }
            for i in 0..6 {
                v.push((24 + i * 2, key(egui::Key::ArrowDown, egui::Modifiers::NONE)));
            }
            v
        }
        // Walk the caret far below the fold; the view must follow it.
        Some("scroll") => (0..60)
            .map(|i| (6 + i, key(egui::Key::ArrowDown, egui::Modifiers::NONE)))
            .collect(),
        _ => Vec::new(),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1500.0, 900.0]),
        ..Default::default()
    };
    eframe::run_native(
        "DuiBi",
        options,
        Box::new(move |cc| {
            let mut app = DuiBi::new(cc);
            app.open_at_startup(
                left.as_deref().map(std::path::Path::new),
                right.as_deref().map(std::path::Path::new),
            );
            Ok(Box::new(Shooter {
                inner: app,
                frame_no: 0,
                out,
                requested: false,
                script: script.clone(),
            }) as Box<dyn eframe::App>)
        }),
    )
}
