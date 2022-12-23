mod about;
mod feed;
mod people;
mod relays;
mod settings;
mod stats;
mod style;
mod you;

use crate::about::About;
use crate::error::Error;
use crate::globals::GLOBALS;
use crate::settings::Settings;
use eframe::{egui, IconData, Theme};
use egui::{ColorImage, Context, ImageData, TextureHandle, TextureOptions};

pub fn run() -> Result<(), Error> {
    let icon_bytes = include_bytes!("../../gossip.png");
    let icon = image::load_from_memory(icon_bytes)?.to_rgba8();
    let (icon_width, icon_height) = icon.dimensions();

    let options = eframe::NativeOptions {
        decorated: true,
        drag_and_drop_support: true,
        default_theme: Theme::Light,
        icon_data: Some(IconData {
            rgba: icon.into_raw(),
            width: icon_width,
            height: icon_height,
        }),
        initial_window_size: Some(egui::vec2(700.0, 900.0)),
        resizable: true,
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "gossip",
        options,
        Box::new(|cc| Box::new(GossipUi::new(cc))),
    );

    Ok(())
}

#[derive(PartialEq)]
enum Page {
    Feed,
    People,
    You,
    Relays,
    Settings,
    Stats,
    About,
}

struct GossipUi {
    page: Page,
    about: About,
    icon: TextureHandle,
    placeholder_avatar: TextureHandle,
    draft: String,
    settings: Settings,
}

impl GossipUi {
    fn new(cctx: &eframe::CreationContext<'_>) -> Self {
        if cctx.egui_ctx.style().visuals.dark_mode {
            cctx.egui_ctx.set_visuals(style::dark_mode_visuals());
        } else {
            cctx.egui_ctx.set_visuals(style::light_mode_visuals());
        };

        cctx.egui_ctx.set_fonts(style::font_definitions());

        let mut style: egui::Style = (*cctx.egui_ctx.style()).clone();
        style.text_styles = style::text_styles();
        cctx.egui_ctx.set_style(style);

        let icon_texture_handle = {
            let bytes = include_bytes!("../../gossip.png");
            let image = image::load_from_memory(bytes).unwrap();
            let size = [image.width() as _, image.height() as _];
            let image_buffer = image.to_rgba8();
            let pixels = image_buffer.as_flat_samples();
            cctx.egui_ctx.load_texture(
                "icon",
                ImageData::Color(ColorImage::from_rgba_unmultiplied(size, pixels.as_slice())),
                TextureOptions::default(), // magnification, minification
            )
        };

        let placeholder_avatar_texture_handle = {
            let bytes = include_bytes!("../../placeholder_avatar.png");
            let image = image::load_from_memory(bytes).unwrap();
            let size = [image.width() as _, image.height() as _];
            let image_buffer = image.to_rgba8();
            let pixels = image_buffer.as_flat_samples();
            cctx.egui_ctx.load_texture(
                "placeholder_avatar",
                ImageData::Color(ColorImage::from_rgba_unmultiplied(size, pixels.as_slice())),
                TextureOptions::default(), // magnification, minification
            )
        };

        let settings = GLOBALS.settings.blocking_lock().clone();

        GossipUi {
            page: Page::Feed,
            about: crate::about::about(),
            icon: icon_texture_handle,
            placeholder_avatar: placeholder_avatar_texture_handle,
            draft: "".to_owned(),
            settings,
        }
    }
}

impl eframe::App for GossipUi {
    fn update(&mut self, ctx: &Context, frame: &mut eframe::Frame) {
        let darkmode: bool = ctx.style().visuals.dark_mode;

        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.page, Page::Feed, "Feed");
                ui.separator();
                ui.selectable_value(&mut self.page, Page::People, "People");
                ui.separator();
                ui.selectable_value(&mut self.page, Page::You, "You");
                ui.separator();
                ui.selectable_value(&mut self.page, Page::Relays, "Relays");
                ui.separator();
                ui.selectable_value(&mut self.page, Page::Settings, "Settings");
                ui.separator();
                ui.selectable_value(&mut self.page, Page::Stats, "Stats");
                ui.separator();
                ui.selectable_value(&mut self.page, Page::About, "About");
                ui.separator();
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.page {
            Page::Feed => feed::update(self, ctx, frame, ui),
            Page::People => people::update(self, ctx, frame, ui),
            Page::You => you::update(self, ctx, frame, ui),
            Page::Relays => relays::update(self, ctx, frame, ui),
            Page::Settings => settings::update(self, ctx, frame, ui, darkmode),
            Page::Stats => stats::update(self, ctx, frame, ui),
            Page::About => about::update(self, ctx, frame, ui),
        });
    }
}
