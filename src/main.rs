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
        self, Color, Event, Length, Subscription, clipboard,
        event,
        futures::SinkExt,
        keyboard::{Event as KeyEvent, Key, Modifiers, key::Named},
        stream, window,
    },
    iced::platform_specific::{
        runtime::wayland::layer_surface::{IcedOutput, SctkLayerSurfaceSettings},
        shell::commands::layer_surface::{
            Anchor, KeyboardInteractivity, Layer, destroy_layer_surface, get_layer_surface,
            set_keyboard_interactivity,
        },
    },
    widget::{self, menu::key_bind::{KeyBind, Modifier}},
};
use tokio::sync::mpsc;

use config::{COSMIC_THEME_DARK, COSMIC_THEME_LIGHT, ColorSchemeKind, Config};
use menu::{MenuState, context_menu};
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

    let settings = Settings::default()
        .theme(config.app_theme.theme())
        .no_main_window(true);
    cosmic::app::run::<Quake>(
        settings,
        Flags {
            config_handler,
            config,
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
    start_visible: bool,
}

#[derive(Clone, Debug)]
enum Message {
    Config(Box<Config>),
    ContextMenu(Option<MenuState>),
    CopyLink,
    Dbus(dbus::Command),
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

        let color = Color::from(self.config.app_theme.theme().cosmic().background.base);
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
        Task::batch([
            get_layer_surface(SctkLayerSurfaceSettings {
                id,
                layer: Layer::Overlay,
                // Exclusive so the compositor focuses us right away; relaxed to
                // OnDemand in `Message::Settle` so other windows stay clickable.
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                anchor: Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                output: IcedOutput::Active,
                namespace: "cosmic-quake-term".into(),
                size: Some((None, Some(400))),
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
        match self.surface.take() {
            Some(id) => destroy_layer_surface(id),
            None => Task::none(),
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

    fn init(core: Core, flags: Flags) -> (Self, Task<Message>) {
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
                        .find(|(bind, _)| bind.matches(modifiers, &key))
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
            .opacity(self.config.opacity_ratio())
            .padding(space_xxs)
            .sharp_corners(true)
            .show_headerbar(false);

        let Some(menu_state) = &self.context_menu else {
            return terminal_box.into();
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
        widget::popover(terminal_box)
            .popup(context_menu(
                &self.key_binds,
                has_selection,
                menu_state.link.clone(),
            ))
            .position(widget::popover::Position::Point(point))
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            // The status is deliberately ignored: terminal_box captures key presses
            // that match a key bind so they are not sent to the shell, and we still
            // need to see exactly those here.
            event::listen_with(|event, _status, _| match event {
                Event::Keyboard(KeyEvent::KeyPressed { key, modifiers, .. }) => {
                    Some(Message::Key(modifiers, key))
                }
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
            cosmic_config::config_subscription(
                TypeId::of::<ConfigSubscription>(),
                TERM_CONFIG_ID.into(),
                CONFIG_VERSION,
            )
            .map(|update| Message::Config(Box::new(update.config))),
        ])
    }
}
