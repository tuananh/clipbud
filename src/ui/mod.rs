use eframe::egui;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, hotkey::HotKey};
use mouse_position::mouse_position::Mouse;
use std::{str::FromStr, sync::mpsc};
use tray_icon::menu::MenuEvent;

use crate::ai::{ButtonsWrap, Config};
use crate::{ai, clipboard};

pub(crate) mod dialogs;
mod spinner;
mod tray;

const DEFAULT_WINDOW_SIZE: egui::Vec2 = egui::vec2(400.0, 200.0);
const DEFAULT_WINDOW_OFFSET: f32 = 10.0;
const DEFAULT_MAX_TEXTAREA_HEIGHT: f32 = 130.0;
pub(crate) struct UI {
    config: Config,

    window_visible: bool,
    window_size: egui::Vec2,
    window_position: egui::Pos2,
    monitor_size: egui::Vec2,

    clipboard_text: Option<String>,

    is_loading: bool,
    loading_start_time: std::time::Instant,
    current_action_label: String,

    // modal state
    show_error_modal: bool,
    error_message: String,

    clipboard_rx: mpsc::Receiver<clipboard::Event>,
    action_response_rx: mpsc::Receiver<ai::ActionEvent>,
    action_response_tx: mpsc::Sender<ai::ActionEvent>,
    tray: tray::Tray,
    _hotkey_manager: Option<GlobalHotKeyManager>,
}

impl UI {
    fn new(
        creation_context: &eframe::CreationContext<'_>,
        clipboard_rx: mpsc::Receiver<clipboard::Event>,
        config: Config,
    ) -> anyhow::Result<Self> {
        let (action_response_tx, action_response_rx) = mpsc::channel();

        let tray = tray::build_tray_menu_icon()?;

        let mut _hotkey_manager = None;
        if let Some(hotkey) = config.hotkey.as_ref() {
            let manager = GlobalHotKeyManager::new()?;
            let hotkey = HotKey::from_str(hotkey)?;
            println!("registering for hotkey: {}", hotkey);
            manager.register(hotkey)?;
            _hotkey_manager = Some(manager);
        } else {
            println!("registering for clipboard change")
        }

        if let Some(theme) = config.theme.as_ref() {
            match theme.as_str() {
                "dark" => creation_context.egui_ctx.set_theme(egui::Theme::Dark),
                "light" => creation_context.egui_ctx.set_theme(egui::Theme::Light),
                "system" => {}
                _ => return Err(anyhow::anyhow!("invalid theme: {}", theme)),
            }
        }

        Ok(Self {
            clipboard_text: None,
            window_visible: false,
            window_size: DEFAULT_WINDOW_SIZE,
            window_position: egui::pos2(0.0, 0.0),
            monitor_size: egui::vec2(1024.0, 768.0),
            config,
            is_loading: false,
            loading_start_time: std::time::Instant::now(),
            current_action_label: String::new(),
            show_error_modal: false,
            error_message: String::new(),
            clipboard_rx,
            action_response_rx,
            action_response_tx,
            tray,
            _hotkey_manager,
        })
    }

    fn show_error_modal(&mut self, ctx: &egui::Context) {
        if self.show_error_modal {
            let mut should_close = false;
            let mut show_modal = self.show_error_modal;

            egui::Window::new("Error")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .open(&mut show_modal)
                .show(ctx, |ui| {
                    ui.label(self.error_message.clone());
                    ui.add_space(10.0);
                    if ui.button("OK").clicked() {
                        should_close = true;
                    }
                });

            if should_close || !show_modal {
                self.show_error_modal = false;
                self.error_message.clear();
            }
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        let scale_factor = ctx.pixels_per_point();
        let (mouse_x, mouse_y) = match Mouse::get_mouse_position() {
            Mouse::Position { x, y } => {
                let scaled_x = (x as f32) / scale_factor;
                let scaled_y = (y as f32) / scale_factor;
                (scaled_x, scaled_y)
            }
            _ => (0.0, 0.0),
        };

        // add screen bounds checking
        let max_x = self.monitor_size.x - self.window_size.x;
        let max_y = self.monitor_size.y - self.window_size.y;
        let offset = DEFAULT_WINDOW_OFFSET;
        let window_x = (mouse_x + offset).clamp(0.0, max_x);
        let window_y = (mouse_y + offset).clamp(0.0, max_y);

        self.window_position = egui::pos2(window_x, window_y);

        // position and resize the window at mouse cursor (with small offset)
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(self.window_position));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(self.window_size));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Visible(true));

        self.window_visible = true;
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        if !self.is_loading {
            self.window_visible = false;
            self.show_error_modal = false;
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                egui::ViewportCommand::Visible(false),
            );
            ctx.request_repaint();
        }
    }

    fn render_spinner(&self, ui: &mut egui::Ui) {
        // used for the spinner angle
        let elapsed = self.loading_start_time.elapsed().as_secs_f32();
        // center the spinner and label vertically and horizontally
        let available_rect = ui.available_rect_before_wrap();
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(available_rect)
                .layout(egui::Layout::top_down(egui::Align::Center)),
            |ui| {
                let content_height = spinner::LAYOUT_SIZE.y + 8.0 + 20.0;
                let vertical_offset = (available_rect.height() - content_height) / 2.0;

                if vertical_offset > 0.0 {
                    ui.add_space(vertical_offset);
                }
                ui.allocate_ui_with_layout(
                    spinner::LAYOUT_SIZE,
                    egui::Layout::centered_and_justified(egui::Direction::TopDown),
                    |ui| {
                        spinner::render_spinner(ui, elapsed * 2.0);
                    },
                );
                ui.add_space(8.0);
                ui.label(&self.current_action_label);
            },
        );
    }

    fn render_actions(&mut self, ui: &mut egui::Ui) {
        for action in self.config.actions.iter() {
            if ui
                .button(egui::RichText::new(action.button_text()))
                .clicked()
            {
                if let Some(clipboard_text) = self.clipboard_text.as_ref() {
                    self.is_loading = true;
                    self.loading_start_time = std::time::Instant::now();
                    self.current_action_label = action.label.clone();
                    action.trigger(clipboard_text, self.action_response_tx.clone());
                } else {
                    self.show_error_modal = true;
                    self.error_message = "❌ No clipboard text found".to_string();
                }
            }
        }
    }

    fn render_main(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut clipboard_text = if let Some(text) = self.clipboard_text.as_ref() {
            text.clone()
        } else {
            "❌ No clipboard text found.".to_string()
        };

        egui::ScrollArea::vertical()
            .max_height(DEFAULT_MAX_TEXTAREA_HEIGHT)
            .show(ui, |ui| {
                ui.add_sized(
                    ui.available_size(),
                    egui::TextEdit::multiline(&mut clipboard_text).interactive(false),
                );
            });

        ui.add_space(6.0);

        match self.config.wrap_buttons {
            ButtonsWrap::Horizontal => {
                egui::ScrollArea::horizontal()
                    .id_salt("buttons_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            self.render_actions(ui);
                        });
                    });
            }
            ButtonsWrap::None => {
                let response = ui.horizontal_wrapped(|ui| {
                    self.render_actions(ui);
                });
                // add response height to current window height (only the first time)
                if self.window_size.y == DEFAULT_WINDOW_SIZE.y {
                    let rows_height = response.response.rect.height() - 20.0;
                    let new_height = self.window_size.y + rows_height;
                    self.window_size.y = new_height;
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                        self.window_size.x,
                        new_height,
                    )));
                    // make window non-resizable after calculating height
                    ctx.send_viewport_cmd(egui::ViewportCommand::Resizable(false));
                }
            }
        }
    }

    fn render(&mut self, ctx: &egui::Context) {
        // update monitor size
        ctx.input(|i| {
            if let Some(monitor_size) = i.viewport().monitor_size {
                self.monitor_size = monitor_size;
            }
        });

        if self.window_visible {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.label(format!("📋 Clipboard Buddy v{}", env!("CARGO_PKG_VERSION")));
                    ui.separator();

                    if self.is_loading {
                        self.render_spinner(ui);
                    } else {
                        self.render_main(ui, ctx);
                    }
                });
            });
        } else {
            // when the window is created for the first time, we need to do this
            // to hide the root container
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                egui::ViewportCommand::Visible(false),
            );
        }
    }
}

impl UI {
    fn on_keypress(&mut self, ctx: &egui::Context) {
        if self.is_loading {
            return;
        }

        // check for action key presses
        for action in self.config.actions.iter() {
            if action.key.is_some()
                && ctx.input(|i| {
                    i.key_pressed(egui::Key::from_name(action.key.as_ref().unwrap()).unwrap())
                })
            {
                if let Some(clipboard_text) = self.clipboard_text.as_ref() {
                    self.is_loading = true;
                    self.loading_start_time = std::time::Instant::now();
                    self.current_action_label = action.label.clone();
                    action.trigger(clipboard_text, self.action_response_tx.clone());
                } else {
                    self.show_error_modal = true;
                    self.error_message = "❌ No clipboard text found".to_string();
                }
                break;
            }
        }
    }

    fn on_clibboard_change_or_hotkey(&mut self, ctx: &egui::Context) {
        let mut do_show = false;

        // update clipboard text
        if let Ok(event) = self.clipboard_rx.try_recv() {
            self.clipboard_text = Some(event.text.clone());
            // if no hotkey is set, show window
            if self.config.hotkey.is_none() {
                do_show = true;
            }
        }

        // check for hotkey press if configured
        if self.config.hotkey.is_some()
            && let Ok(_) = GlobalHotKeyEvent::receiver().try_recv()
        {
            do_show = true;
        }

        // show window if needed at the last clipboard change mouse position
        if do_show {
            self.show_window(ctx);
        }
    }

    fn on_action_response(&mut self, _ctx: &egui::Context) {
        if let Ok(response) = self.action_response_rx.try_recv() {
            // stop loading when response is received
            self.is_loading = false;
            self.current_action_label.clear();

            match response {
                ai::ActionEvent::Response(response, do_paste) => {
                    self.clipboard_text = Some(response.clone());
                    if do_paste && let Err(e) = clipboard::set_clipboard_text(response) {
                        self.error_message = format!("❌ Failed to paste to clipboard: {}", e);
                        self.show_error_modal = true;
                    }
                }
                ai::ActionEvent::Error(error) => {
                    self.error_message = format!("❌ {}", error);
                    self.show_error_modal = true;
                }
            }
        }
    }

    fn on_tray_menu_event(&mut self) {
        if let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.tray.reload_menu_item.id()
                && let Err(e) = tray::reload_config()
            {
                self.error_message = format!("❌ Failed to reload config: {}", e);
                self.show_error_modal = true;
            } else if event.id == self.tray.quit_menu_item.id() {
                std::process::exit(0)
            } else if event.id == self.tray.configure_menu_item.id()
                && let Err(e) = tray::open_config_folder()
            {
                self.error_message = format!("❌ Failed to open config folder: {}", e);
                self.show_error_modal = true;
            }
        }
    }

    fn on_esc_pressed(&mut self, ctx: &egui::Context) {
        // handle escape key to hide window
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.hide_window(ctx);
        }
    }

    fn on_mouse_outside_window(&mut self, ctx: &egui::Context) {
        if self.window_visible
            && let Mouse::Position { x, y } = Mouse::get_mouse_position()
        {
            let mouse_x = x as f32;
            let mouse_y = y as f32;

            // calculate window bounds
            let window_rect = egui::Rect::from_min_size(self.window_position, self.window_size);

            // add some margin to prevent accidental hiding when moving cursor to edges
            let margin = 20.0;
            let expanded_rect = window_rect.expand(margin);

            // check if mouse is outside the expanded window bounds
            let mouse_pos = egui::pos2(mouse_x, mouse_y);
            if !expanded_rect.contains(mouse_pos) {
                self.hide_window(ctx);
            }
        }
    }
}

impl eframe::App for UI {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.on_tray_menu_event();
        self.on_clibboard_change_or_hotkey(ctx);
        self.on_esc_pressed(ctx);
        self.on_keypress(ctx);
        self.on_action_response(ctx);

        if self.config.hide_on_mouse_outside_window.unwrap_or(true) {
            self.on_mouse_outside_window(ctx);
        }

        let ui_scale = self.config.ui_scale.unwrap_or(1.0);
        ctx.set_pixels_per_point(ui_scale);

        self.show_error_modal(ctx);

        // always request repaint to ensure we process channel messages
        ctx.request_repaint();

        self.render(ctx);
    }
}

pub fn run(event_rx: mpsc::Receiver<clipboard::Event>, config: Config) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    gtk::init()?;

    let icon = eframe::icon_data::from_png_bytes(&include_bytes!("../../assets/icon-256.png")[..])?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Clipboard Buddy")
            .with_inner_size(DEFAULT_WINDOW_SIZE)
            .with_decorations(false)
            .with_transparent(false)
            .with_always_on_top()
            .with_resizable(matches!(config.wrap_buttons, ButtonsWrap::Horizontal))
            .with_visible(false)
            .with_icon(icon),
        ..Default::default()
    };

    if let Err(e) = eframe::run_native(
        "Clipboard Buddy",
        options,
        Box::new(|cc| Ok(Box::new(UI::new(cc, event_rx, config)?))),
    ) {
        Err(anyhow::anyhow!("eframe::run_native error: {}", e))
    } else {
        Ok(())
    }
}
