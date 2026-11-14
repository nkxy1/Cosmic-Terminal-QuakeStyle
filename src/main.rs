// SPDX-License-Identifier: GPL-3.0-only
//! Quake-style drop-down terminal for the COSMIC desktop.
//!
//! The terminal widget (`terminal`, `terminal_box`, `mouse_reporter`,
//! `terminal_theme`, `config`) is derived from cosmic-term
//! (<https://github.com/pop-os/cosmic-term>, GPL-3.0-only).

mod config;
mod dbus;
mod localize;
mod menu;
mod mouse_reporter;
mod settings;
mod slide;
mod tabs;
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
    cctk::sctk::reexports::client::protocol::wl_output::WlOutput,
    iced::event::{PlatformSpecific, wayland::{Event as WaylandEvent, OutputEvent}},
    widget::{self, menu::key_bind::{KeyBind, Modifier}, pane_grid, segmented_button},
};
use tokio::sync::mpsc;

use config::{COSMIC_THEME_DARK, COSMIC_THEME_LIGHT, ColorSchemeKind, Config};
use menu::{MenuState, context_menu};
use settings::QuakeConfig;
use tabs::{Tab, TermPane};
use terminal::{Terminal, TerminalId};
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
    Settings,
    NewTab,
    ClosePane,
    SplitHorizontal,
    SplitVertical,
    NextTab,
    PrevTab,
    /// Jump to the n-th tab (0-based).
    ActivateTab(usize),
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    ToggleMaximize,
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
    // Same defaults as cosmic-term for tabs and splits.
    bind(&cs, Key::Character("T".into()), Action::NewTab);
    bind(&cs, Key::Character("W".into()), Action::ClosePane);
    let ca = [Modifier::Ctrl, Modifier::Alt];
    bind(&ca, Key::Character("d".into()), Action::SplitHorizontal);
    bind(&ca, Key::Character("r".into()), Action::SplitVertical);
    bind(&cs, Key::Character("X".into()), Action::ToggleMaximize);
    bind(&[Modifier::Ctrl], Key::Named(Named::Tab), Action::NextTab);
    bind(&cs, Key::Named(Named::Tab), Action::PrevTab);
    for n in 0..9usize {
        bind(&cs, Key::Character((n + 1).to_string().into()), Action::ActivateTab(n));
    }
    bind(&cs, Key::Named(Named::ArrowLeft), Action::FocusLeft);
    bind(&cs, Key::Character("H".into()), Action::FocusLeft);
    bind(&cs, Key::Named(Named::ArrowRight), Action::FocusRight);
    bind(&cs, Key::Character("L".into()), Action::FocusRight);
    bind(&cs, Key::Named(Named::ArrowUp), Action::FocusUp);
    bind(&cs, Key::Character("K".into()), Action::FocusUp);
    bind(&cs, Key::Named(Named::ArrowDown), Action::FocusDown);
    bind(&cs, Key::Character("J".into()), Action::FocusDown);
    bind(&[Modifier::Ctrl], Key::Character("=".into()), Action::ZoomIn);
    bind(&cs, Key::Character("+".into()), Action::ZoomIn);
    bind(&[Modifier::Ctrl], Key::Character("-".into()), Action::ZoomOut);
    bind(&[Modifier::Ctrl], Key::Character("0".into()), Action::ZoomReset);
    map
}

/// Help text in the user's language.
fn usage() -> String {
    let rows = [
        ("", fl!("cli-start")),
        ("--toggle", fl!("cli-toggle")),
        ("--show", fl!("cli-show")),
        ("--hide", fl!("cli-hide")),
        ("--quit", fl!("cli-quit")),
        ("--daemon", fl!("cli-daemon")),
        ("--height N", fl!("cli-height")),
        ("--width N", fl!("cli-width")),
        ("--animation on|off|MS", fl!("cli-animation")),
        ("--monitor NAME|active", fl!("cli-monitor")),
        ("--inherit-cwd on|off", fl!("cli-inherit-cwd")),
        ("--list-monitors", fl!("cli-list-monitors")),
        ("--new-tab", fl!("cli-new-tab")),
        ("--split-horizontal", fl!("cli-split-horizontal")),
        ("--split-vertical", fl!("cli-split-vertical")),
        ("--help", fl!("cli-help")),
    ];
    let mut text = format!(
        "cosmic-quake-term - {}\n\n{}:\n",
        fl!("cli-description"),
        fl!("cli-usage")
    );
    for (flag, description) in rows {
        text.push_str(&format!("    cosmic-quake-term {flag:<22} {description}\n"));
    }
    text.push_str(&format!("\n{}\n", fl!("cli-live-note")));
    text
}

fn main() -> iced::Result {
    // Pick the language first: even the command line messages are translated.
    localize::localize();

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
                eprintln!("{}", fl!("cli-already-running"));
                std::process::exit(0);
            }
            false
        }
        Some(flag @ ("--toggle" | "--show" | "--hide" | "--quit" | "--new-tab" | "--split-horizontal" | "--split-vertical")) => {
            let command = match flag {
                "--toggle" => dbus::Command::Toggle,
                "--show" => dbus::Command::Show,
                "--hide" => dbus::Command::Hide,
                "--new-tab" => dbus::Command::NewTab,
                "--split-horizontal" => dbus::Command::SplitHorizontal,
                "--split-vertical" => dbus::Command::SplitVertical,
                _ => dbus::Command::Quit,
            };
            if dbus::is_running() {
                exit_after_send(command);
            }
            // No daemon yet: only toggle/show make sense, they start one.
            if !matches!(command, dbus::Command::Toggle | dbus::Command::Show) {
                eprintln!("{}", fl!("cli-not-running"));
                std::process::exit(0);
            }
            true
        }
        Some("--list-monitors") => {
            if !dbus::is_running() {
                eprintln!("{}", fl!("cli-not-running"));
                std::process::exit(1);
            }
            match dbus::list_monitors() {
                Ok(names) if names.is_empty() => println!("{}", fl!("cli-no-monitors")),
                Ok(names) => names.iter().for_each(|name| println!("{name}")),
                Err(err) => {
                    eprintln!("{}", fl!("cli-contact-failed", error = err.to_string()));
                    std::process::exit(1);
                }
            }
            std::process::exit(0);
        }
        Some(flag @ ("--height" | "--width" | "--animation" | "--monitor" | "--inherit-cwd")) => {
            let mut update = settings::Update::default();
            let mut next = Some(flag.to_string());
            while let Some(flag) = next.take() {
                let value = args.next();
                let bad = |kind: u8| -> ! {
                    let message = match kind {
                        1 => fl!("cli-needs-animation", flag = flag.clone()),
                        2 => fl!("cli-needs-monitor", flag = flag.clone()),
                        3 => fl!("cli-needs-switch", flag = flag.clone()),
                        _ => fl!("cli-needs-percent", flag = flag.clone()),
                    };
                    eprintln!("{message}\n\n{}", usage());
                    std::process::exit(2);
                };
                match flag.as_str() {
                    "--height" => {
                        update.height = Some(
                            value.and_then(|v| v.parse().ok()).unwrap_or_else(|| bad(0)),
                        )
                    }
                    "--width" => {
                        update.width = Some(
                            value.and_then(|v| v.parse().ok()).unwrap_or_else(|| bad(0)),
                        )
                    }
                    "--animation" => {
                        update.animation_ms = Some(
                            value
                                .and_then(|v| settings::parse_animation(&v))
                                .unwrap_or_else(|| bad(1)),
                        )
                    }
                    "--inherit-cwd" => {
                        update.inherit_cwd = Some(
                            value
                                .and_then(|v| settings::parse_switch(&v))
                                .unwrap_or_else(|| bad(3)),
                        )
                    }
                    "--monitor" => {
                        update.monitor = Some(
                            value
                                .filter(|v| !v.is_empty())
                                .map(|v| {
                                    if v.eq_ignore_ascii_case("active") {
                                        String::new()
                                    } else {
                                        v
                                    }
                                })
                                .unwrap_or_else(|| bad(2)),
                        )
                    }
                    _ => {
                        eprintln!("{}\n\n{}", fl!("cli-unknown-argument", argument = flag), usage());
                        std::process::exit(2);
                    }
                }
                next = args.next();
            }
            match settings::save(update) {
                Ok(config) => {
                    let animation = match config.animation_ms {
                        0 => fl!("cli-animation-off"),
                        ms => fl!("cli-animation-ms", ms = ms.to_string()),
                    };
                    println!(
                        "{}",
                        fl!(
                            "cli-summary",
                            height = config.height_percent.to_string(),
                            width = config.width_percent.to_string(),
                            animation = animation,
                            monitor = if config.monitor.is_empty() {
                                fl!("cli-monitor-active")
                            } else {
                                config.monitor.clone()
                            }
                        )
                    );
                    std::process::exit(0);
                }
                Err(err) => {
                    eprintln!("{}", fl!("cli-save-failed", error = err));
                    std::process::exit(1);
                }
            }
        }
        Some("--help") | Some("-h") => {
            print!("{}", usage());
            std::process::exit(0);
        }
        Some(other) => {
            eprintln!("{}\n\n{}", fl!("cli-unknown-argument", argument = other), usage());
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
            eprintln!("{}", fl!("cli-contact-failed", error = err.to_string()));
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
    ContextMenu(pane_grid::Pane, Option<MenuState>),
    CopyLink,
    Dbus(dbus::Command),
    Tick(std::time::Instant),
    QuakeConfig(Box<QuakeConfig>),
    ResizeStart(Edge),
    ResizeMove(Point),
    ResizeEnd,
    HandleHover(Option<Edge>),
    ReorderTab(segmented_button::ReorderEvent),
    SettingsOpen,
    SettingsClose,
    SettingsCommit,
    SettingsReset,
    /// A slider moved (not applied until released).
    Draft(Field, f32),
    AnimationToggle(bool),
    InheritToggle(bool),
    /// Index into `monitor_choices`.
    MonitorSelected(usize),
    Output(OutputEvent, WlOutput),
    /// Text typed into a field.
    FieldInput(Field, String),
    /// Enter pressed in a field.
    FieldSubmit(Field),
    MenuAction(Action),
    OpenLink,
    Key(Modifiers, Key),
    Paste,
    PasteValue(String),
    TermEvent(TerminalId, TermEvent),
    Toggle,
    Zoom(i8),
    NewTab,
    ClosePane,
    Split(pane_grid::Axis),
    TabActivate(segmented_button::Entity),
    TabClose(segmented_button::Entity),
    NextTab,
    PrevTab,
    ActivateTabIndex(usize),
    FocusPane(pane_grid::Direction),
    ToggleMaximize,
    PaneClicked(pane_grid::Pane),
    PaneResized(pane_grid::ResizeEvent),
    PaneDragged(pane_grid::DragEvent),
    ZoomReset,
    /// Sent shortly after the surface is mapped: relax exclusive keyboard grab.
    Settle(window::Id),
    /// First configure or a resize of a layer surface, in logical pixels.
    SurfaceSize(window::Id, cosmic::iced::Size),
}

/// Which edge of the terminal is being grabbed to resize it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Edge {
    Bottom,
    Left,
    Right,
}

/// A numeric setting that is edited with a slider and a text field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Field {
    Height,
    Width,
    Duration,
}

impl Field {
    fn index(self) -> usize {
        self as usize
    }

    fn range(self) -> (u16, u16) {
        match self {
            Field::Height | Field::Width => (settings::MIN_PERCENT, settings::MAX_PERCENT),
            Field::Duration => (settings::MIN_ANIMATION_MS, settings::MAX_ANIMATION_MS),
        }
    }

    fn get(self, config: &QuakeConfig) -> u16 {
        match self {
            Field::Height => config.height_percent,
            Field::Width => config.width_percent,
            Field::Duration => config.animation_ms,
        }
    }

    fn set(self, config: &mut QuakeConfig, value: u16) {
        let (min, max) = self.range();
        let value = value.clamp(min, max);
        match self {
            Field::Height => config.height_percent = value,
            Field::Width => config.width_percent = value,
            Field::Duration => config.animation_ms = value,
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Field::Height | Field::Width => "%",
            Field::Duration => "ms",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlideDir {
    In,
    Out,
}

/// A running slide animation. The Wayland surface stays where it is (below the panel);
/// the content is translated upwards inside it, so it appears to come out from under the
/// panel and the panel is never overdrawn. Nothing is reallocated or re-laid-out.
#[derive(Clone, Copy, Debug)]
struct Slide {
    dir: SlideDir,
    start: std::time::Instant,
    /// Progress when the animation started (0 = hidden above the screen, 1 = shown).
    from: f32,
    ms: u32,
}

/// A connected output as announced by the compositor.
struct OutputEntry {
    output: WlOutput,
    name: String,
}

struct Quake {
    core: Core,
    #[allow(dead_code)]
    config_handler: Option<cosmic_config::Config>,
    config: Config,
    themes: HashMap<(String, ColorSchemeKind), TermColors>,
    term_config: term::Config,
    key_binds: HashMap<KeyBind, Action>,
    /// All tabs; each owns a grid of split terminals.
    tabs: Vec<Tab>,
    /// The tab bar's model; its active entity selects the visible tab.
    tab_model: segmented_button::SingleSelectModel,
    next_terminal_id: TerminalId,
    term_tx: mpsc::UnboundedSender<(TerminalId, TermEvent)>,
    surface: Option<window::Id>,
    /// Open context menu: the pane it belongs to and its position/link.
    context_menu: Option<(pane_grid::Pane, MenuState)>,
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
    /// Connected outputs, kept up to date from the compositor's output events.
    outputs: Vec<OutputEntry>,
    /// Entries of the monitor drop-down: the "active monitor" label, then every output.
    monitor_choices: Vec<String>,
    /// The settings page replaces the terminal while this is set.
    settings_open: bool,
    /// Values shown by the settings page; written to `quake` when a slider is released.
    draft: QuakeConfig,
    /// Duration restored when the animation is switched on again.
    last_animation_ms: u16,
    /// Text of the input fields, indexed by `Field::index` (may be mid-edit).
    field_text: [String; 3],
    slide: Option<Slide>,
    /// 0 = fully above the screen, 1 = fully shown.
    slide_progress: f32,
    /// While dragging (and until the final shrink is acknowledged) the content is laid
    /// out at this size inside a surface that may be larger. The Wayland surface is
    /// resized only twice per drag (grow at the start, shrink at the end) instead of on
    /// every pointer event, which is what made the window flicker.
    display_size: Option<(f32, f32)>,
    /// Edge being dragged.
    resize_edge: Option<Edge>,
    /// For side drags: the surface has grown to the full width, pointer x is meaningful.
    resize_ready: bool,
    /// Width in pixels while dragging (the surface itself may be wider).
    drag_width: f32,
    /// The final shrink was requested and its configure has not arrived yet.
    shrink_pending: bool,
    /// Pointer is over the grab strip.
    handle_hover: Option<Edge>,
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

    fn active_tab(&self) -> Option<&Tab> {
        let entity = self.tab_model.active();
        self.tabs.iter().find(|tab| tab.entity == entity)
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        let entity = self.tab_model.active();
        self.tabs.iter_mut().find(|tab| tab.entity == entity)
    }

    /// The terminal that receives keyboard input.
    fn focused_terminal(&self) -> Option<&Mutex<Terminal>> {
        self.active_tab()?.focused().map(|pane| &pane.terminal)
    }

    fn terminal_by_id(&self, id: TerminalId) -> Option<&Mutex<Terminal>> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.panes.iter())
            .find(|(_, pane)| pane.id == id)
            .map(|(_, pane)| &pane.terminal)
    }

    fn all_terminals(&self) -> impl Iterator<Item = &Mutex<Terminal>> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.panes.iter())
            .map(|(_, pane)| &pane.terminal)
    }

    /// Give the keyboard focus to the focused pane of the visible tab.
    fn focus_task(&self) -> Task<Message> {
        match self.active_tab().and_then(Tab::focused) {
            Some(pane) => widget::text_input::focus(pane.widget_id.clone()),
            None => Task::none(),
        }
    }

    /// Start a shell in a new terminal. `None` if it could not be created.
    fn create_pane(&mut self, cwd: Option<std::path::PathBuf>) -> Option<TermPane> {
        let Some(colors) = self.colors() else {
            log::error!("no terminal color scheme available");
            return None;
        };
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        match Terminal::new(
            id,
            self.term_tx.clone(),
            self.term_config.clone(),
            tty::Options {
                // Without an inherited directory start in $HOME, not wherever the daemon
                // happened to be launched from.
                working_directory: cwd.or_else(|| {
                    std::env::var_os("HOME")
                        .map(std::path::PathBuf::from)
                        .filter(|home| home.is_dir())
                }),
                ..tty::Options::default()
            },
            &self.config,
            colors,
            None,
            None,
        ) {
            Ok(mut terminal) => {
                terminal.set_zoom_adj(self.quake.zoom);
                terminal.set_config(&self.config, &self.themes);
                Some(TermPane {
                    id,
                    terminal: Mutex::new(terminal),
                    widget_id: widget::Id::unique(),
                })
            }
            Err(err) => {
                log::error!("failed to open terminal: {err}");
                None
            }
        }
    }

    /// Directory a new tab or split should start in: the focused terminal's current
    /// directory, unless the setting is off.
    fn inherited_cwd(&self) -> Option<std::path::PathBuf> {
        if !self.quake.inherit_cwd {
            return None;
        }
        self.focused_terminal()?.lock().ok()?.working_directory()
    }

    fn new_tab(&mut self, cwd: Option<std::path::PathBuf>) -> Task<Message> {
        let Some(pane) = self.create_pane(cwd) else {
            return Task::none();
        };
        if let Some(tab) = self.active_tab() {
            tab.unfocus_all();
        }
        let entity = self
            .tab_model
            .insert()
            .text(fl!("tab-title"))
            .closable()
            .activate()
            .id();
        let tab = Tab::new(entity, pane);
        tab.sync_focus();
        self.tabs.push(tab);
        self.focus_task()
    }

    fn split(&mut self, axis: pane_grid::Axis) -> Task<Message> {
        let cwd = self.inherited_cwd();
        let Some(new_pane) = self.create_pane(cwd) else {
            return Task::none();
        };
        if let Some(tab) = self.active_tab_mut() {
            let focus = tab.focus;
            if let Some((pane, _)) = tab.panes.split(axis, focus, new_pane) {
                tab.focus = pane;
                tab.sync_focus();
            }
        }
        self.focus_task()
    }

    /// Close the terminal `id` (shell ended or user closed it). A tab disappears with its
    /// last terminal; when the very last tab goes, a fresh shell is started and the
    /// terminal is hidden.
    fn close_terminal(&mut self, id: TerminalId) -> Task<Message> {
        let Some(index) = self.tabs.iter().position(|tab| tab.pane_of(id).is_some()) else {
            return Task::none();
        };
        self.close_context_menu();
        let tab = &mut self.tabs[index];
        if tab.panes.iter().count() > 1 {
            if let Some(handle) = tab.pane_of(id) {
                if let Some((_closed, sibling)) = tab.panes.close(handle) {
                    if tab.focus == handle {
                        tab.focus = sibling;
                    }
                }
                tab.sync_focus();
            }
            self.focus_task()
        } else {
            self.close_tab_at(index)
        }
    }

    fn close_tab_at(&mut self, index: usize) -> Task<Message> {
        self.close_context_menu();
        let removed = self.tabs.remove(index);
        let was_active = self.tab_model.active() == removed.entity;
        // The neighbour that takes over when the active tab is closed.
        let next = self
            .tabs
            .get(index)
            .or_else(|| index.checked_sub(1).and_then(|i| self.tabs.get(i)))
            .map(|tab| tab.entity);
        self.tab_model.remove(removed.entity);
        drop(removed);
        match next {
            Some(entity) => {
                if was_active {
                    self.tab_model.activate(entity);
                    if let Some(tab) = self.active_tab() {
                        tab.sync_focus();
                    }
                }
                self.focus_task()
            }
            None => {
                // Nothing left: prepare a fresh shell for the next toggle and hide.
                let fresh = self.new_tab(None);
                Task::batch([fresh, self.hide()])
            }
        }
    }

    fn activate_tab(&mut self, entity: segmented_button::Entity) -> Task<Message> {
        if self.tab_model.active() == entity || !self.tabs.iter().any(|t| t.entity == entity) {
            return Task::none();
        }
        self.close_context_menu();
        if let Some(tab) = self.active_tab() {
            tab.unfocus_all();
        }
        self.tab_model.activate(entity);
        if let Some(tab) = self.active_tab() {
            tab.sync_focus();
        }
        self.focus_task()
    }

    /// Switch to the tab `offset` positions away (wraps around).
    fn cycle_tab(&mut self, offset: isize) -> Task<Message> {
        let count = self.tabs.len() as isize;
        let Some(current) = self
            .tabs
            .iter()
            .position(|tab| tab.entity == self.tab_model.active())
        else {
            return Task::none();
        };
        let target = (current as isize + offset).rem_euclid(count) as usize;
        let entity = self.tabs[target].entity;
        self.activate_tab(entity)
    }

    /// Set the font zoom (steps relative to the configured size) for all terminals and
    /// store it. `self.quake` is updated before saving so the config watcher's echo is
    /// a no-op.
    fn apply_zoom(&mut self, zoom: i32) {
        let zoom = zoom.clamp(i32::from(settings::MIN_ZOOM), i32::from(settings::MAX_ZOOM)) as i8;
        if zoom == self.quake.zoom {
            return;
        }
        self.quake.zoom = zoom;
        self.draft.zoom = zoom;
        self.apply_zoom_to_terminals();
        if let Some(handler) = &self.quake_handler {
            let mut stored = self.quake.clone();
            if let Err(err) = stored.set_zoom(handler, zoom) {
                log::warn!("failed to save zoom: {err}");
            }
        }
    }

    fn apply_zoom_to_terminals(&self) {
        for terminal in self.all_terminals() {
            let mut terminal = terminal.lock().unwrap();
            terminal.set_zoom_adj(self.quake.zoom);
            terminal.set_config(&self.config, &self.themes);
        }
    }

    /// Name a tab after the title its focused terminal reports (e.g. the running program).
    fn set_tab_title(&mut self, id: TerminalId, title: String) {
        let entity = self
            .tabs
            .iter()
            .find(|tab| tab.focused().is_some_and(|pane| pane.id == id))
            .map(|tab| tab.entity);
        if let Some(entity) = entity {
            self.tab_model.text_set(entity, title);
        }
    }

    fn focus_pane(&mut self, pane: pane_grid::Pane) -> Task<Message> {
        if let Some(tab) = self.active_tab_mut() {
            if tab.panes.get(pane).is_some() && tab.focus != pane {
                tab.focus = pane;
                tab.sync_focus();
            }
        }
        self.focus_task()
    }

    /// Remember a new or changed output and refresh everything derived from the list.
    fn update_output(&mut self, event: OutputEvent, output: WlOutput) {
        match event {
            OutputEvent::Created(Some(info)) | OutputEvent::InfoUpdate(info) => {
                let name = info
                    .name
                    .clone()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| format!("{} {}", info.make, info.model).trim().to_string());
                match self.outputs.iter_mut().find(|e| e.output == output) {
                    Some(entry) => entry.name = name,
                    None => self.outputs.push(OutputEntry { output, name }),
                }
            }
            OutputEvent::Created(None) => {}
            OutputEvent::Removed => self.outputs.retain(|e| e.output != output),
        }
        self.outputs.sort_by(|a, b| a.name.cmp(&b.name));
        self.monitor_choices = std::iter::once(fl!("settings-monitor-active"))
            .chain(self.outputs.iter().map(|e| e.name.clone()))
            .collect();
        if let Ok(mut names) = dbus::MONITOR_NAMES.lock() {
            *names = self.outputs.iter().map(|e| e.name.clone()).collect();
        }
    }

    /// The output to map the surface on: the configured one if it is connected,
    /// otherwise whatever the compositor considers the active output.
    fn selected_output(&self) -> IcedOutput {
        self.outputs
            .iter()
            .find(|e| !self.quake.monitor.is_empty() && e.name == self.quake.monitor)
            .map_or(IcedOutput::Active, |e| IcedOutput::Output(e.output.clone()))
    }

    /// Make the text fields show the draft values again.
    fn sync_field_text(&mut self) {
        for field in [Field::Height, Field::Width, Field::Duration] {
            let value = match field {
                // While the animation is off the field shows the duration it would use.
                Field::Duration if self.draft.animation_ms == 0 => self.last_animation_ms,
                _ => field.get(&self.draft),
            };
            self.field_text[field.index()] = value.to_string();
        }
    }

    /// Anchor for a surface of the given width (`None` = full width).
    fn anchor_for(width: Option<u32>) -> Anchor {
        if width.is_some() {
            Anchor::TOP
        } else {
            Anchor::TOP | Anchor::LEFT | Anchor::RIGHT
        }
    }

    /// Store `new`, make it the active configuration and re-fit a visible surface.
    /// `self.quake` is updated before saving so the config watcher's echo is a no-op.
    fn commit_quake(&mut self, new: QuakeConfig) -> Task<Message> {
        let old = std::mem::replace(&mut self.quake, new.clone());
        self.draft = new.clone();
        if new.animation_ms > 0 {
            self.last_animation_ms = new.animation_ms;
        }
        self.sync_field_text();
        if let Some(handler) = &self.quake_handler {
            let mut stored = old.clone();
            let results = [
                (new.height_percent != old.height_percent)
                    .then(|| stored.set_height_percent(handler, new.height_percent)),
                (new.width_percent != old.width_percent)
                    .then(|| stored.set_width_percent(handler, new.width_percent)),
                (new.animation_ms != old.animation_ms)
                    .then(|| stored.set_animation_ms(handler, new.animation_ms)),
                (new.monitor != old.monitor)
                    .then(|| stored.set_monitor(handler, new.monitor.clone())),
                (new.inherit_cwd != old.inherit_cwd)
                    .then(|| stored.set_inherit_cwd(handler, new.inherit_cwd)),
            ];
            for err in results.into_iter().flatten().filter_map(Result::err) {
                log::warn!("failed to save settings: {err}");
            }
        }
        // A different monitor needs a new surface on that output.
        if new.monitor != old.monitor && self.surface.is_some() {
            let hide = self.hide_now();
            let show = self.show();
            return Task::batch([hide, show]);
        }
        let resized = new.height_percent != old.height_percent
            || new.width_percent != old.width_percent;
        let (Some(id), Some(output)) = (self.surface, self.output_size) else {
            return Task::none();
        };
        if !resized || self.measuring || self.resizing || self.slide.is_some() {
            return Task::none();
        }
        let (width, height) = self.quake.target_size(output);
        self.surface_width = width;
        self.surface_height = height;
        Task::batch([
            set_anchor(id, Self::anchor_for(width)),
            set_size(id, width, Some(height)),
            self.blur_task(id, height as f32),
        ])
    }

    fn animation_ms(&self) -> u32 {
        u32::from(self.quake.animation_ms)
    }

    /// Is the terminal on screen and not on its way out?
    fn is_shown(&self) -> bool {
        self.surface.is_some() && !matches!(self.slide, Some(Slide { dir: SlideDir::Out, .. }))
    }

    fn start_slide(&mut self, dir: SlideDir) {
        self.slide = Some(Slide {
            dir,
            start: std::time::Instant::now(),
            from: self.slide_progress,
            ms: self.animation_ms().max(1),
        });
    }

    fn show(&mut self) -> Task<Message> {
        if let Some(id) = self.surface {
            // Shown again while sliding out: turn around instead of doing nothing.
            if matches!(self.slide, Some(Slide { dir: SlideDir::Out, .. })) {
                self.start_slide(SlideDir::In);
                return Task::batch([
                    set_keyboard_interactivity(id, KeyboardInteractivity::OnDemand),
                    self.focus_task(),
                ]);
            }
            return Task::none();
        }
        let id = window::Id::unique();
        self.surface = Some(id);
        self.measuring = true;
        self.slide = None;
        self.slide_progress = if self.animation_ms() > 0 { 0.0 } else { 1.0 };
        Task::batch([
            // Layer-shell only knows pixels. To turn percentages into pixels we first
            // map an invisible surface anchored to all edges: the compositor sizes it
            // to the active output, and `Message::SurfaceSize` then shrinks it to the
            // configured size. It only exists for a moment and draws nothing.
            get_layer_surface(SctkLayerSurfaceSettings {
                id,
                // `Top`, like the panel: popups of panel applets are stacked above this
                // layer and stay visible over the terminal. (`Overlay` would cover them,
                // but would also show over fullscreen windows.)
                layer: Layer::Top,
                // Exclusive so the compositor focuses us right away; relaxed to
                // OnDemand in `Message::Settle` so other windows stay clickable.
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                output: self.selected_output(),
                namespace: "cosmic-quake-term".into(),
                size: Some((None, None)),
                size_limits: cosmic::iced::core::layout::Limits::NONE
                    .min_width(1.0)
                    .min_height(1.0)
                    .max_width(16384.0)
                    .max_height(16384.0),
                ..Default::default()
            }),
            self.focus_task(),
            Task::perform(
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    id
                },
                |id| cosmic::Action::App(Message::Settle(id)),
            ),
        ])
    }

    /// Hide the terminal, sliding it out first when animations are on.
    fn hide(&mut self) -> Task<Message> {
        let Some(id) = self.surface else {
            return Task::none();
        };
        if self.measuring || self.resizing || self.animation_ms() == 0 {
            return self.hide_now();
        }
        if matches!(self.slide, Some(Slide { dir: SlideDir::Out, .. })) {
            return Task::none();
        }
        self.start_slide(SlideDir::Out);
        self.close_context_menu();
        // Hand the keyboard back right away; the surface is only a ghost from now on.
        set_keyboard_interactivity(id, KeyboardInteractivity::None)
    }

    /// Destroy the surface immediately, without animation.
    fn hide_now(&mut self) -> Task<Message> {
        self.slide = None;
        self.slide_progress = 1.0;
        self.measuring = false;
        self.resizing = false;
        self.display_size = None;
        self.resize_edge = None;
        self.shrink_pending = false;
        self.handle_hover = None;
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
        let width = self
            .surface_width
            .map(|w| w as f32)
            .or(self.output_size.map(|(w, _)| w))
            .unwrap_or(0.0);
        self.blur_rect(id, 0.0, width, height)
    }

    /// Blur the rectangle `x`, 0, `width` x `height` of the surface. The terminal is
    /// centered in the surface while a side edge is dragged, so `x` is not always 0.
    fn blur_rect(&self, id: window::Id, x: f32, width: f32, height: f32) -> Task<Message> {
        if !self.frosted() {
            return Task::none();
        }
        blur(
            id,
            Some(vec![Rectangle::new(Point::new(x, 0.0), Size::new(width, height))]),
        )
        .map(|()| cosmic::Action::None)
    }

    /// Put thin grab strips on the left, right and bottom edge; dragging one resizes the
    /// terminal. At rest they are painted like the terminal; they light up on hover.
    fn with_resize_handle<'a>(&self, content: Element<'a, Message>) -> Element<'a, Message> {
        const HANDLE: f32 = 6.0;
        let idle = self
            .focused_terminal()
            .map(|terminal| {
                let terminal = terminal.lock().unwrap();
                let bg = terminal.metadata_set[terminal.default_attrs().metadata].bg;
                Color::from_rgba8(bg.r(), bg.g(), bg.b(), self.effective_opacity())
            })
            .unwrap_or(Color::TRANSPARENT);

        let strip = |edge: Edge| -> Element<'a, Message> {
            let highlighted = self.handle_hover == Some(edge) || self.resize_edge == Some(edge);
            let (width, height, cursor) = match edge {
                Edge::Bottom => (
                    Length::Fill,
                    Length::Fixed(HANDLE),
                    mouse::Interaction::ResizingVertically,
                ),
                Edge::Left | Edge::Right => (
                    Length::Fixed(HANDLE),
                    Length::Fill,
                    mouse::Interaction::ResizingHorizontally,
                ),
            };
            let visual = widget::container(widget::space::vertical())
                .width(width)
                .height(height)
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
            widget::mouse_area(visual)
                .on_enter(Message::HandleHover(Some(edge)))
                .on_exit(Message::HandleHover(None))
                .on_press(Message::ResizeStart(edge))
                .interaction(cursor)
                .into()
        };

        let middle = widget::row::with_children(vec![
            strip(Edge::Left),
            widget::container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            strip(Edge::Right),
        ])
        .width(Length::Fill)
        .height(Length::Fill);
        let column = widget::column::with_children(vec![middle.into(), strip(Edge::Bottom)])
            .width(Length::Fill)
            .height(Length::Fill);

        let framed: Element<'a, Message> = match self.display_size {
            // The surface is larger than the terminal while dragging: draw the terminal
            // at its size, centered at the top, and leave the rest transparent.
            Some((width, height)) => widget::container(
                widget::container(column)
                    .width(Length::Fixed(width))
                    .height(Length::Fixed(height)),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(cosmic::iced::Alignment::Center)
            .into(),
            None => column.into(),
        };
        // Slide animation: translate the content up by the part that is still hidden.
        let hidden = (1.0 - self.slide_progress.clamp(0.0, 1.0)) * self.surface_height as f32;
        slide::SlideIn::new(framed, -hidden).into()
    }

    /// The settings page: every option the program offers. A slider gives quick changes,
    /// the text field exact values; both are applied on release / Enter.
    fn settings_view(&self) -> Element<'_, Message> {
        let theme = cosmic::theme::active();
        let cosmic = theme.cosmic();
        let draft = &self.draft;
        let animated = draft.animation_ms > 0;
        let spacing = cosmic.space_s();

        let field_row = |label: String, field: Field| {
            let (min, max) = field.range();
            widget::settings::item(
                label,
                widget::row::with_children(vec![
                    widget::slider(
                        f32::from(min)..=f32::from(max),
                        f32::from(field.get(draft)),
                        move |value| Message::Draft(field, value),
                    )
                    .step(1.0_f32)
                    .on_release(Message::SettingsCommit)
                    .width(Length::Fixed(200.0))
                    .into(),
                    widget::text_input(field.unit(), &self.field_text[field.index()])
                        .on_input(move |text| Message::FieldInput(field, text))
                        .on_submit(move |_| Message::FieldSubmit(field))
                        .width(Length::Fixed(76.0))
                        .into(),
                    widget::text(field.unit()).width(Length::Fixed(24.0)).into(),
                ])
                .spacing(spacing)
                .align_y(cosmic::iced::Alignment::Center),
            )
        };

        let size = widget::settings::section()
            .title(fl!("settings-size"))
            .add(field_row(fl!("settings-height"), Field::Height))
            .add(field_row(fl!("settings-width"), Field::Width));

        let selected = if draft.monitor.is_empty() {
            Some(0)
        } else {
            self.monitor_choices.iter().position(|name| *name == draft.monitor)
        };
        let monitor = widget::settings::section()
            .title(fl!("settings-monitor"))
            .add(widget::settings::item(
                fl!("settings-monitor"),
                widget::dropdown(&self.monitor_choices, selected, Message::MonitorSelected),
            ));

        let terminals = widget::settings::section()
            .title(fl!("settings-terminals"))
            .add(widget::settings::item(
                fl!("settings-inherit-cwd"),
                widget::toggler(draft.inherit_cwd).on_toggle(Message::InheritToggle),
            ));

        let mut animation = widget::settings::section()
            .title(fl!("settings-animation"))
            .add(widget::settings::item(
                fl!("settings-slide-animation"),
                widget::toggler(animated).on_toggle(Message::AnimationToggle),
            ));
        if animated {
            animation = animation.add(field_row(fl!("settings-duration"), Field::Duration));
        }

        let header = widget::row::with_children(vec![
            widget::text::title3(fl!("settings-title")).into(),
            widget::space::horizontal().into(),
            widget::button::icon(widget::icon::from_name("window-close-symbolic"))
                .on_press(Message::SettingsClose)
                .into(),
        ])
        .align_y(cosmic::iced::Alignment::Center);

        let page = widget::column::with_children(vec![
            header.into(),
            size.into(),
            monitor.into(),
            terminals.into(),
            animation.into(),
            widget::button::standard(fl!("settings-reset"))
                .on_press(Message::SettingsReset)
                .into(),
        ])
        .spacing(cosmic.space_m())
        .padding(cosmic.space_m())
        .max_width(600.0);

        let background = Color::from(cosmic.background(self.frosted()).base);
        widget::container(widget::scrollable(page))
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(cosmic::iced::Alignment::Center)
            .class(cosmic::theme::Container::custom(move |_| {
                widget::container::Style {
                    background: Some(background.into()),
                    ..Default::default()
                }
            }))
            .into()
    }

    /// The tab strip (shown when there is more than one tab) with a "new tab" button.
    fn tab_bar(&self) -> Element<'_, Message> {
        let theme = cosmic::theme::active();
        let cosmic = theme.cosmic();
        let background = Color::from(cosmic.background(self.frosted()).base);
        let bar = widget::tab_bar::horizontal(&self.tab_model)
            .button_height(32)
            .button_spacing(cosmic.space_xxs())
            .enable_tab_drag(String::from("x-cosmic-quake-term/tab"))
            .on_reorder(Message::ReorderTab)
            .tab_drag_threshold(25.0)
            .on_activate(Message::TabActivate)
            .on_close(Message::TabClose);
        let add = widget::button::icon(widget::icon::from_name("list-add-symbolic"))
            .on_press(Message::NewTab);
        widget::container(
            widget::row::with_children(vec![
                widget::container(bar).width(Length::Fill).into(),
                add.into(),
            ])
            .align_y(cosmic::iced::Alignment::Center),
        )
        .width(Length::Fill)
        .class(cosmic::theme::Container::custom(move |_| {
            widget::container::Style {
                background: Some(background.into()),
                ..Default::default()
            }
        }))
        .into()
    }

    fn close_context_menu(&mut self) {
        self.context_menu = None;
        for terminal in self.all_terminals() {
            terminal.lock().unwrap().context_menu = None;
        }
    }

    fn run_action(&mut self, action: Action) -> Task<Message> {
        match action {
            Action::Copy => {
                if let Some(terminal) = self.focused_terminal() {
                    let terminal = terminal.lock().unwrap();
                    let term = terminal.term.lock();
                    if let Some(text) = term.selection_to_string() {
                        return clipboard::write(text);
                    }
                }
                Task::none()
            }
            Action::Paste => self.update(Message::Paste),
            Action::Settings => self.update(Message::SettingsOpen),
            Action::SelectAll => {
                if let Some(terminal) = self.focused_terminal() {
                    let mut terminal = terminal.lock().unwrap();
                    terminal.select_all();
                    terminal.needs_update = true;
                }
                Task::none()
            }
            Action::NewTab => self.update(Message::NewTab),
            Action::ClosePane => self.update(Message::ClosePane),
            Action::SplitHorizontal => self.update(Message::Split(pane_grid::Axis::Horizontal)),
            Action::SplitVertical => self.update(Message::Split(pane_grid::Axis::Vertical)),
            Action::NextTab => self.update(Message::NextTab),
            Action::PrevTab => self.update(Message::PrevTab),
            Action::ActivateTab(index) => self.update(Message::ActivateTabIndex(index)),
            Action::FocusLeft => self.update(Message::FocusPane(pane_grid::Direction::Left)),
            Action::FocusRight => self.update(Message::FocusPane(pane_grid::Direction::Right)),
            Action::FocusUp => self.update(Message::FocusPane(pane_grid::Direction::Up)),
            Action::FocusDown => self.update(Message::FocusPane(pane_grid::Direction::Down)),
            Action::ToggleMaximize => self.update(Message::ToggleMaximize),
            Action::ZoomIn => self.update(Message::Zoom(1)),
            Action::ZoomOut => self.update(Message::Zoom(-1)),
            Action::ZoomReset => self.update(Message::ZoomReset),
        }
    }
}

/// Receiver for terminal events. Created in `init` (so the shell can start before
/// the first frame) and taken over by the event subscription.
static TERM_EVENT_RX: Mutex<Option<mpsc::UnboundedReceiver<(TerminalId, TermEvent)>>> =
    Mutex::new(None);

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
        // Terminal events carry the id of the terminal they belong to.
        let (term_tx, rx) = mpsc::unbounded_channel();
        *TERM_EVENT_RX.lock().unwrap() = Some(rx);
        let mut app = Quake {
            core,
            config_handler: flags.config_handler,
            config: flags.config,
            themes: HashMap::new(),
            term_config: term::Config::default(),
            key_binds: default_key_binds(),
            tabs: Vec::new(),
            tab_model: segmented_button::ModelBuilder::default().build(),
            next_terminal_id: 0,
            term_tx,
            surface: None,
            context_menu: None,
            quake: flags.quake.clone(),
            measuring: false,
            quake_handler: flags.quake_handler,
            output_size: None,
            surface_width: None,
            surface_height: 0,
            resizing: false,
            outputs: Vec::new(),
            monitor_choices: vec![fl!("settings-monitor-active")],
            settings_open: false,
            field_text: Default::default(),
            draft: flags.quake.clone(),
            last_animation_ms: if flags.quake.animation_ms > 0 {
                flags.quake.animation_ms
            } else {
                settings::DEFAULT_ANIMATION_MS
            },
            slide: None,
            slide_progress: 1.0,
            display_size: None,
            resize_edge: None,
            resize_ready: false,
            drag_width: 0.0,
            shrink_pending: false,
            handle_hover: None,
        };
        app.update_theme_state();
        app.sync_field_text();
        // Start the first shell right away so the very first frame already shows it.
        let _ = app.new_tab(None);
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
                    for terminal in self.all_terminals() {
                        terminal
                            .lock()
                            .unwrap()
                            .set_config(&self.config, &self.themes);
                    }
                    return cosmic::command::set_theme(self.config.app_theme.theme());
                }
            }
            Message::ContextMenu(pane, menu_state) => {
                self.close_context_menu();
                if let Some(state) = menu_state {
                    // A right click focuses the pane it happened in.
                    let focus = self.focus_pane(pane);
                    if let Some(terminal) = self.focused_terminal() {
                        terminal.lock().unwrap().context_menu = Some(state.clone());
                    }
                    self.context_menu = Some((pane, state));
                    return focus;
                }
            }
            Message::MenuAction(action) => {
                self.close_context_menu();
                return self.run_action(action);
            }
            Message::OpenLink => {
                let link = self.context_menu.as_ref().and_then(|(_, m)| m.link.clone());
                self.close_context_menu();
                if let Some(link) = link {
                    if let Err(err) = open::that_detached(&link) {
                        log::warn!("failed to open {link:?}: {err}");
                    }
                }
            }
            Message::CopyLink => {
                let link = self.context_menu.as_ref().and_then(|(_, m)| m.link.clone());
                self.close_context_menu();
                if let Some(link) = link {
                    return clipboard::write(link);
                }
            }
            Message::Key(_, Key::Named(Named::Escape)) if self.settings_open => {
                return self.update(Message::SettingsClose);
            }
            // The settings page has its own inputs; shortcuts must not act on the
            // terminal hidden behind it (e.g. pasting into it).
            Message::Key(..) if self.settings_open => {}
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
                if let Some(terminal) = self.focused_terminal() {
                    terminal.lock().unwrap().paste(value);
                }
            }
            Message::TermEvent(id, event) => match event {
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
                    if let Some(terminal) = self.terminal_by_id(id) {
                        let terminal = terminal.lock().unwrap();
                        let rgb = terminal.colors()[index].unwrap_or_default();
                        terminal.input_no_scroll(f(rgb).into_bytes());
                    }
                }
                TermEvent::PtyWrite(text) => {
                    if let Some(terminal) = self.terminal_by_id(id) {
                        terminal.lock().unwrap().input_no_scroll(text.into_bytes());
                    }
                }
                TermEvent::TextAreaSizeRequest(f) => {
                    if let Some(terminal) = self.terminal_by_id(id) {
                        let terminal = terminal.lock().unwrap();
                        terminal.input_no_scroll(f(terminal.size().into()).into_bytes());
                    }
                }
                TermEvent::MouseCursorDirty | TermEvent::Wakeup => {
                    if let Some(terminal) = self.terminal_by_id(id) {
                        terminal.lock().unwrap().needs_update = true;
                    }
                }
                TermEvent::Title(title) => self.set_tab_title(id, title),
                TermEvent::ResetTitle => self.set_tab_title(id, fl!("tab-title")),
                TermEvent::Exit => {
                    // The shell of this terminal ended (e.g. `exit`).
                    return self.close_terminal(id);
                }
                _ => {}
            },
            Message::NewTab => {
                let cwd = self.inherited_cwd();
                return self.new_tab(cwd);
            }
            Message::ClosePane => {
                let id = self.active_tab().and_then(Tab::focused).map(|pane| pane.id);
                if let Some(id) = id {
                    return self.close_terminal(id);
                }
            }
            Message::Split(axis) => return self.split(axis),
            Message::TabActivate(entity) => return self.activate_tab(entity),
            Message::TabClose(entity) => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.entity == entity) {
                    return self.close_tab_at(index);
                }
            }
            Message::NextTab => return self.cycle_tab(1),
            Message::PrevTab => return self.cycle_tab(-1),
            Message::ActivateTabIndex(index) => {
                if let Some(entity) = self.tabs.get(index).map(|tab| tab.entity) {
                    return self.activate_tab(entity);
                }
            }
            Message::FocusPane(direction) => {
                let adjacent = self
                    .active_tab()
                    .and_then(|tab| tab.panes.adjacent(tab.focus, direction));
                if let Some(pane) = adjacent {
                    return self.focus_pane(pane);
                }
            }
            Message::ToggleMaximize => {
                if let Some(tab) = self.active_tab_mut() {
                    if tab.panes.maximized().is_some() {
                        tab.panes.restore();
                    } else {
                        let focus = tab.focus;
                        tab.panes.maximize(focus);
                    }
                }
                return self.focus_task();
            }
            Message::PaneClicked(pane) => return self.focus_pane(pane),
            Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.panes.resize(split, ratio);
                }
            }
            Message::PaneDragged(pane_grid::DragEvent::Dropped { pane, target }) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.panes.drop(pane, target);
                }
            }
            Message::PaneDragged(_) => {}
            Message::Dbus(command) => {
                return match command {
                    dbus::Command::Toggle => self.update(Message::Toggle),
                    dbus::Command::Show => self.show(),
                    dbus::Command::Hide => self.hide(),
                    dbus::Command::Quit => cosmic::iced::exit(),
                    dbus::Command::NewTab => {
                        let show = self.show();
                        let cwd = self.inherited_cwd();
                        return Task::batch([show, self.new_tab(cwd)]);
                    }
                    dbus::Command::SplitHorizontal => {
                        let show = self.show();
                        return Task::batch([show, self.split(pane_grid::Axis::Horizontal)]);
                    }
                    dbus::Command::SplitVertical => {
                        let show = self.show();
                        return Task::batch([show, self.split(pane_grid::Axis::Vertical)]);
                    }
                };
            }
            Message::QuakeConfig(config) => {
                if self.quake != *config {
                    let zoom_changed = self.quake.zoom != config.zoom;
                    let resized = self.quake.height_percent != config.height_percent
                        || self.quake.width_percent != config.width_percent
                        || self.quake.monitor != config.monitor;
                    self.quake = *config;
                    if zoom_changed {
                        self.apply_zoom_to_terminals();
                    }
                    // Re-map the surface so the new size takes effect immediately.
                    // (Animation settings are simply picked up on the next show/hide.)
                    if resized && self.surface.is_some() {
                        let hide = self.hide_now();
                        let show = self.show();
                        return Task::batch([hide, show]);
                    }
                }
            }
            Message::SurfaceSize(id, size)
                if self.shrink_pending && !self.resizing && self.surface == Some(id) =>
            {
                // Ignore configures that are still about the earlier grow.
                let wanted_width = self
                    .surface_width
                    .map(|w| w as f32)
                    .or(self.output_size.map(|(w, _)| w))
                    .unwrap_or(size.width);
                if (size.height - self.surface_height as f32).abs() < 1.5
                    && (size.width - wanted_width).abs() < 1.5
                {
                    self.shrink_pending = false;
                    self.display_size = None;
                    // The surface has its final size: the terminal starts at x = 0 again.
                    return self.blur_task(id, self.surface_height as f32);
                }
            }
            Message::SurfaceSize(id, size)
                if self.resizing
                    && !self.resize_ready
                    && self.surface == Some(id)
                    && self
                        .output_size
                        .is_some_and(|(output_w, _)| size.width >= output_w - 1.5) =>
            {
                // The surface now spans the full width: pointer x is the real position, and
                // the terminal sits in the middle of it, so the blur has to move there.
                self.resize_ready = true;
                if let Some((output_w, _)) = self.output_size {
                    let x = ((output_w - self.drag_width) / 2.0).max(0.0);
                    return self.blur_rect(id, x, self.drag_width, self.surface_height as f32);
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
                    let anchor = Self::anchor_for(width);
                    // With animation the content starts fully pushed out of the surface;
                    // `Message::Tick` then slides it in.
                    let visible = if self.animation_ms() > 0 {
                        self.slide_progress = 0.0;
                        self.start_slide(SlideDir::In);
                        1.0
                    } else {
                        height as f32
                    };
                    return Task::batch([
                        set_anchor(id, anchor),
                        set_size(id, width, Some(height)),
                        self.blur_task(id, visible),
                        self.focus_task(),
                    ]);
                }
            }
            Message::ResizeStart(edge) => {
                if let (Some(id), false, true, Some((output_w, output_h))) =
                    (self.surface, self.measuring, self.slide.is_none(), self.output_size)
                {
                    self.resizing = true;
                    self.resize_edge = Some(edge);
                    self.shrink_pending = false;
                    let width = self.surface_width.map_or(output_w, |w| w as f32);
                    self.drag_width = width;
                    // Keep showing the content at its current size, then grow the surface
                    // once: to the full height for the bottom edge, to the full width for
                    // the side edges. While dragging only the layout changes.
                    self.display_size = Some((width, self.surface_height as f32));
                    return match edge {
                        Edge::Bottom => {
                            self.resize_ready = true;
                            set_size(id, self.surface_width, Some(output_h.ceil() as u32))
                        }
                        Edge::Left | Edge::Right => {
                            // Already full width: no configure will follow, ready now.
                            self.resize_ready = self.surface_width.is_none();
                            Task::batch([
                                set_anchor(id, Anchor::TOP | Anchor::LEFT | Anchor::RIGHT),
                                set_size(id, None, Some(self.surface_height)),
                            ])
                        }
                    };
                }
            }
            Message::ResizeMove(point) => {
                let (true, Some(edge), Some((output_w, output_h)), Some(id)) =
                    (self.resizing, self.resize_edge, self.output_size, self.surface)
                else {
                    return Task::none();
                };
                let min = f32::from(settings::MIN_PERCENT) / 100.0;
                match edge {
                    // Anchored to the top: the pointer's y inside the surface is the height.
                    Edge::Bottom => {
                        self.surface_height =
                            point.y.clamp(output_h * min, output_h).round() as u32;
                    }
                    // Centered: the distance from the middle is half the width.
                    Edge::Left | Edge::Right => {
                        if !self.resize_ready {
                            return Task::none();
                        }
                        self.drag_width = (2.0 * (point.x - output_w / 2.0).abs())
                            .clamp(output_w * min, output_w)
                            .round();
                    }
                }
                let height = self.surface_height as f32;
                self.display_size = Some((self.drag_width, height));
                // The surface is as wide as the output for side drags, otherwise as wide as
                // the terminal; the terminal is centered in it.
                let surface_width = match edge {
                    Edge::Bottom => self.surface_width.map_or(output_w, |w| w as f32),
                    Edge::Left | Edge::Right => output_w,
                };
                let x = ((surface_width - self.drag_width) / 2.0).max(0.0);
                return self.blur_rect(id, x, self.drag_width, height);
            }
            Message::ResizeEnd => {
                if !self.resizing {
                    return Task::none();
                }
                self.resizing = false;
                let edge = self.resize_edge.take();
                let (Some(id), Some(output)) = (self.surface, self.output_size) else {
                    return Task::none();
                };
                // Save the dragged dimension, then snap to the stored percentages so what
                // you see is what a fresh start will give. `self.quake` is updated first
                // so the config watcher's echo is a no-op and does not re-map the surface.
                let mut stored = self.quake.clone();
                let mut errors = Vec::new();
                if edge == Some(Edge::Bottom) {
                    let percent = settings::clamp_percent(
                        (self.surface_height as f32 / output.1 * 100.0).round() as u16,
                    );
                    self.quake.height_percent = percent;
                    self.draft.height_percent = percent;
                    if let Some(handler) = &self.quake_handler {
                        errors.extend(stored.set_height_percent(handler, percent).err());
                    }
                } else if edge.is_some() {
                    let percent = settings::clamp_percent(
                        (self.drag_width / output.0 * 100.0).round() as u16,
                    );
                    self.quake.width_percent = percent;
                    self.draft.width_percent = percent;
                    if let Some(handler) = &self.quake_handler {
                        errors.extend(stored.set_width_percent(handler, percent).err());
                    }
                }
                for err in errors {
                    log::warn!("failed to save size: {err}");
                }
                self.sync_field_text();
                let (width, height) = self.quake.target_size(output);
                self.surface_width = width;
                self.surface_height = height;
                // Keep the fixed-size layout until the surface has really shrunk,
                // otherwise the terminal would stretch over the large surface for a frame.
                self.display_size = Some((width.map_or(output.0, |w| w as f32), height as f32));
                self.shrink_pending = true;
                // The blur keeps its dragged rectangle until the surface has really shrunk
                // (see the `shrink_pending` arm of `SurfaceSize`), otherwise it would sit
                // at the wrong place for a frame.
                return Task::batch([
                    set_anchor(id, Self::anchor_for(width)),
                    set_size(id, width, Some(height)),
                ]);
            }
            Message::HandleHover(edge) => self.handle_hover = edge,
            Message::ReorderTab(event) => {
                let _ = self
                    .tab_model
                    .reorder(event.dragged, event.target, event.position);
                // Keep the tab list in the order the tab bar shows.
                let model = &self.tab_model;
                self.tabs
                    .sort_by_key(|tab| model.position(tab.entity).unwrap_or(u16::MAX));
            }
            Message::SettingsOpen => {
                self.close_context_menu();
                self.draft = self.quake.clone();
                self.settings_open = true;
                self.sync_field_text();
            }
            Message::SettingsClose => {
                self.settings_open = false;
                // The terminal widget was out of the tree; give it the focus back.
                return self.focus_task();
            }
            Message::Draft(field, value) => {
                field.set(&mut self.draft, value.round() as u16);
                self.sync_field_text();
            }
            Message::FieldInput(field, text) => {
                // Only digits; the value is validated and applied on Enter.
                self.field_text[field.index()] =
                    text.chars().filter(char::is_ascii_digit).take(4).collect();
            }
            Message::FieldSubmit(field) => {
                match self.field_text[field.index()].parse::<u16>() {
                    Ok(value) => {
                        let mut new = self.draft.clone();
                        field.set(&mut new, value);
                        return self.commit_quake(new);
                    }
                    // Empty or unparsable: show the current value again.
                    Err(_) => self.sync_field_text(),
                }
            }
            Message::Output(event, output) => self.update_output(event, output),
            Message::MonitorSelected(index) => {
                let mut new = self.draft.clone();
                new.monitor = match index {
                    0 => String::new(),
                    _ => self.monitor_choices.get(index).cloned().unwrap_or_default(),
                };
                return self.commit_quake(new);
            }
            Message::InheritToggle(on) => {
                let mut new = self.draft.clone();
                new.inherit_cwd = on;
                return self.commit_quake(new);
            }
            Message::AnimationToggle(on) => {
                let mut new = self.draft.clone();
                new.animation_ms = if on { self.last_animation_ms } else { 0 };
                return self.commit_quake(new);
            }
            Message::SettingsCommit => {
                let new = self.draft.clone();
                return self.commit_quake(new);
            }
            Message::SettingsReset => {
                return self.commit_quake(QuakeConfig::default());
            }
            Message::Tick(now) => {
                let (Some(slide), Some(id)) = (self.slide, self.surface) else {
                    self.slide = None;
                    return Task::none();
                };
                let elapsed_ms = now.saturating_duration_since(slide.start).as_secs_f32() * 1000.0;
                let u = (elapsed_ms / slide.ms as f32).clamp(0.0, 1.0);
                let eased = 1.0 - (1.0 - u).powi(3); // ease-out cubic
                let target = if slide.dir == SlideDir::In { 1.0 } else { 0.0 };
                self.slide_progress = slide.from + (target - slide.from) * eased;
                if u >= 1.0 {
                    self.slide = None;
                    return match slide.dir {
                        SlideDir::In => self.blur_task(id, self.surface_height as f32),
                        SlideDir::Out => self.hide_now(),
                    };
                }
                // The blur must follow the visible part of the sliding content.
                let visible = (self.slide_progress * self.surface_height as f32).max(1.0);
                return self.blur_task(id, visible);
            }
            Message::Settle(id) => {
                if self.surface == Some(id) && self.is_shown() {
                    return Task::batch([
                        self.focus_task(),
                        set_keyboard_interactivity(id, KeyboardInteractivity::OnDemand),
                    ]);
                }
            }
            Message::Toggle => {
                return if self.is_shown() {
                    self.hide()
                } else {
                    self.show()
                };
            }
            // Zoom is one global value: it applies to every terminal and is remembered
            // for the next start.
            Message::Zoom(delta) => self.apply_zoom(i32::from(self.quake.zoom) + i32::from(delta)),
            Message::ZoomReset => self.apply_zoom(0),
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
        let Some(tab) = self.active_tab() else {
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
        if self.settings_open {
            return self.with_resize_handle(self.settings_view());
        }
        let space_xxs = cosmic::theme::active().cosmic().space_xxs();

        let grid = widget::pane_grid::PaneGrid::new(&tab.panes, |pane, data, _maximized| {
            let mut terminal_box = terminal_box(&data.terminal, &self.key_binds)
                .id(data.widget_id.clone())
                .on_context_menu(move |menu_state| Message::ContextMenu(pane, menu_state))
                .on_zoom(Message::Zoom)
                .on_middle_double_click(|| Message::ZoomReset)
                .opacity(self.effective_opacity())
                .padding(space_xxs)
                .sharp_corners(true)
                .show_headerbar(false);

            let menu = self.context_menu.as_ref().filter(|(menu_pane, _)| *menu_pane == pane);
            let element: Element<'_, Message> = match menu {
                None => terminal_box.into(),
                Some((_, menu_state)) => {
                    let point = menu_state
                        .local_position
                        .or(menu_state.position)
                        .unwrap_or_default();
                    // Tell terminal_box a menu is open so the next click dismisses it.
                    terminal_box = terminal_box.context_menu(point);
                    let has_selection = data
                        .terminal
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
            };
            pane_grid::Content::new(element)
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .on_click(Message::PaneClicked)
        .on_resize(space_xxs, Message::PaneResized)
        .on_drag(Message::PaneDragged);

        let body: Element<'_, Message> = if self.tabs.len() > 1 {
            widget::column::with_children(vec![self.tab_bar(), grid.into()])
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else {
            grid.into()
        };
        self.with_resize_handle(body)
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
                Event::PlatformSpecific(PlatformSpecific::Wayland(WaylandEvent::Output(
                    event,
                    output,
                ))) => Some(Message::Output(event, output)),
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
                        while let Some((id, event)) = rx.recv().await {
                            output.send(Message::TermEvent(id, event)).await.unwrap();
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
            if self.slide.is_some() {
                // ~120 Hz while animating, nothing otherwise.
                cosmic::iced::time::every(std::time::Duration::from_millis(8)).map(Message::Tick)
            } else {
                Subscription::none()
            },
            if self.resizing {
                // Only listen to pointer motion while a drag is in progress, so idle
                // mouse movement does not wake the app.
                event::listen_with(|event, _status, _id| match event {
                    Event::Mouse(mouse::Event::CursorMoved { position }) => {
                        Some(Message::ResizeMove(position))
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
