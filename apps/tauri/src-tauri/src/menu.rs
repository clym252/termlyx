//! The macOS app menu: Tauri's default menu, except that Quit and Close
//! Window go through the window's close request, so the UI's quit
//! confirmation (sessions and transfers still live) sees them.
//!
//! The predefined Quit item sends `terminate:`, which tao (0.37) does not
//! let the app cancel (it implements only `applicationWillTerminate`), so
//! it would end live sessions without asking. Here "Quit Termlyx" (Cmd+Q) is
//! our own item that asks every window to close (`Window::close`, the same
//! close request as the window's close button). The predefined Close Window
//! item is replaced by one without an accelerator, so Cmd+W reaches the
//! page (it closes the focused terminal pane).
//!
//! Still unguarded: Quit from the Dock menu, logout and shutdown, which
//! send `terminate:` to the app directly.
//!
//! Windows and Linux have no app menu by default (Tauri adds one on macOS
//! only); there, closing the window (Alt+F4, the title bar) is the close
//! request the UI already guards.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Manager, Runtime};

/// Menu id of "Quit Termlyx".
pub const QUIT_ID: &str = "tether-quit";
/// Menu id of "Close Window" (no accelerator).
pub const CLOSE_WINDOW_ID: &str = "tether-close-window";

/// One entry of the menu, as data (so the layout can be tested without a
/// running app).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
    Separator,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    Fullscreen,
    Minimize,
    Maximize,
    /// Our own item: `(id, text, accelerator)`.
    Custom(&'static str, &'static str, Option<&'static str>),
}

/// The menu: `(submenu title, entries)`; `None` titles the app menu with
/// the app's name.
pub fn layout() -> Vec<(Option<&'static str>, Vec<Entry>)> {
    use Entry::*;
    vec![
        (
            None,
            vec![
                About,
                Separator,
                Services,
                Separator,
                Hide,
                HideOthers,
                ShowAll,
                Separator,
                Custom(QUIT_ID, "Quit Termlyx", Some("CmdOrCtrl+Q")),
            ],
        ),
        (
            Some("File"),
            vec![Custom(CLOSE_WINDOW_ID, "Close Window", None)],
        ),
        (
            Some("Edit"),
            vec![Undo, Redo, Separator, Cut, Copy, Paste, SelectAll],
        ),
        (Some("View"), vec![Fullscreen]),
        (Some("Window"), vec![Minimize, Maximize]),
    ]
}

fn item<R: Runtime>(
    app: &AppHandle<R>,
    entry: Entry,
) -> tauri::Result<Box<dyn tauri::menu::IsMenuItem<R>>> {
    Ok(match entry {
        Entry::About => Box::new(PredefinedMenuItem::about(app, None, None)?),
        Entry::Services => Box::new(PredefinedMenuItem::services(app, None)?),
        Entry::Hide => Box::new(PredefinedMenuItem::hide(app, None)?),
        Entry::HideOthers => Box::new(PredefinedMenuItem::hide_others(app, None)?),
        Entry::ShowAll => Box::new(PredefinedMenuItem::show_all(app, None)?),
        Entry::Separator => Box::new(PredefinedMenuItem::separator(app)?),
        Entry::Undo => Box::new(PredefinedMenuItem::undo(app, None)?),
        Entry::Redo => Box::new(PredefinedMenuItem::redo(app, None)?),
        Entry::Cut => Box::new(PredefinedMenuItem::cut(app, None)?),
        Entry::Copy => Box::new(PredefinedMenuItem::copy(app, None)?),
        Entry::Paste => Box::new(PredefinedMenuItem::paste(app, None)?),
        Entry::SelectAll => Box::new(PredefinedMenuItem::select_all(app, None)?),
        Entry::Fullscreen => Box::new(PredefinedMenuItem::fullscreen(app, None)?),
        Entry::Minimize => Box::new(PredefinedMenuItem::minimize(app, None)?),
        Entry::Maximize => Box::new(PredefinedMenuItem::maximize(app, None)?),
        Entry::Custom(id, text, accelerator) => {
            Box::new(MenuItem::with_id(app, id, text, true, accelerator)?)
        }
    })
}

/// Builds [`layout`] for `app`.
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    for (title, entries) in layout() {
        let title = title.map_or_else(|| app.package_info().name.clone(), str::to_owned);
        let submenu = Submenu::new(app, title, true)?;
        for entry in entries {
            submenu.append(item(app, entry)?.as_ref())?;
        }
        menu.append(&submenu)?;
    }
    Ok(menu)
}

/// Quit and Close Window: ask the window(s) to close, which the UI may
/// cancel (live sessions or transfers) or confirm.
pub fn on_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    let id = event.id().as_ref();
    let windows: Vec<_> = if id == QUIT_ID {
        app.webview_windows().into_values().collect()
    } else if id == CLOSE_WINDOW_ID {
        app.webview_windows()
            .into_values()
            .filter(|w| w.is_focused().unwrap_or(false))
            .collect()
    } else {
        return;
    };
    for window in windows {
        // The close request may open the quit dialog in this window: bring
        // it up first, or a minimised or hidden window shows it to no one
        // and Quit seems to do nothing. Best effort; the close still goes.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        if let Err(e) = window.close() {
            tracing::warn!(error = %e, "close request failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry> {
        layout().into_iter().flat_map(|(_, e)| e).collect()
    }

    #[test]
    fn quit_is_our_guarded_item_with_cmd_q() {
        let quits: Vec<_> = entries()
            .into_iter()
            .filter(|e| matches!(e, Entry::Custom(id, ..) if *id == QUIT_ID))
            .collect();
        assert_eq!(
            quits,
            vec![Entry::Custom(QUIT_ID, "Quit Termlyx", Some("CmdOrCtrl+Q"))]
        );
        assert_eq!("Quit Termlyx", format!("Quit {}", crate::APP_NAME));
        // The menu has no entry kind for the predefined Quit or Close
        // Window (both bypass the UI); the only accelerators are ours.
        let accelerators: Vec<_> = entries()
            .into_iter()
            .filter_map(|e| match e {
                Entry::Custom(_, _, Some(a)) => Some(a),
                _ => None,
            })
            .collect();
        assert_eq!(accelerators, vec!["CmdOrCtrl+Q"]);
    }

    #[test]
    fn close_window_has_no_accelerator_so_cmd_w_reaches_the_page() {
        assert!(
            entries().contains(&Entry::Custom(CLOSE_WINDOW_ID, "Close Window", None)),
            "{:?}",
            entries()
        );
    }

    #[test]
    fn keeps_the_standard_edit_items() {
        let edit = layout()
            .into_iter()
            .find(|(t, _)| *t == Some("Edit"))
            .expect("Edit menu")
            .1;
        for e in [
            Entry::Undo,
            Entry::Redo,
            Entry::Cut,
            Entry::Copy,
            Entry::Paste,
            Entry::SelectAll,
        ] {
            assert!(edit.contains(&e), "missing {e:?}");
        }
    }
}
