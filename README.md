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

- Overlay anchored to the top edge, full width, takes keyboard focus immediately
- Reuses your **cosmic-term settings** (font, color scheme, opacity) —
  change them in cosmic-term and the drop-down terminal follows
- Copy/paste (`Ctrl+Shift+C` / `Ctrl+Shift+V`), select all (`Ctrl+Shift+A`), zoom
  (`Ctrl+=`, `Ctrl+-`, `Ctrl+0`)
- Right-click menu: copy, paste, select all, open/copy link
- Controlled over D-Bus, so any COSMIC custom shortcut can drive it
- Shell exits (`exit`) → hides and starts a fresh shell for the next toggle

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
```

The D-Bus API is `dev.norman.CosmicQuakeTerm` on the session bus, object
`/dev/norman/CosmicQuakeTerm`, methods `Toggle`, `Show`, `Hide`, `Quit`:

```sh
busctl --user call dev.norman.CosmicQuakeTerm /dev/norman/CosmicQuakeTerm \
    dev.norman.CosmicQuakeTerm Toggle
```

## Limitations / TODO

- Height is fixed at 400 px, width is the full screen; no setting yet
- No slide animation
- Single terminal: no tabs or splits
- The context menu is English only
- Shows on the active output only

## How it is built

`libcosmic` (iced + sctk) provides the layer-shell surface. The terminal itself —
`terminal.rs`, `terminal_box.rs`, `mouse_reporter.rs`, `terminal_theme.rs`, `config.rs`,
and the context menu in `menu.rs` — is derived from
[cosmic-term](https://github.com/pop-os/cosmic-term) with tabs, panes and settings UI
removed. `Cargo.lock` is taken from cosmic-term so libcosmic and cosmic-text stay on
matching revisions; keep them in sync when updating.

## License

GPL-3.0-only, because it contains code derived from cosmic-term (© System76).
See [LICENSE](LICENSE).
