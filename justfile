name := 'cosmic-quake-term'
app-id := 'dev.norman.CosmicQuakeTerm'

# Per-user install by default; override with `just prefix=/usr install` (needs root).
prefix := env('HOME') / '.local'
bin-dst := prefix / 'bin' / name
desktop-dst := prefix / 'share/applications' / app-id + '.desktop'
autostart-dst := env('XDG_CONFIG_HOME', env('HOME') / '.config') / 'autostart' / app-id + '-autostart.desktop'

default: build-release

build-debug *args:
    cargo build {{args}}

build-release *args:
    cargo build --release {{args}}

# Formatting and lints; run before committing.
check:
    cargo fmt --check
    cargo clippy --all-targets

fmt:
    cargo fmt

run *args:
    cargo run --release -- {{args}}

# Install binary and launcher entry. Stops a running daemon first so the new binary is used.
install: build-release
    -{{bin-dst}} --quit
    install -Dm755 target/release/{{name}} {{bin-dst}}
    install -Dm644 res/{{app-id}}.desktop {{desktop-dst}}
    @echo "installed to {{bin-dst}}"
    @echo "next: bind '{{name}} --toggle' in Settings > Keyboard > Custom Shortcuts"

# Start the daemon hidden at login.
install-autostart:
    install -Dm644 res/{{app-id}}-autostart.desktop {{autostart-dst}}
    @echo "autostart entry: {{autostart-dst}}"

uninstall:
    -{{bin-dst}} --quit
    rm -f {{bin-dst}} {{desktop-dst}} {{autostart-dst}}
