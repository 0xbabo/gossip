use std::cmp::Ordering;

use crate::{db::DbRelay, globals::GLOBALS, comms::ToOverlordMessage};

use super::{GossipUi, Page, widgets::RelayEntry};
use eframe::egui;
use egui::{Context, Ui};
use egui_winit::egui::{Id, vec2, Rect, RichText, TextBuffer};
use nostr_types::RelayUrl;

mod active;
mod mine;
mod known;

pub(super) struct RelayUi {
    /// text of search field
    search: String,
    /// how to sort relay entries
    sort: RelaySorting,
    /// which relays to include in the list
    filter: RelayFilter,
    /// to edit, add the relay url here
    edit: Option<RelayUrl>,
    /// did we just finish editing an entry, add it here
    edit_done: Option<RelayUrl>,
    /// do we still need to scroll to the edit
    edit_needs_scroll: bool,

    /// Add Relay dialog
    pub(super) add_dialog_active: bool,
    new_relay_url: String,
}

impl RelayUi {
    pub(super) fn new() -> Self {
        Self {
            search: String::new(),
            sort: RelaySorting::default(),
            filter: RelayFilter::default(),
            edit: None,
            edit_done: None,
            edit_needs_scroll: false,
            add_dialog_active: false,
            new_relay_url: "".to_string(),
        }
    }
}

#[derive(PartialEq,Default)]
pub(super) enum RelaySorting {
    #[default]
    Rank,
    WriteRelays,
    AdvertiseRelays,
    HighestFollowing,
    HighestSuccessRate,
    LowestSuccessRate,
}

impl RelaySorting {
    pub fn get_name(&self) -> &str {
        match self {
            RelaySorting::Rank => "Rank",
            RelaySorting::WriteRelays => "Write Relays",
            RelaySorting::AdvertiseRelays => "Advertise Relays",
            RelaySorting::HighestFollowing => "Following",
            RelaySorting::HighestSuccessRate => "Success Rate",
            RelaySorting::LowestSuccessRate => "Failure Rate",
        }
    }
}

#[derive(PartialEq, Default)]
pub(super) enum RelayFilter {
    #[default]
    All,
    Write,
    Read,
    Advertise,
    Private,
}

impl RelayFilter {
    pub fn get_name(&self) -> &str {
        match self {
            RelayFilter::All => "All",
            RelayFilter::Write => "Write",
            RelayFilter::Read => "Read",
            RelayFilter::Advertise => "Advertise",
            RelayFilter::Private => "Private",
        }
    }
}

///
/// Show the Relays UI
///
pub(super) fn update(app: &mut GossipUi, ctx: &Context, frame: &mut eframe::Frame, ui: &mut Ui) {
    if app.page == Page::RelaysActivityMonitor {
        active::update(app, ctx, frame, ui);
    } else if app.page == Page::RelaysMine {
        mine::update(app, ctx, frame, ui);
    } else if app.page == Page::RelaysKnownNetwork {
        known::update(app, ctx, frame, ui);
    }
}

pub(super) fn relay_scroll_list(app: &mut GossipUi, ui: &mut Ui, relays: Vec<DbRelay>, id_source: Id) {
    let scroll_size = ui.available_size_before_wrap();
    let is_editing = app.relays.edit.is_some();
    let enable_scroll = !is_editing && !egui::ScrollArea::is_scrolling(ui, id_source);

    egui::ScrollArea::vertical()
        .id_source(id_source)
        .enable_scrolling(enable_scroll)
        .show(ui, |ui| {
        let mut pos_last_entry = ui.cursor().left_top();
        let mut has_edit_target = false;

        for db_relay in relays {
            let db_url = db_relay.url.clone();
            // is THIS entry being edited?
            let edit = if let Some(edit_url) = &app.relays.edit {
                if edit_url == &db_url {
                    has_edit_target = true;
                    true
                } else {
                    false
                }
            } else {
                false
            };
            let enabled = edit || !is_editing;
            let mut widget = RelayEntry::new(db_relay, app);
            widget.set_edit(edit);
            widget.set_enabled(enabled);
            if let Some(ref assignment) = GLOBALS.relay_picker.get_relay_assignment(&db_url) {
                widget.set_user_count(assignment.pubkeys.len());
            }
            let response = ui.add_enabled(enabled, widget.clone());
            if response.clicked() {
                if !edit {
                    app.relays.edit = Some(db_url);
                    app.relays.edit_needs_scroll = true;
                    has_edit_target = true;
                } else {
                    app.relays.edit_done = Some(db_url);
                    app.relays.edit = None;
                }
            } else {
                if edit && has_edit_target && app.relays.edit_needs_scroll {
                    // on the start of an edit, scroll to the entry (after fixed sorting)
                    response.scroll_to_me(Some(egui::Align::Center));
                    app.relays.edit_needs_scroll = false;
                } else if Some(db_url) == app.relays.edit_done {
                    // on the end of an edit, scroll to the entry (after sorting has reverted)
                    response.scroll_to_me(Some(egui::Align::Center));
                    app.relays.edit_done = None;
                }
            }
            pos_last_entry = response.rect.left_top();
        }

        if !has_edit_target {
            // the relay we wanted to edit was not in the list anymore
            // -> release edit modal
            app.relays.edit = None;
        }

        // add enough space to show the last relay entry at the top when editing
        if app.relays.edit.is_some() {
            let desired_size = scroll_size - vec2(0.0, ui.cursor().top() - pos_last_entry.y);
            ui.allocate_exact_size(desired_size, egui::Sense::hover());
        }
    });
}

pub(super) fn entry_dialog(ctx: &Context, app: &mut GossipUi) {
    let dlg_size = vec2(ctx.screen_rect().width() * 0.66, 120.0);

    egui::Area::new("hide-background-area")
        .fixed_pos(ctx.screen_rect().left_top())
        .movable(false)
        .interactable(false)
        .order(egui::Order::Middle)
        .show(ctx, |ui| {
            ui.painter().rect_filled(
                ctx.screen_rect(),
                egui::Rounding::same(0.0),
                egui::Color32::from_rgba_unmultiplied(0x9f,0x9f,0x9f,102));
        });

    let id: Id = "relays-add-dialog".into();
    let mut frame = egui::Frame::popup(&ctx.style());
    let area = egui::Area::new(id)
        .movable(false)
        .interactable(true)
        .order(egui::Order::Foreground)
        .fixed_pos(ctx.screen_rect().center() - vec2(dlg_size.x/2.0, dlg_size.y));
    area.show_open_close_animation(ctx, &frame, app.relays.add_dialog_active);
    area.show(ctx, |ui| {
        frame.fill = ui.visuals().extreme_bg_color;
        frame.inner_margin = egui::Margin::symmetric(20.0, 10.0);
        frame.show(ui, |ui|{
            ui.set_min_size(dlg_size);
            ui.set_max_size(dlg_size);

            // ui.max_rect is inner_margin size
            let tr = ui.max_rect().right_top();

            ui.vertical(|ui|{
                ui.horizontal(|ui|{
                    ui.heading("Add a new relay");
                    let rect = Rect::from_x_y_ranges(tr.x ..= tr.x + 10.0 , tr.y - 20.0 ..= tr.y - 10.0 );
                    ui.allocate_ui_at_rect(
                        rect,
                        |ui|{
                            if ui.add_sized(
                                rect.size(),
                                super::widgets::NavItem::new("\u{274C}", false)).clicked() {
                                app.relays.add_dialog_active = false;
                            }
                        });
                });
                ui.add_space(10.0);
                ui.add(egui::Label::new("Enter relay URL:"));
                ui.add_space(10.0);
                let edit_response = ui.horizontal(|ui|{
                    ui.style_mut().visuals.widgets.inactive.bg_stroke.width = 1.0;
                    ui.style_mut().visuals.widgets.hovered.bg_stroke.width = 1.0;
                    ui.add(
                        text_edit_line!(app, app.relays.new_relay_url).desired_width(ui.available_width())
                        .hint_text("wss://myrelay.com") )
                });

                ui.add_space(10.0);
                ui.allocate_ui_with_layout( vec2(edit_response.inner.rect.width(), 30.0), egui::Layout::left_to_right(egui::Align::Min), |ui|{
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui|{
                        ui.visuals_mut().widgets.inactive.weak_bg_fill = app.settings.theme.accent_color();
                        ui.visuals_mut().widgets.hovered.weak_bg_fill = {
                            let mut hsva: egui::ecolor::HsvaGamma  = app.settings.theme.accent_color().into();
                            hsva.v *= 0.8;
                            hsva.into()
                        };
                        ui.spacing_mut().button_padding *= 2.0;
                        let text = RichText::new("Check & Configure").color(ui.visuals().extreme_bg_color);
                        if ui.add(egui::Button::new(text)).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            if let Ok(url) = RelayUrl::try_from_str(&app.relays.new_relay_url) {
                                let _ = GLOBALS.to_overlord.send(ToOverlordMessage::AddRelay(url.clone()));
                                GLOBALS.status_queue.write().write( format!(
                                    "I asked the overlord to add relay {}. Check for it below.",
                                    &app.relays.new_relay_url
                                ).to_owned());

                                // send user to known relays page (where the new entry should show up)
                                app.set_page( Page::RelaysKnownNetwork );
                                // set the new relay to edit mode
                                app.relays.edit = Some(url);
                                // search for the new relay so it shows at the top
                                app.relays.search = app.relays.new_relay_url.take();
                                // reset the filters so it will show
                                app.relays.filter = RelayFilter::All;

                                // close this dialog
                                app.relays.add_dialog_active = false;
                                app.relays.new_relay_url = "".to_owned();
                            } else {
                                GLOBALS.status_queue.write().write(
                                    "That's not a valid relay URL.".to_owned()
                                );
                            }
                        }
                    });
                });
            });

        });
    });
}

///
/// Draw relay sort comboBox
///
pub(super) fn relay_sort_combo(app: &mut GossipUi, ui: &mut Ui) {
    let sort_combo = egui::ComboBox::from_id_source(Id::from("RelaySortCombo"));
    sort_combo
        .width(130.0)
        .selected_text("Sort by ".to_string() + app.relays.sort.get_name())
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut app.relays.sort,
                RelaySorting::Rank,
                RelaySorting::Rank.get_name(),
            );
            ui.selectable_value(
                &mut app.relays.sort,
                RelaySorting::HighestFollowing,
                RelaySorting::HighestFollowing.get_name(),
            );
            ui.selectable_value(
                &mut app.relays.sort,
                RelaySorting::HighestSuccessRate,
                RelaySorting::HighestSuccessRate.get_name(),
            );
            ui.selectable_value(
                &mut app.relays.sort,
                RelaySorting::LowestSuccessRate,
                RelaySorting::LowestSuccessRate.get_name(),
            );
            ui.selectable_value(
                &mut app.relays.sort,
                RelaySorting::WriteRelays,
                RelaySorting::WriteRelays.get_name(),
            );
            ui.selectable_value(
                &mut app.relays.sort,
                RelaySorting::AdvertiseRelays,
                RelaySorting::AdvertiseRelays.get_name(),
            );
        });
}

///
/// Draw relay filter comboBox
///
pub(super) fn relay_filter_combo(app: &mut GossipUi, ui: &mut Ui) {
    let filter_combo = egui::ComboBox::from_id_source(Id::from("RelayFilterCombo"));
    filter_combo
        .selected_text(app.relays.filter.get_name())
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut app.relays.filter,
                RelayFilter::All,
                RelayFilter::All.get_name(),
            );
            ui.selectable_value(
                &mut app.relays.filter,
                RelayFilter::Write,
                RelayFilter::Write.get_name(),
            );
            ui.selectable_value(
                &mut app.relays.filter,
                RelayFilter::Read,
                RelayFilter::Read.get_name(),
            );
            ui.selectable_value(
                &mut app.relays.filter,
                RelayFilter::Advertise,
                RelayFilter::Advertise.get_name(),
            );
            ui.selectable_value(
                &mut app.relays.filter,
                RelayFilter::Private,
                RelayFilter::Private.get_name(),
            );
        });
}

///
/// Filter a relay entry
/// - return: true if selected
///
pub(super) fn sort_relay(rui: &RelayUi, a: &DbRelay, b: &DbRelay) -> Ordering {
    match rui.sort {
        RelaySorting::Rank => b
            .rank.cmp(&a.rank)
            .then(b.usage_bits.cmp(&a.usage_bits))
            .then(a.url.cmp(&b.url)),
        RelaySorting::WriteRelays => b
            .has_usage_bits(DbRelay::WRITE)
            .cmp(&a.has_usage_bits(DbRelay::WRITE))
            .then(a.url.cmp(&b.url)),
        RelaySorting::AdvertiseRelays => b
            .has_usage_bits(DbRelay::ADVERTISE)
            .cmp(&a.has_usage_bits(DbRelay::ADVERTISE))
            .then(a.url.cmp(&b.url)),
        RelaySorting::HighestFollowing => a.url.cmp(&b.url), // FIXME need following numbers here
        RelaySorting::HighestSuccessRate => b
            .success_rate()
            .total_cmp(&a.success_rate())
            .then(a.url.cmp(&b.url)),
        RelaySorting::LowestSuccessRate => a
            .success_rate()
            .total_cmp(&b.success_rate())
            .then(a.url.cmp(&b.url)),
    }
}

///
/// Filter a relay entry
/// - return: true if selected
///
pub(super) fn filter_relay(rui: &RelayUi, ri: &DbRelay) -> bool {
    let search = if rui.search.len() > 1 {
        ri.url
            .as_str()
            .to_lowercase()
            .contains(&rui.search.to_lowercase())
    } else {
        true
    };

    let filter = match rui.filter {
        RelayFilter::All => true,
        RelayFilter::Write => ri.has_usage_bits(DbRelay::WRITE),
        RelayFilter::Read => ri.has_usage_bits(DbRelay::READ),
        RelayFilter::Advertise => ri.has_usage_bits(DbRelay::ADVERTISE),
        RelayFilter::Private => !ri.has_usage_bits(DbRelay::INBOX | DbRelay::OUTBOX),
    };

    search && filter
}
