# cosmic-quake-term

A Quake-style drop-down terminal for the [COSMIC](https://system76.com/cosmic) desktop.

Press a key and a terminal slides in at the top of your screen; press it again and it
disappears. The shell keeps running while hidden, so your session is still there when
you bring it back.

It is a small standalone program (not a cosmic-term plugin — cosmic-term has no plugin
system). On Wayland a normal window cannot pin itself to a screen edge, so the terminal
is drawn on a `wlr-layer-shell` overlay surface, the same mechanism the COSMIC launcher
and panel use.

## Features

- Drops down from the top edge below the panel and takes keyboard focus immediately;
  panel applet popups stay visible above it
- Size in percent of the screen (default 40 % high, 100 % wide); drag the bottom edge
  with the mouse to resize, the new height is remembered
- Slide animation (default 300 ms, can be changed or switched off)
- Settings page (right-click → *Settings*): height, width and animation with sliders
  and text fields, everything applied immediately; the same values are available on
  the command line
- Follows the system appearance: accent color, dark/light, and the translucent
  "frosted" look (transparency level and blur) chosen in COSMIC Settings
- Reuses your **cosmic-term settings** (font, color scheme, opacity) —
  change them in cosmic-term and the drop-down terminal follows
- Copy/paste (`Ctrl+Shift+C` / `Ctrl+Shift+V`), select all (`Ctrl+Shift+A`), zoom
  (`Ctrl+=`, `Ctrl+-`, `Ctrl+0` or `Ctrl` + mouse wheel)
- Right-click menu: copy, paste, select all, open/copy link
- Controlled over D-Bus, so any COSMIC custom shortcut can drive it
- Shell exits (`exit`) → hides and starts a fresh shell for the next toggle
- English and German; the language follows the system locale (`LANGUAGE`, `LC_ALL`,
  `LC_MESSAGES`, `LANG`), other locales fall back to English

## Install

Requires Rust 1.90+ and the usual libcosmic build dependencies. On Fedora:

```sh
sudo dnf install rust cargo just git gcc pkgconf-pkg-config wayland-devel \
    libxkbcommon-devel mesa-libEGL-devel fontconfig-devel freetype-devel \
    libinput-devel systemd-devel openssl-devel mold
```

(`mold` is used as linker via `.cargo/config.toml`; delete that file if you don't want it.)

```sh
just install             # release build → ~/.local/bin + launcher entry
just install-autostart   # start the daemon hidden at login
just uninstall           # remove everything again
```

Make sure `~/.local/bin` is in your `PATH`.

## Set up the shortcut

COSMIC Settings → Keyboard → Custom Shortcuts → Add:

| Name | Command | Key |
| --- | --- | --- |
| Quake terminal | `cosmic-quake-term --toggle` | e.g. `F12` |

## Usage

```
cosmic-quake-term              start (or show the running instance)
cosmic-quake-term --toggle     show/hide; starts the daemon if needed
cosmic-quake-term --show       show the terminal
cosmic-quake-term --hide       hide the terminal
cosmic-quake-term --quit       stop the running daemon
cosmic-quake-term --daemon     start hidden in the background (for autostart)
cosmic-quake-term --height N   height in percent of the screen, 10-100
cosmic-quake-term --width N    width in percent of the screen, 10-100 (centered)
cosmic-quake-term --animation on|off|MS   slide animation, `on` = 300 ms, max 2000
```

`--height`, `--width` and `--animation` can be combined and are applied live by a
running daemon. They are stored in `~/.config/cosmic/dev.norman.CosmicQuakeTerm/v1/`.
The same values can be changed in the settings page (right-click → *Settings*).

The D-Bus API is `dev.norman.CosmicQuakeTerm` on the session bus, object
`/dev/norman/CosmicQuakeTerm`, methods `Toggle`, `Show`, `Hide`, `Quit`:

```sh
busctl --user call dev.norman.CosmicQuakeTerm /dev/norman/CosmicQuakeTerm \
    dev.norman.CosmicQuakeTerm Toggle
```

## Limitations / TODO

- Width can only be changed with the settings or `--width`, not with the mouse
- The surface sits on the `top` layer (like the panel) so applet popups can appear above
  it; fullscreen windows therefore cover the terminal
- While dragging the bottom edge, a transparent screen-height area covers the windows
  behind it (clicks beside the terminal go to the terminal's surface until you release)
- Shortcuts on non-Latin keyboard layouts (e.g. Cyrillic) fall back to the logical key
  only
- Single terminal: no tabs or splits
- Shows on the active output only

## How it is built

`libcosmic` (iced + sctk) provides the layer-shell surface. The terminal itself —
`terminal.rs`, `terminal_box.rs`, `mouse_reporter.rs`, `terminal_theme.rs`, `config.rs`,
the context menu in `menu.rs` and `localize.rs` — is derived from
[cosmic-term](https://github.com/pop-os/cosmic-term) with tabs, panes and settings UI
removed. `slide.rs` is a reduced copy of iced's `Float` widget that moves the content
for the slide animation (the surface itself never moves, so the panel is not overdrawn).

Translations live in `i18n/<language>/cosmic_quake_term.ftl` (Fluent). To add a language,
copy `i18n/en`, translate it and add `Name[xx]`/`Comment[xx]` to the `.desktop` files.

libcosmic is pinned to a revision in `Cargo.toml`. It must read the same theme config
version as your COSMIC release (COSMIC 1.10 stores it under `…/v2`); an older libcosmic
silently falls back to the default theme, which shows up as wrong accent and transparency.
The copied terminal code uses libcosmic's API, so bump the revision and the code together.

## License

GPL-3.0-only, because it contains code derived from cosmic-term (© System76).
See [LICENSE](LICENSE).
