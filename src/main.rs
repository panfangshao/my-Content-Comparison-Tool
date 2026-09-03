//! DuiBi - a local-first text compare and merge tool.
//!
//! Nothing typed into this program leaves the machine: there is no network
//! code anywhere in the dependency tree that this binary reaches.

// Do not pop a console window behind the GUI on Windows release builds. Debug
// builds keep it, because that is where panic messages and logs show up.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use duibi::app::DuiBi;
use duibi::config::Config;

/// `duibi [LEFT [RIGHT]]` - open one or both files at startup, so the tool can
/// be wired into a shell alias or used as a git difftool.
fn parse_args() -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {
    let mut files = std::env::args_os().skip(1).filter_map(|a| {
        let s = a.to_string_lossy().into_owned();
        // Leave room for future flags without treating them as filenames.
        (!s.starts_with('-')).then(|| std::path::PathBuf::from(a))
    });
    (files.next(), files.next())
}

fn main() -> eframe::Result {
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!(
            "DuiBi - local text compare & merge

             USAGE:
    duibi [LEFT] [RIGHT]

             Opens the two files side by side. With no arguments, starts empty.
             All processing is local; nothing is ever uploaded."
        );
        return Ok(());
    }
    let (left, right) = parse_args();

    // Read the window geometry before creating the window, so a restart comes
    // back the size the user left it.
    let saved = Config::load();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DuiBi")
            .with_inner_size(saved.window_size)
            .with_min_inner_size([720.0, 420.0])
            .with_maximized(saved.window_maximized)
            .with_app_id("duibi"),
        centered: true,
        // Let eframe keep the window geometry in its own storage too: it is
        // saved on a timer (and on exit), so even a crash keeps the size the
        // user left. The config file remains the source read at startup.
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "DuiBi",
        options,
        Box::new(move |cc| {
            let mut app = DuiBi::new(cc);
            app.open_at_startup(left.as_deref(), right.as_deref());
            Ok(Box::new(app))
        }),
    )
}
