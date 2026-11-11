# sway-lang

Each Sway window keeps the keyboard layout you last used there. Focus it again and the keys are already in that language.

Sway still owns the keymap. You keep `xkb_layout` and Super+Space. This daemon only remembers, and restores, the layout index.

```
window A, English  →  window B, Ukrainian  →  window A
                                              English comes back
new window, or an empty workspace            English, layout 0
```

With `xkb_layout us,ua`, layout 0 is English and layout 1 is Ukrainian. A window you have never focused starts on English, even if the previous window was Ukrainian. So does a workspace with no focused window. The window you left keeps Ukrainian.

Try the same rules in the browser: [docs/index.html](docs/index.html). GitHub Pages publishes that folder. After the first push to `main`, the Pages workflow deploys it. In the repository settings, set Pages to GitHub Actions.

## Quick start

Sway must already be running. The shell needs `SWAYSOCK`.

```
cargo test
cargo build --release
./target/release/sway-lang
```

| Command | What it does |
| --- | --- |
| `cargo test` | Runs the layout memory tests |
| `cargo clippy -- -D warnings` | Lints the crate |
| `cargo build --release` | Writes `target/release/sway-lang` |
| `./target/release/sway-lang` | Listens until Sway exits |
| `./target/release/sway-lang --verbose` | Prints focus, layout, and switch lines |
| `./target/release/sway-lang --version` | Prints `0.1.0` |

`--verbose` prints one line per keyboard device. One physical keyboard often appears as several Sway devices, so one Super+Space prints several `layout` lines.

## Check it

1. Focus window A. Press Super+Space until the layout is Ukrainian.
2. Focus window B. Press Super+Space until the layout is English.
3. Focus window A. The layout returns to Ukrainian.
4. Open a new window. It starts in English.
5. Switch to an empty workspace. The layout returns to English. Window A is still Ukrainian when you go back.

## systemd

`sway-lang.service` is in the repository. It is not installed and not enabled.

`SWAYSOCK` looks like `/run/user/1000/sway-ipc.1000.<sway-pid>.sock`. The pid changes every time Sway starts. `sway --get-socketpath` fails without that variable, and so does this daemon. Import it after Sway is up, then start the service:

```
mkdir -p ~/.config/systemd/user
cp sway-lang.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user import-environment SWAYSOCK
systemctl --user restart sway-lang.service
```

A service started at login, before that import, exits and stays down until you import and restart it. Edit `ExecStart` in the unit if the binary does not live at `~/Projects/sway-lang-source/target/release/sway-lang`.

## How it is put together

```
src/memory.rs    window id → layout index, and when to switch
src/main.rs      two Sway sockets: one for events, one for commands
```

swayipc 4’s `subscribe` consumes its connection, so commands need a second one. The decision is written down in [docs/decisions/0001-layout-memory.md](docs/decisions/0001-layout-memory.md).

The daemon asks Sway for `input type:keyboard xkb_switch_layout <index>`. `input *` also hits pointers. A switch this process requested is marked `pending`, so the layout event that comes back is not stored as if you had pressed Super+Space.

## Not included

- One layout per browser tab. Firefox is one Sway window, so its tabs share a layout.
- fcitx5 or ibus. English and Ukrainian here are XKB layouts.
- A file of remembered layouts. Sway window ids do not survive a restart.
