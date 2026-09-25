use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;

use crate::dbus::{Bridge, now_secs};
use crate::model::{EntryMeta, Kind};

const RECENT_LIMIT: u32 = 300;
const SEARCH_DEBOUNCE_MS: u64 = 80;

thread_local! {
    static ACTIVE: RefCell<Option<Popup>> = const { RefCell::new(None) };
}

pub fn show(app: &adw::Application, bridge: Arc<Bridge>, query: &str) {
    ACTIVE.with(|slot| {
        if slot.borrow().is_some() {
            let slot = slot.borrow();
            let popup = slot.as_ref().expect("checked above");
            popup.focus_with(query);
        } else {
            let popup = Popup::new(app, bridge, query);
            *slot.borrow_mut() = Some(popup);
        }
    });
}

pub struct Popup {
    window: adw::ApplicationWindow,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    status: gtk::Label,
    toast: adw::ToastOverlay,
    bridge: Arc<Bridge>,
}

impl Popup {
    fn new(app: &adw::Application, bridge: Arc<Bridge>, query: &str) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Clipboard")
            .default_width(680)
            .default_height(520)
            .build();
        window.set_decorated(false);

        let toolbar = adw::ToolbarView::new();

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new("Clipboard", "GNOME Clipway")));
        let clear_button = gtk::Button::from_icon_name("user-trash-symbolic");
        clear_button.set_tooltip_text(Some("Clear history"));
        header.pack_start(&clear_button);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let banner =
            adw::Banner::new("Incognito mode is on. New clipboard content is not captured.");
        banner.set_revealed(bridge.snapshot().incognito);
        content.append(&banner);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search clipboard history"));
        search.set_hexpand(true);
        search.set_margin_top(12);
        search.set_margin_bottom(6);
        search.set_margin_start(12);
        search.set_margin_end(12);
        content.append(&search);

        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("boxed-list");
        list.set_margin_start(12);
        list.set_margin_end(12);
        list.set_margin_bottom(12);

        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .build();
        scroller.set_child(Some(&list));
        content.append(&scroller);

        let status = gtk::Label::new(Some(""));
        status.add_css_class("dim-label");
        status.set_margin_top(6);
        status.set_margin_bottom(10);
        content.append(&status);

        toolbar.set_content(Some(&content));

        let toast = adw::ToastOverlay::new();
        toast.set_child(Some(&toolbar));
        window.set_content(Some(&toast));

        let debounce = Rc::new(Cell::new(None::<glib::SourceId>));
        let popup = Self {
            window: window.clone(),
            search: search.clone(),
            list: list.clone(),
            status: status.clone(),
            toast: toast.clone(),
            bridge: bridge.clone(),
        };

        {
            let debounce = debounce.clone();
            search.connect_search_changed(move |_| {
                if let Some(id) = debounce.replace(None) {
                    id.remove();
                }
                let cell = debounce.clone();
                debounce.set(Some(glib::timeout_add_local(
                    Duration::from_millis(SEARCH_DEBOUNCE_MS),
                    move || {
                        cell.set(None);
                        refresh_active();
                        glib::ControlFlow::Break
                    },
                )));
            });
        }

        {
            let bridge = bridge.clone();
            let window = window.clone();
            search.connect_activate(move |_| {
                if let Some(id) = first_entry_id() {
                    paste_and_hide(bridge.clone(), id, window.downgrade());
                }
            });
        }

        {
            let bridge = bridge.clone();
            let window = window.clone();
            clear_button.connect_clicked(move |_| {
                confirm_clear(&window, &bridge);
            });
        }

        {
            let window_for_keys = window.clone();
            let key = gtk::EventControllerKey::new();
            key.connect_key_pressed(move |_, keyval, _, _| {
                if keyval == gtk::gdk::Key::Escape {
                    window_for_keys.set_visible(false);
                    return glib::Propagation::Stop;
                }
                if keyval == gtk::gdk::Key::Delete {
                    delete_selected();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            window.add_controller(key);
        }

        {
            let window_for_close = window.clone();
            window.connect_close_request(move |_| {
                window_for_close.set_visible(false);
                glib::Propagation::Stop
            });
        }

        popup.focus_with(query);
        popup
    }

    fn focus_with(&self, query: &str) {
        if !query.is_empty() {
            self.search.set_text(query);
        }
        self.refresh();
        self.window.present();
        self.search.grab_focus();
    }

    fn refresh(&self) {
        let query = self.search.text().to_string();
        let entries = if query.trim().is_empty() {
            self.bridge.recent(RECENT_LIMIT)
        } else {
            self.bridge.search(query.trim(), RECENT_LIMIT)
        }
        .unwrap_or_default();

        self.list.remove_all();
        for meta in &entries {
            let row = self.build_row(meta);
            self.list.append(&row);
        }
        if let Some(row) = self
            .list
            .first_child()
            .and_then(|first| first.downcast::<gtk::ListBoxRow>().ok())
        {
            self.list.select_row(Some(&row));
        }
        let noun = if entries.len() == 1 {
            "entry"
        } else {
            "entries"
        };
        self.status.set_text(&format!("{} {noun}", entries.len()));
    }

    fn build_row(&self, meta: &EntryMeta) -> gtk::ListBoxRow {
        let row = adw::ActionRow::new();
        row.set_widget_name(&meta.id.to_string());
        row.set_title(&meta.preview);
        let time = relative_time(meta.ts);
        if meta.source.is_empty() {
            row.set_subtitle(&time);
        } else {
            row.set_subtitle(&format!("{} · {time}", meta.source));
        }
        row.set_activatable(true);

        if let Ok(Some(entry)) = self.bridge.store().get(meta.id) {
            match meta.kind {
                Kind::Image => {
                    let picture = gtk::Picture::new();
                    picture.set_content_fit(gtk::ContentFit::Cover);
                    picture.set_size_request(32, 32);
                    if let Ok(texture) =
                        gtk::gdk::Texture::from_bytes(&glib::Bytes::from(entry.data.as_slice()))
                    {
                        picture.set_paintable(Some(&texture));
                    }
                    row.add_prefix(&picture);
                }
                Kind::Text => {
                    if let Ok(text) = String::from_utf8(entry.data) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            row.set_tooltip_text(Some(trimmed));
                        }
                    }
                }
                Kind::Files => {}
            }
        }

        if meta.kind != Kind::Image {
            let icon = gtk::Image::from_icon_name(meta.kind.icon_name());
            icon.set_pixel_size(20);
            row.add_prefix(&icon);
        }

        let pin = gtk::ToggleButton::new();
        pin.set_icon_name(if meta.pinned {
            "starred-symbolic"
        } else {
            "non-starred-symbolic"
        });
        pin.set_valign(gtk::Align::Center);
        pin.set_active(meta.pinned);
        let bridge = self.bridge.clone();
        let id = meta.id;
        pin.connect_clicked(move |button| {
            let _ = bridge.set_pinned(id, button.is_active());
            refresh_active();
        });
        row.add_suffix(&pin);

        let bridge = self.bridge.clone();
        let id = meta.id;
        let window = self.window.downgrade();
        row.connect_activated(move |_| {
            paste_and_hide(bridge.clone(), id, window.clone());
        });

        row.upcast()
    }
}

pub fn schedule_screenshot(app: adw::Application, path: String) {
    glib::timeout_add_local(Duration::from_millis(800), move || {
        let saved = ACTIVE.with(|slot| {
            let slot = slot.borrow();
            slot.as_ref()
                .is_some_and(|popup| capture(&popup.window, &path))
        });
        if !saved {
            eprintln!("clipway: screenshot capture failed");
        }
        app.quit();
        glib::ControlFlow::Break
    });
}

fn capture(window: &adw::ApplicationWindow, path: &str) -> bool {
    let width = window.width().max(1);
    let height = window.height().max(1);
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let Some(node) = snapshot.to_node() else {
        return false;
    };
    let Some(renderer) = window.native().and_then(|native| native.renderer()) else {
        return false;
    };
    renderer
        .render_texture(&node, None)
        .save_to_png(std::path::Path::new(path))
        .is_ok()
}

fn refresh_active() {
    ACTIVE.with(|slot| {
        if let Some(popup) = slot.borrow().as_ref() {
            popup.refresh();
        }
    });
}

fn first_entry_id() -> Option<i64> {
    ACTIVE.with(|slot| {
        let slot = slot.borrow();
        let popup = slot.as_ref()?;
        let row = popup.list.first_child()?;
        let name = row.downcast_ref::<gtk::ListBoxRow>()?.widget_name();
        if name.is_empty() {
            return None;
        }
        name.parse().ok()
    })
}

fn delete_selected() {
    ACTIVE.with(|slot| {
        let slot = slot.borrow();
        let Some(popup) = slot.as_ref() else {
            return;
        };
        let selected = popup.list.selected_row().or_else(|| {
            popup
                .list
                .first_child()
                .and_then(|widget| widget.downcast::<gtk::ListBoxRow>().ok())
        });
        let Some(row) = selected else {
            return;
        };
        let name = row.widget_name();
        if name.is_empty() {
            return;
        }
        let Ok(id) = name.parse::<i64>() else {
            return;
        };
        let bridge = popup.bridge.clone();
        if bridge.delete(id).unwrap_or(false) {
            popup.toast.add_toast(adw::Toast::new("Entry deleted"));
            popup.refresh();
        }
    });
}

fn paste_and_hide(bridge: Arc<Bridge>, id: i64, window: glib::WeakRef<adw::ApplicationWindow>) {
    glib::spawn_future_local(async move {
        let pasted = bridge.paste(id).await;
        if let Some(window) = window.upgrade() {
            if pasted {
                window.set_visible(false);
            } else {
                ACTIVE.with(|slot| {
                    if let Some(popup) = slot.borrow().as_ref() {
                        popup
                            .toast
                            .add_toast(adw::Toast::new("Could not set the clipboard"));
                    }
                });
            }
        }
    });
}

fn confirm_clear(window: &adw::ApplicationWindow, bridge: &Arc<Bridge>) {
    let dialog = adw::AlertDialog::new(
        Some("Clear clipboard history?"),
        Some("Every entry, including pinned ones, will be deleted."),
    );
    dialog.add_responses(&[("cancel", "_Cancel"), ("clear", "_Clear")]);
    dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let bridge = bridge.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "clear" {
            let bridge = bridge.clone();
            glib::spawn_future_local(async move {
                let _ = bridge.clear(false).await;
                refresh_active();
                ACTIVE.with(|slot| {
                    if let Some(popup) = slot.borrow().as_ref() {
                        popup.toast.add_toast(adw::Toast::new("History cleared"));
                    }
                });
            });
        }
        dialog.close();
    });
    dialog.present(Some(window));
}

fn relative_time(timestamp: i64) -> String {
    let now = now_secs();
    let delta = now.saturating_sub(timestamp).max(0);
    if delta < 60 {
        return "just now".to_string();
    }
    if delta < 3600 {
        return format!("{} min ago", delta / 60);
    }
    if delta < 86_400 {
        return format!("{} h ago", delta / 3600);
    }
    if delta < 7 * 86_400 {
        return format!("{} d ago", delta / 86_400);
    }
    glib::DateTime::from_unix_utc(timestamp)
        .ok()
        .and_then(|datetime| datetime.format("%x").ok())
        .map(|formatted| formatted.to_string())
        .unwrap_or_default()
}
