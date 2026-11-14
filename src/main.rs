// SPDX-License-Identifier: GPL-3.0-only
//! Quake-style drop-down terminal for the COSMIC desktop.
//!
//! The terminal widget (`terminal`, `terminal_box`, `mouse_reporter`,
//! `terminal_theme`, `config`) is derived from cosmic-term
//! (<https://github.com/pop-os/cosmic-term>, GPL-3.0-only).

mod config;
mod dbus;
mod menu;
mod mouse_reporter;
mod settings;
mod terminal;
mod terminal_box;
mod terminal_theme;

use std::{
    any::TypeId,
    collections::HashMap,
    sync::{Mutex, atomic::Ordering},
};

use alacritty_terminal::{event::Event as TermEvent, term, term::color::Colors as TermColors, tty};
use cosmic::{
    Application, Element,
    app::{Core, Settings, Task},
    cosmic_config::{self, CosmicConfigEntry},
    iced::{
        self, Color, Event, Length, Point, Rectangle, Size, Subscription, clipboard, mouse,
        event,
        futures::SinkExt,
        keyboard::{Event as KeyEvent, Key, Modifiers, key::Named},
        stream, window,
    },
    iced::platform_specific::{
        runtime::wayland::layer_surface::{IcedOutput, SctkLayerSurfaceSettings},
        shell::commands::blur::blur,
        shell::commands::layer_surface::{
            Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
            set_anchor, set_keyboard_interactivity, set_size,
        },
    },
    widget::{self, menu::key_bind::{KeyBind, Modifier}},
};
use tokio::sync::mpsc;

use config::{COSMIC_THEME_DARK, COSMIC_THEME_LIGHT, ColorSchemeKind, Config};
use menu::{MenuState, context_menu};
use settings::QuakeConfig;
use terminal::Terminal;
use terminal_box::terminal_box;

const CONFIG_VERSION: u64 = config::CONFIG_VERSION;
/// We deliberately share cosmic-term's config so font, colors and opacity match.
const TERM_CONFIG_ID: &str = "com.system76.CosmicTerm";

/// Actions reachable through key bindings. `terminal_box` uses the key-bind map
/// to decide which key presses must NOT be forwarded to the shell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Copy,
    Paste,
    SelectAll,
    ZoomIn,
    ZoomOut,
    ZoomReset,
}

fn default_key_binds() -> HashMap<KeyBind, Action> {
    let mut map = HashMap::new();
    let mut bind = |mods: &[Modifier], key: Key, action: Action| {
        map.insert(
            KeyBind {
                modifiers: mods.to_vec(),
                key,
            },
            action,
        );
    };
    let cs = [Modifier::Ctrl, Modifier::Shift];
    bind(&cs, Key::Character("C".into()), Action::Copy);
    bind(&cs, Key::Character("V".into()), Action::Paste);
    bind(&cs, Key::Character("A".into()), Action::SelectAll);
    bind(&[Modifier::Ctrl], Key::Character("=".into()), Action::ZoomIn);
    bind(&cs, Key::Character("+".into()), Action::ZoomIn);
    bind(&[Modifier::Ctrl], Key::Character("-".into()), Action::ZoomOut);
    bind(&[Modifier::Ctrl], Key::Character("0".into()), Action::ZoomReset);
    map
}

const USAGE: &str = "\
cosmic-quake-term - Quake-style drop-down terminal for COSMIC

USAGE:
    cosmic-quake-term              start (or show the running instance)
    cosmic-quake-term --toggle     show/hide; starts the daemon if needed
    cosmic-quake-term --show       show the terminal
    cosmic-quake-term --hide       hide the terminal
    cosmic-quake-term --quit       stop the running daemon
    cosmic-quake-term --daemon     start hidden in the background (for autostart)
    cosmic-quake-term --height N   set the height in percent of the screen (10-100)
    cosmic-quake-term --width N    set the width in percent of the screen (10-100)
                                   (both can be combined; a running daemon applies them live)
    cosmic-quake-term --help       show this help
";

fn main() -> iced::Result {
    // Parse the command line before touching any GUI state.
    let mut args = std::env::args().skip(1);
    let start_visible = match args.next().as_deref() {
        None => {
            if dbus::is_running() {
                exit_after_send(dbus::Command::Show);
            }
            true
        }
        Some("--daemon") | Some("--hidden") => {
            if dbus::is_running() {
                eprintln!("cosmic-quake-term is already running");
                std::process::exit(0);
            }
            false
        }
        Some(flag @ ("--toggle" | "--show" | "--hide" | "--quit")) => {
            let command = match flag {
                "--toggle" => dbus::Command::Toggle,
                "--show" => dbus::Command::Show,
                "--hide" => dbus::Command::Hide,
                _ => dbus::Command::Quit,
            };
            if dbus::is_running() {
                exit_after_send(command);
            }
            // No daemon yet: only toggle/show make sense, they start one.
            if matches!(command, dbus::Command::Hide | dbus::Command::Quit) {
                eprintln!("cosmic-quake-term is not running");
                std::process::exit(0);
            }
            true
        }
        Some(flag @ ("--height" | "--width")) => {
            let mut height = None;
            let mut width = None;
            let mut next = Some(flag.to_string());
            while let Some(flag) = next.take() {
                let value = args.next().and_then(|v| v.parse::<u16>().ok());
                let Some(value) = value else {
                    eprintln!("{flag} needs a whole number (percent)\n\n{USAGE}");
                    std::process::exit(2);
                };
                match flag.as_str() {
                    "--height" => height = Some(value),
                    "--width" => width = Some(value),
                    _ => {
                        eprintln!("unexpected argument {flag:?}\n\n{USAGE}");
                        std::process::exit(2);
                    }
                }
                next = args.next();
            }
            match settings::save(height, width) {
                Ok(config) => {
                    println!(
                        "height {}%, width {}%",
                        config.height_percent, config.width_percent
                    );
                    std::process::exit(0);
                }
                Err(err) => {
                    eprintln!("failed to save settings: {err}");
                    std::process::exit(1);
                }
            }
        }
        Some("--help") | Some("-h") => {
            print!("{USAGE}");
            std::process::exit(0);
        }
        Some(other) => {
            eprintln!("unknown argument {other:?}\n\n{USAGE}");
            std::process::exit(2);
        }
    };

    let (config_handler, config) = match cosmic_config::Config::new(TERM_CONFIG_ID, CONFIG_VERSION)
    {
        Ok(handler) => {
            let config = match Config::get_entry(&handler) {
                Ok(config) => config,
                Err((errors, config)) => {
                    log::info!("errors loading config: {errors:?}");
                    config
                }
            };
            (Some(handler), config)
        }
        Err(err) => {
            log::error!("failed to create config handler: {err}");
            (None, Config::default())
        }
    };

    tty::setup_env();
    unsafe {
        std::env::set_var("TERM", "xterm-256color");
    }

    let (quake_handler, quake) = settings::load();

    let settings = Settings::default()
        .theme(config.app_theme.theme())
        .no_main_window(true);
    cosmic::app::run::<Quake>(
        settings,
        Flags {
            config_handler,
            config,
            quake,
            quake_handler,
            start_visible,
        },
    )
}

/// Forward `command` to the running daemon and terminate this client process.
fn exit_after_send(command: dbus::Command) -> ! {
    match dbus::send(command) {
        Ok(()) => std::process::exit(0),
        Err(err) => {
            eprintln!("failed to contact cosmic-quake-term: {err}");
            std::process::exit(1);
        }
    }
}

#[derive(Clone)]
struct Flags {
    config_handler: Option<cosmic_config::Config>,
    config: Config,
    quake: QuakeConfig,
    quake_handler: Option<cosmic_config::Config>,
    start_visible: bool,
}

#[derive(Clone, Debug)]
enum Message {
    Config(Box<Config>),
    ContextMenu(Option<MenuState>),
    CopyLink,
    Dbus(dbus::Command),
    QuakeConfig(Box<QuakeConfig>),
    ResizeStart,
    ResizeMove(f32),
    ResizeEnd,
    HandleHover(bool),
    MenuAction(Action),
    OpenLink,
    Key(Modifiers, Key),
    Paste,
    PasteValue(String),
    TermEvent(TermEvent),
    Toggle,
    Zoom(i8),
    ZoomReset,
    /// Sent shortly after the surface is mapped: relax exclusive keyboard grab.
    Settle(window::Id),
    /// First configure or a resize of a layer surface, in logical pixels.
    SurfaceSize(window::Id, cosmic::iced::Size),
}

struct Quake {
    core: Core,
    #[allow(dead_code)]
    config_handler: Option<cosmic_config::Config>,
    config: Config,
    themes: HashMap<(String, ColorSchemeKind), TermColors>,
    term_config: term::Config,
    key_binds: HashMap<KeyBind, Action>,
    terminal: Option<Mutex<Terminal>>,
    term_tx: Option<mpsc::UnboundedSender<TermEvent>>,
    terminal_id: widget::Id,
    surface: Option<window::Id>,
    context_menu: Option<MenuState>,
    quake: QuakeConfig,
    /// True between creating the surface and learning the output size.
    measuring: bool,
    quake_handler: Option<cosmic_config::Config>,
    /// Logical size of the output the surface was measured on.
    output_size: Option<(f32, f32)>,
    /// Current logical size of the surface (width `None` = full width).
    surface_width: Option<u32>,
    surface_height: u32,
    /// True while the user drags the bottom edge.
    resizing: bool,
    /// While dragging (and until the final shrink is acknowledged) the content is laid
    /// out at this height inside a surface that may be taller. The Wayland surface is
    /// resized only twice per drag (grow at the start, shrink at the end) instead of on
    /// every pointer event, which is what made the window flicker.
    display_height: Option<f32>,
    /// The final shrink was requested and its configure has not arrived yet.
    shrink_pending: bool,
    /// Pointer is over the grab strip.
    handle_hover: bool,
}

impl Quake {
    fn update_theme_state(&mut self) {
        self.themes = terminal_theme::terminal_themes();
        for kind in [ColorSchemeKind::Dark, ColorSchemeKind::Light] {
            for color_scheme in self.config.color_schemes(kind).values() {
                self.themes
                    .insert((color_scheme.name.clone(), kind), color_scheme.into());
            }
        }

        let color = Color::from(self.config.app_theme.theme().cosmic().background(false).base);
        let b = color.into_rgba8();
        let data =
            u32::from(b[2]) | (u32::from(b[1]) << 8) | (u32::from(b[0]) << 16) | 0xFF000000;
        terminal::WINDOW_BG_COLOR.store(data, Ordering::SeqCst);
    }

    fn colors(&self) -> Option<TermColors> {
        self.themes
            .get(&self.config.syntax_theme(None))
            .or_else(|| match self.config.color_scheme_kind() {
                ColorSchemeKind::Dark => self
                    .themes
                    .get(&(COSMIC_THEME_DARK.to_string(), ColorSchemeKind::Dark)),
                ColorSchemeKind::Light => self
                    .themes
                    .get(&(COSMIC_THEME_LIGHT.to_string(), ColorSchemeKind::Light)),
            })
            .copied()
    }

    fn spawn_terminal(&mut self, tx: mpsc::UnboundedSender<TermEvent>) {
        let Some(colors) = self.colors() else {
            log::error!("no terminal color scheme available");
            return;
        };
        match Terminal::new(
            tx,
            self.term_config.clone(),
            tty::Options::default(),
            &self.config,
            colors,
            None,
            None,
        ) {
            Ok(mut terminal) => {
                terminal.set_config(&self.config, &self.themes);
                self.terminal = Some(Mutex::new(terminal));
            }
            Err(err) => log::error!("failed to open terminal: {err}"),
        }
    }

    fn show(&mut self) -> Task<Message> {
        if self.surface.is_some() {
            return Task::none();
        }
        let id = window::Id::unique();
        self.surface = Some(id);
        self.measuring = true;
        Task::batch([
            // Layer-shell only knows pixels. To turn percentages into pixels we first
            // map an invisible surface anchored to all edges: the compositor sizes it
            // to the active output, and `Message::SurfaceSize` then shrinks it to the
            // configured size. It only exists for a moment and draws nothing.
            get_layer_surface(SctkLayerSurfaceSettings {
                id,
                layer: Layer::Overlay,
                // Exclusive so the compositor focuses us right away; relaxed to
                // OnDemand in `Message::Settle` so other windows stay clickable.
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                output: IcedOutput::Active,
                namespace: "cosmic-quake-term".into(),
                size: Some((None, None)),
                size_limits: cosmic::iced::core::layout::Limits::NONE
                    .min_width(1.0)
                    .min_height(1.0)
                    .max_width(16384.0)
                    .max_height(16384.0),
                ..Default::default()
            }),
            widget::text_input::focus(self.terminal_id.clone()),
            Task::perform(
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    id
                },
                |id| cosmic::Action::App(Message::Settle(id)),
            ),
        ])
    }

    fn hide(&mut self) -> Task<Message> {
        self.measuring = false;
        self.resizing = false;
        self.display_height = None;
        self.shrink_pending = false;
        self.handle_hover = false;
        match self.surface.take() {
            Some(id) => destroy_layer_surface(id),
            None => Task::none(),
        }
    }

    /// Does the system theme ask for frosted (translucent + blurred) system surfaces?
    fn frosted(&self) -> bool {
        cosmic::theme::active().cosmic().frosted_system_interface
    }

    /// Background alpha chosen in COSMIC Settings (1.0 when transparency is off).
    fn system_alpha(&self) -> f32 {
        if self.frosted() {
            cosmic::theme::active().cosmic().background(true).base.alpha
        } else {
            1.0
        }
    }

    /// cosmic-term's own opacity setting combined with the system transparency.
    fn effective_opacity(&self) -> f32 {
        self.config.opacity_ratio() * self.system_alpha()
    }

    /// Blur only the area the terminal actually covers.
    fn blur_task(&self, id: window::Id, height: f32) -> Task<Message> {
        if !self.frosted() {
            return Task::none();
        }
        let width = self
            .surface_width
            .map(|w| w as f32)
            .or(self.output_size.map(|(w, _)| w))
            .unwrap_or(0.0);
        blur(
            id,
            Some(vec![Rectangle::new(Point::ORIGIN, Size::new(width, height))]),
        )
        .map(|()| cosmic::Action::None)
    }

    /// Put a thin grab strip under the terminal; dragging it resizes the surface.
    fn with_resize_handle<'a>(&self, content: Element<'a, Message>) -> Element<'a, Message> {
        const HANDLE_HEIGHT: f32 = 6.0;
        // At rest the strip is painted like the terminal so it is not noticeable; it
        // only lights up while the pointer is on it or a drag is in progress.
        let highlighted = self.handle_hover || self.resizing;
        let idle = self
            .terminal
            .as_ref()
            .map(|terminal| {
                let terminal = terminal.lock().unwrap();
                let bg = terminal.metadata_set[terminal.default_attrs().metadata].bg;
                Color::from_rgba8(bg.r(), bg.g(), bg.b(), self.effective_opacity())
            })
            .unwrap_or(Color::TRANSPARENT);
        let strip = widget::container(widget::space::vertical())
            .width(Length::Fill)
            .height(Length::Fixed(HANDLE_HEIGHT))
            .class(cosmic::theme::Container::custom(move |theme| {
                let color = if highlighted {
                    Color::from(theme.cosmic().accent_color())
                } else {
                    idle
                };
                widget::container::Style {
                    background: Some(color.into()),
                    ..Default::default()
                }
            }));
        let handle = widget::mouse_area(strip)
            .on_enter(Message::HandleHover(true))
            .on_exit(Message::HandleHover(false))
            .on_press(Message::ResizeStart)
            .interaction(mouse::Interaction::ResizingVertically);
        let column = widget::column::with_children(vec![content, handle.into()])
            .width(Length::Fill)
            .height(Length::Fill);
        match self.display_height {
            // Surface is taller than the terminal: draw only the top part, the rest
            // of the surface stays transparent.
            Some(height) => widget::container(column)
                .width(Length::Fill)
                .height(Length::Fixed(height))
                .into(),
            None => column.into(),
        }
    }

    fn close_context_menu(&mut self) {
        self.context_menu = None;
        if let Some(terminal) = &self.terminal {
            terminal.lock().unwrap().context_menu = None;
        }
    }

    fn run_action(&mut self, action: Action) -> Task<Message> {
        match action {
            Action::Copy => {
                if let Some(terminal) = &self.terminal {
                    let terminal = terminal.lock().unwrap();
                    let term = terminal.term.lock();
                    if let Some(text) = term.selection_to_string() {
                        return clipboard::write(text);
                    }
                }
                Task::none()
            }
            Action::Paste => self.update(Message::Paste),
            Action::SelectAll => {
                if let Some(terminal) = &self.terminal {
                    let mut terminal = terminal.lock().unwrap();
                    terminal.select_all();
                    terminal.needs_update = true;
                }
                Task::none()
            }
            Action::ZoomIn => self.update(Message::Zoom(1)),
            Action::ZoomOut => self.update(Message::Zoom(-1)),
            Action::ZoomReset => self.update(Message::ZoomReset),
        }
    }
}

/// Receiver for terminal events. Created in `init` (so the shell can start before
/// the first frame) and taken over by the event subscription.
static TERM_EVENT_RX: Mutex<Option<mpsc::UnboundedReceiver<TermEvent>>> = Mutex::new(None);

struct TerminalEventSubscription;
struct ConfigSubscription;
struct DbusSubscription;
struct QuakeConfigSubscription;

impl Application for Quake {
    type Executor = cosmic::executor::Default;
    type Flags = Flags;
    type Message = Message;
    const APP_ID: &'static str = "dev.norman.CosmicQuakeTerm";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(mut core: Core, flags: Flags) -> (Self, Task<Message>) {
        // libcosmic would blur the *whole* layer surface, including the invisible
        // measuring surface and the transparent area below the terminal while
        // dragging. We request blur ourselves, limited to the terminal rectangle.
        core.set_auto_blur(Default::default());
        let mut app = Quake {
            core,
            config_handler: flags.config_handler,
            config: flags.config,
            themes: HashMap::new(),
            term_config: term::Config::default(),
            key_binds: default_key_binds(),
            terminal: None,
            term_tx: None,
            terminal_id: widget::Id::unique(),
            surface: None,
            context_menu: None,
            quake: flags.quake,
            measuring: false,
            quake_handler: flags.quake_handler,
            output_size: None,
            surface_width: None,
            surface_height: 0,
            resizing: false,
            display_height: None,
            shrink_pending: false,
            handle_hover: false,
        };
        app.update_theme_state();
        // Start the shell right away so the very first frame already shows the terminal.
        let (tx, rx) = mpsc::unbounded_channel();
        *TERM_EVENT_RX.lock().unwrap() = Some(rx);
        app.term_tx = Some(tx.clone());
        app.spawn_terminal(tx);
        let task = if flags.start_visible {
            app.show()
        } else {
            Task::none()
        };
        (app, task)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Config(config) => {
                if self.config != *config {
                    self.config = *config;
                    self.update_theme_state();
                    if let Some(terminal) = &self.terminal {
                        terminal
                            .lock()
                            .unwrap()
                            .set_config(&self.config, &self.themes);
                    }
                    return cosmic::command::set_theme(self.config.app_theme.theme());
                }
            }
            Message::ContextMenu(menu_state) => {
                if let Some(terminal) = &self.terminal {
                    terminal.lock().unwrap().context_menu = menu_state.clone();
                }
                self.context_menu = menu_state;
            }
            Message::MenuAction(action) => {
                self.close_context_menu();
                return self.run_action(action);
            }
            Message::OpenLink => {
                let link = self.context_menu.as_ref().and_then(|m| m.link.clone());
                self.close_context_menu();
                if let Some(link) = link {
                    if let Err(err) = open::that_detached(&link) {
                        log::warn!("failed to open {link:?}: {err}");
                    }
                }
            }
            Message::CopyLink => {
                let link = self.context_menu.as_ref().and_then(|m| m.link.clone());
                self.close_context_menu();
                if let Some(link) = link {
                    return clipboard::write(link);
                }
            }
            Message::Key(modifiers, key) => match key {
                Key::Named(Named::Copy) => return self.run_action(Action::Copy),
                Key::Named(Named::Paste) => return self.run_action(Action::Paste),
                _ => {
                    let action = self
                        .key_binds
                        .iter()
                        .find(|(bind, _)| bind.matches(modifiers, &key, None))
                        .map(|(_, action)| *action);
                    if let Some(action) = action {
                        return self.run_action(action);
                    }
                }
            },
            Message::Paste => {
                return clipboard::read().map(|value| match value {
                    Some(value) => cosmic::Action::App(Message::PasteValue(value)),
                    None => cosmic::Action::None,
                });
            }
            Message::PasteValue(value) => {
                if let Some(terminal) = &self.terminal {
                    terminal.lock().unwrap().paste(value);
                }
            }
            Message::TermEvent(event) => match event {
                TermEvent::ClipboardLoad(kind, callback) => {
                    if let term::ClipboardType::Clipboard = kind {
                        return clipboard::read().map(move |data| {
                            callback(&data.unwrap_or_default());
                            cosmic::Action::None
                        });
                    }
                }
                TermEvent::ClipboardStore(kind, data) => {
                    if let term::ClipboardType::Clipboard = kind {
                        return clipboard::write(data);
                    }
                }
                TermEvent::ColorRequest(index, f) => {
                    if let Some(terminal) = &self.terminal {
                        let terminal = terminal.lock().unwrap();
                        let rgb = terminal.colors()[index].unwrap_or_default();
                        terminal.input_no_scroll(f(rgb).into_bytes());
                    }
                }
                TermEvent::PtyWrite(text) => {
                    if let Some(terminal) = &self.terminal {
                        terminal.lock().unwrap().input_no_scroll(text.into_bytes());
                    }
                }
                TermEvent::TextAreaSizeRequest(f) => {
                    if let Some(terminal) = &self.terminal {
                        let terminal = terminal.lock().unwrap();
                        terminal.input_no_scroll(f(terminal.size().into()).into_bytes());
                    }
                }
                TermEvent::MouseCursorDirty | TermEvent::Wakeup => {
                    if let Some(terminal) = &self.terminal {
                        terminal.lock().unwrap().needs_update = true;
                    }
                }
                TermEvent::Exit => {
                    // The shell ended (e.g. `exit`): hide and start a fresh shell so
                    // the next toggle shows a working prompt.
                    self.close_context_menu();
                    self.terminal = None;
                    if let Some(tx) = self.term_tx.clone() {
                        self.spawn_terminal(tx);
                    }
                    return self.hide();
                }
                _ => {}
            },
            Message::Dbus(command) => {
                return match command {
                    dbus::Command::Toggle => self.update(Message::Toggle),
                    dbus::Command::Show => self.show(),
                    dbus::Command::Hide => self.hide(),
                    dbus::Command::Quit => cosmic::iced::exit(),
                };
            }
            Message::QuakeConfig(config) => {
                if self.quake != *config {
                    self.quake = *config;
                    // Re-map the surface so the new size takes effect immediately.
                    if self.surface.is_some() {
                        let hide = self.hide();
                        let show = self.show();
                        return Task::batch([hide, show]);
                    }
                }
            }
            Message::SurfaceSize(id, size)
                if self.shrink_pending && !self.resizing && self.surface == Some(id) =>
            {
                // Ignore configures that are still about the earlier grow.
                if (size.height - self.surface_height as f32).abs() < 1.5 {
                    self.shrink_pending = false;
                    self.display_height = None;
                }
            }
            Message::SurfaceSize(id, size) => {
                if self.measuring && self.surface == Some(id) {
                    self.measuring = false;
                    self.output_size = Some((size.width, size.height));
                    let (width, height) = self.quake.target_size((size.width, size.height));
                    self.surface_width = width;
                    self.surface_height = height;
                    // Anchored to the top edge only the surface is centered horizontally;
                    // with full width it also spans left to right.
                    let anchor = if width.is_some() {
                        Anchor::TOP
                    } else {
                        Anchor::TOP | Anchor::LEFT | Anchor::RIGHT
                    };
                    return Task::batch([
                        set_anchor(id, anchor),
                        set_size(id, width, Some(height)),
                        self.blur_task(id, height as f32),
                        widget::text_input::focus(self.terminal_id.clone()),
                    ]);
                }
            }
            Message::ResizeStart => {
                if let (Some(id), false, Some((_, output_h))) =
                    (self.surface, self.measuring, self.output_size)
                {
                    self.resizing = true;
                    self.shrink_pending = false;
                    // Keep showing the content at its current height, then grow the
                    // surface to the full output height once.
                    self.display_height = Some(self.surface_height as f32);
                    return set_size(id, self.surface_width, Some(output_h.ceil() as u32));
                }
            }
            Message::ResizeMove(y) => {
                // The surface is anchored to the top, so the pointer's y coordinate
                // inside it is exactly the height the user is asking for. Only the
                // layout changes here; no Wayland request is sent.
                if let (true, Some((_, output_h))) = (self.resizing, self.output_size) {
                    let min = output_h * f32::from(settings::MIN_PERCENT) / 100.0;
                    let height = y.clamp(min, output_h).round();
                    let changed = self.surface_height != height as u32;
                    self.surface_height = height as u32;
                    self.display_height = Some(height);
                    if let (true, Some(id)) = (changed, self.surface) {
                        return self.blur_task(id, height);
                    }
                }
            }
            Message::ResizeEnd => {
                if !self.resizing {
                    return Task::none();
                }
                self.resizing = false;
                let (Some(id), Some(output)) = (self.surface, self.output_size) else {
                    return Task::none();
                };
                let percent = settings::clamp_percent(
                    (self.surface_height as f32 / output.1 * 100.0).round() as u16,
                );
                // Save, then snap to the stored percentage so what you see is what a
                // fresh start will give. `self.quake` is updated first so the config
                // watcher's echo is recognised as "no change" and does not re-map us.
                self.quake.height_percent = percent;
                if let Some(handler) = &self.quake_handler {
                    let mut stored = self.quake.clone();
                    if let Err(err) = stored.set_height_percent(handler, percent) {
                        log::warn!("failed to save height: {err}");
                    }
                }
                let (width, height) = self.quake.target_size(output);
                self.surface_width = width;
                self.surface_height = height;
                // Keep the fixed-height layout until the surface has really shrunk,
                // otherwise the terminal would stretch over the tall surface for a frame.
                self.display_height = Some(height as f32);
                self.shrink_pending = true;
                return Task::batch([
                    self.blur_task(id, height as f32),
                    set_size(id, width, Some(height)),
                ]);
            }
            Message::HandleHover(hover) => self.handle_hover = hover,
            Message::Settle(id) => {
                if self.surface == Some(id) {
                    return Task::batch([
                        widget::text_input::focus(self.terminal_id.clone()),
                        set_keyboard_interactivity(id, KeyboardInteractivity::OnDemand),
                    ]);
                }
            }
            Message::Toggle => {
                return if self.surface.is_some() {
                    self.hide()
                } else {
                    self.show()
                };
            }
            Message::Zoom(delta) => {
                if let Some(terminal) = &self.terminal {
                    let mut terminal = terminal.lock().unwrap();
                    let zoom = terminal.zoom_adj().saturating_add(delta);
                    terminal.set_zoom_adj(zoom);
                    terminal.set_config(&self.config, &self.themes);
                }
            }
            Message::ZoomReset => {
                if let Some(terminal) = &self.terminal {
                    let mut terminal = terminal.lock().unwrap();
                    terminal.set_zoom_adj(0);
                    terminal.set_config(&self.config, &self.themes);
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        widget::text("").into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Message> {
        if self.measuring {
            return widget::space::vertical().into();
        }
        let Some(terminal) = &self.terminal else {
            // Same colour the terminal uses, so there is no flash while it is missing.
            let bg = terminal::WINDOW_BG_COLOR.load(Ordering::SeqCst);
            let color = Color::from_rgb8((bg >> 16) as u8, (bg >> 8) as u8, bg as u8);
            return widget::container(widget::text(""))
                .width(Length::Fill)
                .height(Length::Fill)
                .class(cosmic::theme::Container::custom(move |_| {
                    widget::container::Style {
                        background: Some(color.into()),
                        ..Default::default()
                    }
                }))
                .into();
        };
        let space_xxs = cosmic::theme::active().cosmic().space_xxs();
        let mut terminal_box = terminal_box(terminal, &self.key_binds)
            .id(self.terminal_id.clone())
            .on_context_menu(Message::ContextMenu)
            .opacity(self.effective_opacity())
            .padding(space_xxs)
            .sharp_corners(true)
            .show_headerbar(false);

        let Some(menu_state) = &self.context_menu else {
            return self.with_resize_handle(terminal_box.into());
        };
        let point = menu_state
            .local_position
            .or(menu_state.position)
            .unwrap_or_default();
        // Tell terminal_box a menu is open so the next click dismisses it.
        terminal_box = terminal_box.context_menu(point);
        let has_selection = terminal
            .lock()
            .unwrap()
            .term
            .lock()
            .selection_to_string()
            .is_some();
        let popover = widget::popover(terminal_box)
            .popup(context_menu(
                &self.key_binds,
                has_selection,
                menu_state.link.clone(),
            ))
            .position(widget::popover::Position::Point(point));
        self.with_resize_handle(popover.into())
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            // The status is deliberately ignored: terminal_box captures key presses
            // that match a key bind so they are not sent to the shell, and we still
            // need to see exactly those here.
            event::listen_with(|event, _status, id| match event {
                Event::Keyboard(KeyEvent::KeyPressed { key, modifiers, .. }) => {
                    Some(Message::Key(modifiers, key))
                }
                Event::Window(
                    window::Event::Opened { size, .. } | window::Event::Resized(size),
                ) => Some(Message::SurfaceSize(id, size)),
                _ => None,
            }),
            Subscription::run_with(TypeId::of::<TerminalEventSubscription>(), |_| {
                stream::channel(
                    100,
                    |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                        let Some(mut rx) = TERM_EVENT_RX.lock().unwrap().take() else {
                            std::future::pending::<()>().await;
                            return;
                        };
                        while let Some(event) = rx.recv().await {
                            output.send(Message::TermEvent(event)).await.unwrap();
                        }
                        // Keep the stream (and its sender) alive until shutdown.
                        std::future::pending::<()>().await;
                    },
                )
            }),
            Subscription::run_with(TypeId::of::<DbusSubscription>(), |_| {
                stream::channel(
                    16,
                    |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
                        let (tx, mut rx) = mpsc::unbounded_channel();
                        // Keep the connection alive for as long as the stream runs.
                        let _connection = match dbus::serve(tx).await {
                            Ok(connection) => connection,
                            Err(err) => {
                                log::error!("D-Bus service unavailable: {err}");
                                std::future::pending::<()>().await;
                                return;
                            }
                        };
                        while let Some(command) = rx.recv().await {
                            if output.send(Message::Dbus(command)).await.is_err() {
                                break;
                            }
                        }
                        std::future::pending::<()>().await;
                    },
                )
            }),
            if self.resizing {
                // Only listen to pointer motion while a drag is in progress, so idle
                // mouse movement does not wake the app.
                event::listen_with(|event, _status, _id| match event {
                    Event::Mouse(mouse::Event::CursorMoved { position }) => {
                        Some(Message::ResizeMove(position.y))
                    }
                    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                        Some(Message::ResizeEnd)
                    }
                    _ => None,
                })
            } else {
                Subscription::none()
            },
            cosmic_config::config_subscription(
                TypeId::of::<QuakeConfigSubscription>(),
                settings::ID.into(),
                settings::VERSION,
            )
            .map(|update| Message::QuakeConfig(Box::new(update.config))),
            cosmic_config::config_subscription(
                TypeId::of::<ConfigSubscription>(),
                TERM_CONFIG_ID.into(),
                CONFIG_VERSION,
            )
            .map(|update| Message::Config(Box::new(update.config))),
        ])
    }
}
