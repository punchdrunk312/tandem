//! The picture that follows the cursor while files are carried across the screen edge.
//! A small transparent window, click-through and never focused; the page inside
//! (`ghost.html`) says what to show, this module keeps it under the pointer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Manager, PhysicalPosition};

const LABEL: &str = "ghost";
static ON: AtomicBool = AtomicBool::new(false);

fn follow(app: &AppHandle) {
    let (Some(win), Some((x, y))) = (
        app.get_webview_window(LABEL),
        crate::island::cursor_physical(),
    ) else {
        return;
    };
    // Hugging the pointer's tip (the page draws its picture at the window's top-left).
    let _ = win.set_position(PhysicalPosition::new(x as i32 + 4, y as i32 + 6));
}

pub fn set(app: &AppHandle, on: bool) {
    ON.store(on, Ordering::Relaxed);
    let Some(win) = app.get_webview_window(LABEL) else {
        return;
    };
    if on {
        crate::island::make_non_activating(&win);
        let _ = win.set_ignore_cursor_events(true);
        follow(app);
        crate::island::show_no_activate(&win);
        let _ = win.set_always_on_top(true);
    } else {
        let _ = win.hide();
    }
}

pub fn spawn(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("tandem-ghost".into())
        .spawn(move || loop {
            if ON.load(Ordering::Relaxed) {
                follow(&app);
                std::thread::sleep(Duration::from_millis(8));
            } else {
                std::thread::sleep(Duration::from_millis(120));
            }
        });
}
