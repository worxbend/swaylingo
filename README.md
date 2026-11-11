<div align="center">

<img src="assets/banner.jpg" alt="Two application windows above a keyboard, one key lit" width="100%">

<br>

<img src="assets/logo.jpg" alt="swaylingo mark" width="96">

# swaylingo

### the binary is `skbd`

**Each Sway window keeps its own keyboard layout.**

Focus Firefox in English, switch to Telegram and type Ukrainian, then focus Firefox again. English is already back. You do not press the layout key.

[![Release](https://img.shields.io/github/v/release/worxbend/swaylingo?sort=semver&color=C75CFF&label=release&style=for-the-badge)](https://github.com/worxbend/swaylingo/releases)
[![Site](https://img.shields.io/github/actions/workflow/status/worxbend/swaylingo/pages.yml?branch=main&label=site&style=for-the-badge&color=5BF0B8)](https://github.com/worxbend/swaylingo/actions/workflows/pages.yml)
[![Rust](https://img.shields.io/badge/Rust-stable-E85D4C?logo=rust&logoColor=white&style=for-the-badge)](https://www.rust-lang.org)

[**Website**](https://worxbend.github.io/swaylingo/) · [**Install**](https://worxbend.github.io/swaylingo/install.html) · [**Releases**](https://github.com/worxbend/swaylingo/releases) · [**Issues**](https://github.com/worxbend/swaylingo/issues)

</div>

---

## What this is

`skbd` is the daemon. The project is `swaylingo`.

Sway has one keyboard layout for the whole seat. `skbd` remembers the layout of each **application window** and puts it back when that window is focused. An empty workspace, with no window focused, goes back to the fallback layout. A newly opened window uses that fallback too, unless `[apps]` names that application.

Sway still owns the keymap. Super+Space still switches. `skbd` only restores.

```text
Firefox window, English
        ↓ focus
Telegram window, you switch to Ukrainian
        ↓ focus
Firefox window, English is restored
        ↓
empty workspace, fallback English
        ↓
new Alacritty window, fallback English
```

Try the same rules in the browser: [the desk](https://worxbend.github.io/swaylingo/). The demo windows are labeled Firefox and Telegram.

---

## Why it feels different

| Sway alone | With skbd |
| --- | --- |
| Focus the Firefox window and switch to Ukrainian. | Firefox stays on the layout you left it on. |
| Focus the Telegram window. It is still Ukrainian. | Telegram can start in Ukrainian because `[apps]` says so. |
| Switch it back by hand. | Come back to Firefox and English is already there. |
| Open Alacritty. It inherits Ukrainian. | A new Alacritty window starts on the fallback, English when `fallback` is `us`. |
| Switch to an empty workspace. Ukrainian stays. | An empty workspace uses the fallback. |

---

## Quick start

### 1. Two layouts in Sway

`skbd` switches an index. It does not create layouts. The first name in `xkb_layout` is layout 0.

```
input type:keyboard {
    xkb_layout us,ua
    xkb_options grp:win_space_toggle
}
```

Reload Sway. `us` is English. `ua` is Ukrainian.

### 2. Install the binary with binstaller

The site is [worxbend.github.io/binstaller](https://worxbend.github.io/binstaller/). One YAML profile, `plan` before any write, SHA-256 checked, then `apply`. binstaller refuses installer scripts, so this step installs the `skbd` binary only. The systemd unit and the man page stay on `install.sh`.

```bash
curl --proto '=https' --tlsv1.2 -sSfL \
  https://github.com/worxbend/binstaller/releases/latest/download/install.sh | sh
```

From a clone of this repo:

```bash
binstaller plan --config packaging/swaylingo.binstaller.yaml
binstaller apply --config packaging/swaylingo.binstaller.yaml
binstaller versions --config packaging/swaylingo.binstaller.yaml
```

`plan` prints the URL and the checksum. Nothing is written. `apply` puts the binary in `~/.apps/swaylingo/bin/skbd`. A link into `~/.local/bin` is not in the profile: binstaller only allows a relative symlink there, and `install.sh` is what writes `~/.local/bin/skbd`. The other architecture is skipped by `when`. To force one entry:

```bash
binstaller apply --config packaging/swaylingo.binstaller.yaml --only skbd-linux-amd64
```

The profile is [packaging/swaylingo.binstaller.yaml](packaging/swaylingo.binstaller.yaml). It is `policy.mode: strict` and pins `v0.4.0` with the hashes from that release's `SHA256SUMS`. Duplicate plan names are rejected, so the two machines are `skbd-linux-amd64` and `skbd-linux-arm64`.

### 3. Install the unit and the man page

`install.sh` is the path that also writes the user service and `man skbd`. Use it after binstaller, or instead of it when you want all three files in one step. Run it as your user, not root.

```bash
curl --fail --location --proto '=https' --tlsv1.2 \
  --output install.sh \
  https://github.com/worxbend/swaylingo/releases/latest/download/install.sh
bash install.sh
```

That checks `SHA256SUMS`, then writes:

```text
~/.local/bin/skbd
~/.config/systemd/user/skbd.service
~/.local/share/man/man1/skbd.1
```

| Machine | Release file |
| --- | --- |
| Intel / AMD | `skbd-linux-amd64` |
| ARM64 | `skbd-linux-arm64` |

The unit is not enabled. Full steps: [docs/install.md](docs/install.md).

### 4. Start it after Sway is up

`SWAYSOCK` contains Sway's pid. It changes every time Sway starts.

```bash
systemctl --user import-environment SWAYSOCK
systemctl --user restart skbd.service
```

On every Sway start:

```
exec_always "systemctl --user import-environment SWAYSOCK && systemctl --user restart skbd.service"
```

### 5. Optional per-app rule

`~/.config/swaylingo/config.toml` is optional. Copy `config.example.toml`.

```toml
fallback = "us"

[apps]
Telegram = "ua"
discord = "ua"
"jetbrains-idea" = "us"
Code = "us"
code = "us"
```

A key is the Wayland `app_id`, or the X11 class. A wrong key is ignored. Stop `skbd` first, then copy the id from `swaymsg -t get_tree`.

Firefox is one Sway window, so its tabs share one layout.

---

## Check it

1. Focus the Firefox window. Press Super+Space until the layout is Ukrainian.
2. Focus the Telegram window. Press Super+Space until the layout is English.
3. Focus Firefox again. Ukrainian is back.
4. Open a new Alacritty window. It starts on the fallback, English.
5. Switch to an empty workspace, one with no focused window. The layout returns to the fallback. Firefox is still Ukrainian when you go back.

```bash
systemctl --user status skbd.service
```

Stop the service before you run `~/.local/bin/skbd --verbose` in a terminal. Two copies both try to switch.

---

## Commands

```text
skbd
skbd --verbose
skbd --version
skbd --help
skbd --config PATH
skbd --default TOKEN
```

| Flag | What it does |
| --- | --- |
| `--verbose` | Print focus, layout, and switch lines. |
| `--version` | Print the crate version. `0.4.0` on the current release. |
| `--config PATH` | Read this file. A missing explicit path is an error. |
| `--default TOKEN` | Override `fallback` for this process. `us`, `ua`, a layout name, or an index. |

`man skbd` is the same contract.

---

## What you get

| | |
| --- | --- |
| Per window | The layout is stored on the Sway window, not on the application name. |
| Fallback | A new window and an empty workspace use layout 0, unless you set `fallback`. |
| Per app | The first time an application appears, `[apps]` can force Ukrainian or English. |
| Dialogs | An X11 dialog copies the layout of the window it belongs to. |
| Two Linux builds | amd64 and arm64, from the release workflow. |

---

## Not this

- One layout per Firefox tab. One window, one layout.
- fcitx5 or ibus. English and Ukrainian here are XKB layouts.
- A file of window ids. Those ids die when Sway exits.

The decision record is [docs/decisions/0001-layout-memory.md](docs/decisions/0001-layout-memory.md).

---

## Troubleshooting

<details>
<summary><code>SWAYSOCK is not set</code></summary>

<br>

The shell is outside Sway, or the user service started before Sway imported the socket.

```bash
echo "$SWAYSOCK"
systemctl --user import-environment SWAYSOCK
systemctl --user restart skbd.service
```

</details>

<details>
<summary>Focus did not change the layout</summary>

<br>

You need at least two names in `xkb_layout`. Reload Sway after you edit it. `pgrep -a skbd` should show one process.

</details>

<details>
<summary>Telegram did not start in Ukrainian</summary>

<br>

The key under `[apps]` did not match the window. Stop the service and copy `app_id`, or the X11 class, from `swaymsg -t get_tree`. A wrong key is ignored, not an error.

</details>

<details>
<summary>Install says run as your user, not root</summary>

<br>

The unit is a systemd user service. Drop the sudo.

</details>

---

## Development

```bash
git clone https://github.com/worxbend/swaylingo
cd swaylingo
cargo test
cargo clippy -- -D warnings
cargo build --release
./target/release/skbd
```

| Piece | Owns |
| --- | --- |
| `src/memory.rs` | Which window has which layout, and when to switch. |
| `src/config.rs` | `fallback` and `[apps]`. |
| `src/main.rs` | The Sway socket, signals, and the lock. |
| `install.sh` | Download, checksum, `~/.local/bin/skbd`, the user unit, the man page. |

---

<div align="center">

**If Firefox came back in the right language, a star is enough.**

</div>
