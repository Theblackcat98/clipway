//! The clipboard popup: a search entry over a `GtkListView`.
//!
//! Rows are bound lazily from a `gio::ListStore`, and listing history never
//! loads payloads (previews and thumbnails are precomputed in the store), so
//! opening and filtering stay cheap however large the history grows.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;

use crate::dbus::{Bridge, now_millis};
use crate::gui;
use crate::model::{EntryMeta, Kind};

const LIST_LIMIT: u32 = 200;
const SEARCH_DEBOUNCE_MS: u64 = 80;

thread_local! {
    static ACTIVE: RefCell<Option<Popup>> = const { RefCell::new(None) };
}

pub fn show(app: &adw::Application, bridge: Arc<Bridge>) {
    let existing = ACTIVE.with(|slot| slot.borrow().as_ref().map(|popup| popup.window.clone()));
    if existing.is_some() {
        ACTIVE.with(|slot| {
            if let Some(popup) = slot.borrow().as_ref() {
                popup.present();
            }
        });
        return;
    }
    let popup = Popup::new(app, bridge);
    popup.present();
    ACTIVE.with(|slot| *slot.borrow_mut() = Some(popup));
}

/// Refreshes the popup if it is on screen. Safe to call at any time on the
/// main thread.
pub fn refresh_if_visible() {
    ACTIVE.with(|slot| {
        if let Some(popup) = slot.borrow().as_ref()
            && popup.window.is_visible()
        {
            popup.refresh();
        }
    });
}

pub fn toast(message: &str) {
    ACTIVE.with(|slot| {
        if let Some(popup) = slot.borrow().as_ref() {
            popup.toast.add_toast(adw::Toast::new(message));
        }
    });
}

struct Popup {
    window: adw::ApplicationWindow,
    search: gtk::SearchEntry,
    view: gtk::ListView,
    selection: gtk::SingleSelection,
    model: gio::ListStore,
    status: gtk::Label,
    banner: adw::Banner,
    toast: adw::ToastOverlay,
    bridge: Arc<Bridge>,
}

impl Popup {
    fn new(app: &adw::Application, bridge: Arc<Bridge>) -> Self {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Clipboard")
            .default_width(680)
            .default_height(520)
            .build();

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new("Clipboard", "Clipway")));
        let clear_button = gtk::Button::from_icon_name("user-trash-symbolic");
        clear_button.set_tooltip_text(Some("Clear history…"));
        header.pack_start(&clear_button);
        toolbar.add_top_bar(&header);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let banner =
            adw::Banner::new("Incognito mode is on. New clipboard content is not captured.");
        content.append(&banner);

        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search clipboard history"));
        search.set_hexpand(true);
        search.set_margin_top(12);
        search.set_margin_bottom(6);
        search.set_margin_start(12);
        search.set_margin_end(12);
        // Typing anywhere in the window goes to the search entry.
        search.set_key_capture_widget(Some(&window));
        content.append(&search);

        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(true);
        let view = gtk::ListView::new(Some(selection.clone()), Some(row_factory(&bridge)));
        view.set_single_click_activate(false);
        view.add_css_class("navigation-sidebar");

        let scroller = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .child(&view)
            .build();
        scroller.set_margin_start(6);
        scroller.set_margin_end(6);
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

        let popup = Self {
            window: window.clone(),
            search: search.clone(),
            view: view.clone(),
            selection: selection.clone(),
            model,
            status,
            banner,
            toast,
            bridge: bridge.clone(),
        };

        let debounce = std::rc::Rc::new(std::cell::Cell::new(None::<glib::SourceId>));
        search.connect_search_changed(move |_| {
            if let Some(id) = debounce.replace(None) {
                id.remove();
            }
            let cell = debounce.clone();
            debounce.set(Some(glib::timeout_add_local(
                Duration::from_millis(SEARCH_DEBOUNCE_MS),
                move || {
                    cell.set(None);
                    with_active(Popup::refresh);
                    glib::ControlFlow::Break
                },
            )));
        });

        // Enter in the search entry pastes the highlighted row.
        search.connect_activate(|_| with_active(Popup::paste_selected));
        // Enter or double-click on a row pastes that row.
        view.connect_activate(|_, position| {
            with_active(|popup| {
                popup.selection.set_selected(position);
                popup.paste_selected();
            })
        });

        {
            let keys = gtk::EventControllerKey::new();
            keys.connect_key_pressed(|_, keyval, _, _| {
                use gtk::gdk::Key;
                match keyval {
                    Key::Down => with_active(|popup| popup.move_selection(1)),
                    Key::Up => with_active(|popup| popup.move_selection(-1)),
                    Key::Page_Down => with_active(|popup| popup.move_selection(10)),
                    Key::Page_Up => with_active(|popup| popup.move_selection(-10)),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            });
            search.add_controller(keys);
        }

        {
            let keys = gtk::EventControllerKey::new();
            keys.set_propagation_phase(gtk::PropagationPhase::Capture);
            let window_for_keys = window.clone();
            keys.connect_key_pressed(move |_, keyval, _, state| {
                use gtk::gdk::Key;
                let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
                match keyval {
                    Key::Escape => {
                        window_for_keys.set_visible(false);
                        glib::Propagation::Stop
                    }
                    // Delete removes the highlighted entry, except while the
                    // user is editing a non-empty search (forward delete).
                    Key::Delete if !search_is_being_edited(&window_for_keys) => {
                        with_active(Popup::delete_selected);
                        glib::Propagation::Stop
                    }
                    Key::p | Key::P if ctrl => {
                        with_active(Popup::toggle_pin_selected);
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
            window.add_controller(keys);
        }

        {
            let window = window.clone();
            clear_button.connect_clicked(move |_| {
                gui::confirm_clear(Some(window.upcast_ref()), &bridge, || {});
            });
        }

        window.connect_close_request(|window| {
            window.set_visible(false);
            glib::Propagation::Stop
        });

        popup
    }

    fn present(&self) {
        self.search.set_text("");
        self.refresh();
        self.window.present();
        self.search.grab_focus();
    }

    fn refresh(&self) {
        self.banner.set_revealed(self.bridge.snapshot().incognito);
        let query = self.search.text().to_string();
        let entries = if query.trim().is_empty() {
            self.bridge.recent(LIST_LIMIT)
        } else {
            self.bridge.search(query.trim(), LIST_LIMIT)
        }
        .unwrap_or_default();

        let items: Vec<glib::BoxedAnyObject> = entries
            .iter()
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect();
        self.model.splice(0, self.model.n_items(), &items);
        if !items.is_empty() {
            self.selection.set_selected(0);
            self.view.scroll_to(0, gtk::ListScrollFlags::NONE, None);
        }
        let noun = if entries.len() == 1 {
            "entry"
        } else {
            "entries"
        };
        self.status.set_text(&format!("{} {noun}", entries.len()));
    }

    fn move_selection(&self, delta: i64) {
        let count = i64::from(self.model.n_items());
        if count == 0 {
            return;
        }
        let current = i64::from(self.selection.selected()).min(count - 1);
        let next = (current + delta).clamp(0, count - 1) as u32;
        self.selection.set_selected(next);
        self.view.scroll_to(next, gtk::ListScrollFlags::NONE, None);
    }

    fn selected(&self) -> Option<EntryMeta> {
        self.selection
            .selected_item()
            .and_downcast::<glib::BoxedAnyObject>()
            .map(|item| item.borrow::<EntryMeta>().clone())
    }

    fn paste_selected(&self) {
        let Some(meta) = self.selected() else {
            return;
        };
        let bridge = self.bridge.clone();
        let window = self.window.downgrade();
        glib::spawn_future_local(async move {
            // The window must still have focus while the extension sets the
            // clipboard: it only accepts requests from the focused app.
            match bridge.paste(meta.id).await {
                Ok(()) => {
                    if let Some(window) = window.upgrade() {
                        window.set_visible(false);
                    }
                }
                Err(error) => toast(&format!("Could not set the clipboard: {error}")),
            }
        });
    }

    fn delete_selected(&self) {
        let Some(meta) = self.selected() else {
            return;
        };
        if self.bridge.delete(meta.id).unwrap_or(false) {
            self.toast.add_toast(adw::Toast::new("Entry deleted"));
            self.refresh();
        }
    }

    fn toggle_pin_selected(&self) {
        let Some(meta) = self.selected() else {
            return;
        };
        let position = self.selection.selected();
        let _ = self.bridge.set_pinned(meta.id, !meta.pinned);
        self.refresh();
        let count = self.model.n_items();
        if count > 0 {
            self.selection.set_selected(position.min(count - 1));
        }
    }
}

fn with_active(action: impl FnOnce(&Popup)) {
    ACTIVE.with(|slot| {
        if let Some(popup) = slot.borrow().as_ref() {
            action(popup);
        }
    });
}

fn search_is_being_edited(window: &adw::ApplicationWindow) -> bool {
    ACTIVE.with(|slot| {
        let slot = slot.borrow();
        let Some(popup) = slot.as_ref() else {
            return false;
        };
        let focus_in_search = gtk::prelude::GtkWindowExt::focus(window)
            .is_some_and(|focus| focus.is_ancestor(&popup.search) || focus == popup.search);
        focus_in_search && !popup.search.text().is_empty()
    })
}

/// Builds rows once (setup) and fills them per item (bind). Labels are plain
/// text: clipboard content is never parsed as Pango markup.
fn row_factory(bridge: &Arc<Bridge>) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let bridge = bridge.clone();
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.set_margin_start(6);
        row.set_margin_end(6);

        let icon = gtk::Image::new();
        icon.set_pixel_size(20);
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Cover);
        picture.set_size_request(32, 32);
        row.append(&icon);
        row.append(&picture);

        let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
        texts.set_hexpand(true);
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_single_line_mode(true);
        let subtitle = gtk::Label::new(None);
        subtitle.set_xalign(0.0);
        subtitle.add_css_class("dim-label");
        subtitle.add_css_class("caption");
        texts.append(&title);
        texts.append(&subtitle);
        row.append(&texts);

        let pin = gtk::ToggleButton::new();
        pin.add_css_class("flat");
        pin.set_valign(gtk::Align::Center);
        pin.set_tooltip_text(Some("Pin (Ctrl+P)"));
        let bridge = bridge.clone();
        let item_ref = item.downgrade();
        pin.connect_clicked(move |button| {
            let Some(meta) = item_ref
                .upgrade()
                .and_then(|item| item.item())
                .and_downcast::<glib::BoxedAnyObject>()
                .map(|object| object.borrow::<EntryMeta>().clone())
            else {
                return;
            };
            let _ = bridge.set_pinned(meta.id, button.is_active());
            refresh_if_visible();
        });
        row.append(&pin);

        item.set_child(Some(&row));
    });
    factory.connect_bind(|_, object| {
        let Some(item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let (Some(row), Some(object)) = (
            item.child(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        let meta = object.borrow::<EntryMeta>();
        let mut child = row.first_child();
        let mut next = || {
            let current = child.clone();
            child = current.as_ref().and_then(|widget| widget.next_sibling());
            current
        };
        let (Some(icon), Some(picture), Some(texts), Some(pin)) = (next(), next(), next(), next())
        else {
            return;
        };
        let icon = icon.downcast::<gtk::Image>().ok();
        let picture = picture.downcast::<gtk::Picture>().ok();
        let pin = pin.downcast::<gtk::ToggleButton>().ok();
        let title = texts.first_child().and_downcast::<gtk::Label>();
        let subtitle = texts.last_child().and_downcast::<gtk::Label>();

        let thumbnail = meta
            .thumb
            .as_ref()
            .filter(|_| meta.kind == Kind::Image)
            .and_then(|png| gtk::gdk::Texture::from_bytes(&glib::Bytes::from(png)).ok());
        if let (Some(icon), Some(picture)) = (icon, picture) {
            icon.set_icon_name(Some(meta.kind.icon_name()));
            icon.set_visible(thumbnail.is_none());
            picture.set_paintable(thumbnail.as_ref());
            picture.set_visible(thumbnail.is_some());
        }
        if let Some(title) = title {
            title.set_text(&meta.preview);
        }
        if let Some(subtitle) = subtitle {
            let time = relative_time(meta.ts);
            if meta.source.is_empty() {
                subtitle.set_text(&time);
            } else {
                subtitle.set_text(&format!("{} · {time}", meta.source));
            }
        }
        if let Some(pin) = pin {
            pin.set_active(meta.pinned);
            pin.set_icon_name(if meta.pinned {
                "starred-symbolic"
            } else {
                "non-starred-symbolic"
            });
        }
    });
    factory
}

pub fn schedule_screenshot(app: adw::Application, path: String) {
    glib::timeout_add_local(Duration::from_millis(800), move || {
        let saved = ACTIVE.with(|slot| {
            slot.borrow()
                .as_ref()
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

/// `timestamp` is in milliseconds.
fn relative_time(timestamp: i64) -> String {
    let delta = now_millis().saturating_sub(timestamp).max(0) / 1000;
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
    glib::DateTime::from_unix_utc(timestamp / 1000)
        .ok()
        .and_then(|datetime| datetime.format("%x").ok())
        .map(|formatted| formatted.to_string())
        .unwrap_or_default()
}
