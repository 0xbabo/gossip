use super::shatter::{ContentSegment, Span};
use super::{GossipUi, NoteData, Page, RepostType};
use crate::feed::FeedKind;
use crate::globals::GLOBALS;
use eframe::egui::Context;
use eframe::{
    egui::{self, Image, Response},
    epaint::Vec2,
};
use egui::{RichText, Ui};
use nostr_types::{Id, IdHex, NostrBech32, NostrUrl, PublicKeyHex, Tag, Url};
use std::{
    cell::{Ref, RefCell},
    rc::Rc,
};

pub(super) fn render_content(
    app: &mut GossipUi,
    ui: &mut Ui,
    ctx: &Context,
    note_ref: Rc<RefCell<NoteData>>,
    as_deleted: bool,
    content_margin_left: f32,
    bottom_of_avatar: f32,
) {
    ui.style_mut().spacing.item_spacing.x = 0.0;

    if let Ok(note) = note_ref.try_borrow() {
        for segment in note.shattered_content.segments.iter() {
            match segment {
                ContentSegment::NostrUrl(nurl) => render_nostr_url(app, ui, &note, nurl),
                ContentSegment::TagReference(num) => {
                    if let Some(tag) = note.event.tags.get(*num) {
                        match tag {
                            Tag::Pubkey { pubkey, .. } => {
                                render_profile_link(app, ui, pubkey);
                            }
                            Tag::Event { id, .. } => {
                                let mut render_link = true;
                                if app.settings.show_mentions {
                                    match note.repost {
                                        Some(RepostType::MentionOnly)
                                        | Some(RepostType::CommentMention)
                                        | Some(RepostType::Kind6Mention) => {
                                            for (i, cached_id) in note.mentions.iter() {
                                                if *i == *num {
                                                    if let Some(note_data) =
                                                        app.notes.try_update_and_get(cached_id)
                                                    {
                                                        // TODO block additional repost recursion
                                                        super::render_repost(
                                                            app,
                                                            ui,
                                                            ctx,
                                                            &note.repost,
                                                            note_data,
                                                            content_margin_left,
                                                            bottom_of_avatar,
                                                        );
                                                        render_link = false;
                                                    }
                                                }
                                            }
                                        }
                                        _ => (),
                                    }
                                }
                                if render_link {
                                    render_event_link(app, ui, note.event.id, *id);
                                }
                            }
                            Tag::Hashtag(s) => {
                                render_hashtag(ui, s);
                            }
                            _ => {
                                render_unknown_reference(ui, *num);
                            }
                        }
                    }
                }
                ContentSegment::Hyperlink(linkspan) => render_hyperlink(app, ui, &note, linkspan),
                ContentSegment::Plain(textspan) => render_plain(ui, &note, textspan, as_deleted),
            }
        }
    }

    ui.reset_style();
}

pub(super) fn render_nostr_url(
    app: &mut GossipUi,
    ui: &mut Ui,
    note: &Ref<NoteData>,
    nurl: &NostrUrl,
) {
    match &nurl.0 {
        NostrBech32::Pubkey(pk) => {
            render_profile_link(app, ui, &(*pk).into());
        }
        NostrBech32::Profile(prof) => {
            render_profile_link(app, ui, &prof.pubkey.into());
        }
        NostrBech32::Id(id) => {
            render_event_link(app, ui, note.event.id, *id);
        }
        NostrBech32::EventPointer(ep) => {
            render_event_link(app, ui, note.event.id, ep.id);
        }
    }
}

pub(super) fn render_hyperlink(
    app: &mut GossipUi,
    ui: &mut Ui,
    note: &Ref<NoteData>,
    linkspan: &Span,
) {
    let link = note.shattered_content.slice(linkspan).unwrap();
    if let Some(image_url) = as_image_url(app, link) {
        show_image_toggle(app, ui, image_url);
    //} else if is_video_url(&lowercase) {
    // TODO
    //    crate::ui::widgets::break_anywhere_hyperlink_to(ui, link, link);
    } else {
        crate::ui::widgets::break_anywhere_hyperlink_to(ui, link, link);
    }
}

pub(super) fn render_plain(ui: &mut Ui, note: &Ref<NoteData>, textspan: &Span, as_deleted: bool) {
    let text = note.shattered_content.slice(textspan).unwrap();
    if as_deleted {
        ui.label(RichText::new(text).strikethrough());
    } else {
        ui.label(text);
    }
}

pub(super) fn render_profile_link(app: &mut GossipUi, ui: &mut Ui, pubkey: &PublicKeyHex) {
    let nam = GossipUi::display_name_from_pubkeyhex_lookup(pubkey);
    let nam = format!("@{}", nam);
    if ui.link(&nam).clicked() {
        app.set_page(Page::Person(pubkey.to_owned()));
    };
}

pub(super) fn render_event_link(
    app: &mut GossipUi,
    ui: &mut Ui,
    referenced_by_id: Id,
    link_to_id: Id,
) {
    let idhex: IdHex = link_to_id.into();
    let nam = format!("#{}", GossipUi::hex_id_short(&idhex));
    if ui.link(&nam).clicked() {
        app.set_page(Page::Feed(FeedKind::Thread {
            id: link_to_id,
            referenced_by: referenced_by_id,
        }));
    };
}

pub(super) fn render_hashtag(ui: &mut Ui, s: &String) {
    if ui.link(format!("#{}", s)).clicked() {
        *GLOBALS.status_message.blocking_write() =
            "Gossip doesn't have a hashtag feed yet.".to_owned();
    }
}

pub(super) fn render_unknown_reference(ui: &mut Ui, num: usize) {
    if ui.link(format!("#[{}]", num)).clicked() {
        *GLOBALS.status_message.blocking_write() =
            "Gossip can't handle this kind of tag link yet.".to_owned();
    }
}

fn is_image_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".png")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
}

fn as_image_url(app: &mut GossipUi, url: &str) -> Option<Url> {
    if is_image_url(url) {
        app.try_check_url(url)
    } else {
        None
    }
}

/*
fn is_video_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.ends_with(".mov")
        || lower.ends_with(".mp4")
        || lower.ends_with(".mkv")
        || lower.ends_with(".webm")
}
 */

fn show_image_toggle(app: &mut GossipUi, ui: &mut Ui, url: Url) {
    let row_height = ui.cursor().height();
    let url_string = url.to_string();
    let mut show_link = true;

    // FIXME show/hide lists should persist app restarts
    let show_image = (app.settings.show_media && !app.media_hide_list.contains(&url))
        || (!app.settings.show_media && app.media_show_list.contains(&url));

    if show_image {
        if let Some(response) = try_render_media(app, ui, url.clone()) {
            show_link = false;

            // full-width toggle
            if response.clicked() {
                if app.media_full_width_list.contains(&url) {
                    app.media_full_width_list.remove(&url);
                } else {
                    app.media_full_width_list.insert(url.clone());
                }
            }
        }
    }

    if show_link {
        let response = ui.link("[ Image ]");
        // show url on hover
        response.clone().on_hover_text(url_string.clone());
        // show media toggle
        if response.clicked() {
            if app.settings.show_media {
                app.media_hide_list.remove(&url);
            } else {
                app.media_show_list.insert(url.clone());
            }
        }
        // context menu
        response.context_menu(|ui| {
            if ui.button("Open in browser").clicked() {
                let modifiers = ui.ctx().input(|i| i.modifiers);
                ui.ctx().output_mut(|o| {
                    o.open_url = Some(egui::output::OpenUrl {
                        url: url_string.clone(),
                        new_tab: modifiers.any(),
                    });
                });
            }
            if ui.button("Copy URL").clicked() {
                ui.output_mut(|o| o.copied_text = url_string.clone());
            }
            if app.has_media_loading_failed(url_string.as_str())
                && ui.button("Retry loading ...").clicked()
            {
                app.retry_media(&url);
            }
        });
    }

    ui.end_row();

    // workaround for egui bug where image enlarges the cursor height
    ui.set_row_height(row_height);
}

/// Try to fetch and render a piece of media
///  - return: true if successfully rendered, false otherwise
fn try_render_media(app: &mut GossipUi, ui: &mut Ui, url: Url) -> Option<Response> {
    let mut response_return = None;
    if let Some(media) = app.try_get_media(ui.ctx(), url.clone()) {
        let ui_max = if app.media_full_width_list.contains(&url) {
            Vec2::new(
                ui.available_width() * 0.9,
                ui.ctx().screen_rect().height() * 0.9,
            )
        } else {
            Vec2::new(
                ui.available_width() / 2.0,
                ui.ctx().screen_rect().height() / 3.0,
            )
        };
        let msize = media.size_vec2();
        let aspect = media.aspect_ratio();

        // insert a newline if the current line has text
        if ui.cursor().min.x > ui.max_rect().min.x {
            ui.end_row();
        }

        // determine maximum x and y sizes
        let max_x = if ui_max.x > msize.x {
            msize.x
        } else {
            ui_max.x
        };
        let max_y = if ui_max.y > msize.y {
            msize.y
        } else {
            ui_max.y
        };

        // now determine if we are constrained by x or by y and
        // calculate the resulting size
        let mut size = Vec2::new(0.0, 0.0);
        size.x = if max_x > max_y * aspect {
            max_y * aspect
        } else {
            max_x
        };
        size.y = if max_y > max_x / aspect {
            max_x / aspect
        } else {
            max_y
        };

        // render the image with a nice frame around it
        egui::Frame::none()
            .inner_margin(egui::Margin::same(0.0))
            .outer_margin(egui::Margin {
                top: 10.0,
                left: 0.0,
                right: 0.0,
                bottom: 10.0,
            })
            .fill(egui::Color32::TRANSPARENT)
            .rounding(ui.style().noninteractive().rounding)
            .show(ui, |ui| {
                let response = ui.add(Image::new(&media, size).sense(egui::Sense::click()));
                if response.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                // image button menu to the right of the image
                static BTN_SIZE: Vec2 = Vec2 { x: 20.0, y: 20.0 };
                static TXT_SIZE: f32 = 9.0;
                static SPACE: f32 = 10.0;
                let extend_area = egui::Rect {
                    min: response.rect.right_top(),
                    max: response.rect.right_bottom() + egui::Vec2::new(BTN_SIZE.x, 0.0),
                };
                let extend_area = extend_area.expand(SPACE * 2.0);
                if let Some(pointer_pos) = ui.ctx().pointer_latest_pos() {
                    if extend_area.contains(pointer_pos) {
                        ui.add_space(SPACE);
                        ui.vertical(|ui| {
                            ui.add_space(SPACE);
                            if ui
                                .add_sized(
                                    BTN_SIZE,
                                    egui::Button::new(RichText::new("\u{274C}").size(TXT_SIZE)),
                                )
                                .clicked()
                            {
                                if app.settings.show_media {
                                    app.media_hide_list.insert(url.clone());
                                } else {
                                    app.media_show_list.remove(&url);
                                }
                            }
                            ui.add_space(SPACE);
                            if ui
                                .add_sized(
                                    BTN_SIZE,
                                    egui::Button::new(RichText::new("\u{1F310}").size(TXT_SIZE)),
                                )
                                .clicked()
                            {
                                let modifiers = ui.ctx().input(|i| i.modifiers);
                                ui.ctx().output_mut(|o| {
                                    o.open_url = Some(egui::output::OpenUrl {
                                        url: url.to_string(),
                                        new_tab: modifiers.any(),
                                    });
                                });
                            }
                            ui.add_space(SPACE);
                            if ui
                                .add_sized(
                                    BTN_SIZE,
                                    egui::Button::new(RichText::new("\u{1F4CB}").size(TXT_SIZE)),
                                )
                                .clicked()
                            {
                                ui.output_mut(|o| o.copied_text = url.to_string());
                            }
                        });
                    }
                }
                response_return = Some(response);
            });
    };
    response_return
}
