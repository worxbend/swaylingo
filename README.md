<div align="center">

# ⌨️ sway-lang

### 🪟 each window keeps its own keyboard. focus comes back already switched.

**Sway owns the keymap. This daemon just remembers.**

[![Release](https://img.shields.io/github/v/release/worxbend/sway-lang-source?sort=semver&color=C75CFF&label=release&style=for-the-badge)](https://github.com/worxbend/sway-lang-source/releases)
[![Site](https://img.shields.io/github/actions/workflow/status/worxbend/sway-lang-source/pages.yml?branch=main&label=site&style=for-the-badge&color=5BF0B8)](https://github.com/worxbend/sway-lang-source/actions/workflows/pages.yml)
[![Rust](https://img.shields.io/badge/Rust-stable-E85D4C?logo=rust&logoColor=white&style=for-the-badge)](https://www.rust-lang.org)

[**🌐 Website**](https://worxbend.github.io/sway-lang-source/) &nbsp;•&nbsp;
[**📖 Install**](https://worxbend.github.io/sway-lang-source/install.html) &nbsp;•&nbsp;
[**📦 Releases**](https://github.com/worxbend/sway-lang-source/releases) &nbsp;•&nbsp;
[**🐞 Issues**](https://github.com/worxbend/sway-lang-source/issues)

</div>

---

## 🤔 Why?

Sway has one keyboard layout for the whole seat. You type Ukrainian in one window, hop to another, and the second window is still Ukrainian. macOS and GNOME do not do that. Each window keeps the layout it last used.

<table>
<tr><th align="left">😩 Sway alone</th><th align="left">✨ With sway-lang</th></tr>
<tr valign="top"><td>

1. Focus notes, switch to Ukrainian
2. Focus mail
3. Mail is still Ukrainian
4. Switch it back by hand
5. Go back to notes
6. Switch to Ukrainian again

</td><td>

```text
notes 🇺🇦  →  mail 🇬🇧  →  notes
                         🇺🇦 is already back
new window, empty workspace   🇬🇧 layout 0
```

**Super+Space still works.** The daemon only puts the right layout back when focus moves.

</td></tr>
</table>

Layout 0 is the first name in `xkb_layout`. With `us,ua`, that is English. A brand new window starts there. An empty workspace does too. A window you already used comes back in whatever you left it on.

Try the same rules in the browser: [the desk](https://worxbend.github.io/sway-lang-source/).

---

## ⚡ Quick start

### 1️⃣ Two layouts in Sway

The daemon switches an index. It does not invent layouts.

```
input type:keyboard {
    xkb_layout us,ua
    xkb_options grp:win_space_toggle
}
```

Reload Sway. `us` is index 0. `ua` is index 1.

### 2️⃣ Install the binary

Linux amd64 or arm64. Run it as your user, not root.

```bash
curl --fail --location --proto '=https' --tlsv1.2 \
  --output install.sh \
  https://github.com/worxbend/sway-lang-source/releases/latest/download/install.sh
bash install.sh
```

That checks `SHA256SUMS`, then writes:

```text
~/.local/bin/sway-lang
~/.config/systemd/user/sway-lang.service
```

| Machine | Asset |
| --- | --- |
| 🐧 Intel/AMD | `sway-lang-linux-amd64` |
| 🐧 ARM64 | `sway-lang-linux-arm64` |

The unit is **not** enabled. Full steps, upgrade, and remove: [docs/install.md](docs/install.md).

<details>
<summary>🔨 <b>Build from source</b></summary>

<br>

Sway must be running. The shell needs `SWAYSOCK`.

```bash
git clone https://github.com/worxbend/sway-lang-source
cd sway-lang-source
cargo build --release
./target/release/sway-lang
```

</details>

### 3️⃣ Start it after Sway is up

`SWAYSOCK` contains Sway's pid. It changes every time Sway starts. Import it, then restart the unit.

```bash
systemctl --user import-environment SWAYSOCK
systemctl --user restart sway-lang.service
```

Same thing on every Sway start:

```
exec_always "systemctl --user import-environment SWAYSOCK && systemctl --user restart sway-lang.service"
```

A unit that starts at login, before that import, exits and stays down.

---

## 👀 Check it

1. Focus window A. Super+Space until it is Ukrainian.
2. Focus window B. Super+Space until it is English.
3. Focus window A. Ukrainian is back. No extra key.
4. Open a new window. It starts on layout 0.
5. Switch to an empty workspace. Layout 0 again. Window A is still Ukrainian when you return.

```bash
systemctl --user status sway-lang.service
```

Stop the service before you run `~/.local/bin/sway-lang --verbose` in a terminal. Two copies will both try to switch.

---

## ✨ What you get

| | |
| --- | --- |
| 🪟 **Per window** | The layout is stored on the Sway window id, not the app. |
| 🇬🇧 **Default is layout 0** | New windows and an empty desktop start on the first `xkb_layout`. |
| ↩️ **Known windows restore** | Leave a window on Ukrainian. Come back. It is Ukrainian. |
| 🔒 **Checksum** | `install.sh` checks `SHA256SUMS` before it copies the binary. |
| 🐧 **Two Linux builds** | amd64 and arm64, from the `v*` release workflow. |
| 🤫 **Quiet** | No output unless you pass `--verbose`. |

---

## 🚩 Commands

```text
sway-lang
sway-lang --verbose
sway-lang --version
install.sh [latest|vX.Y.Z]
```

| Flag | What it does |
| --- | --- |
| `--verbose` | Print focus, layout, and switch lines. One keyboard can show up as several Sway devices, so one Super+Space prints several lines. |
| `--version` | Print the crate version. `0.1.2` on the current release. |

---

## 🚫 Not this

- 🦊 One layout per browser tab. Firefox is one Sway window, so its tabs share a layout.
- 🈶 fcitx5 or ibus. English and Ukrainian here are XKB layouts.
- 💾 A file of remembered layouts. Window ids die when Sway exits.

The why is in [docs/decisions/0001-layout-memory.md](docs/decisions/0001-layout-memory.md).

---

## 🩹 Troubleshooting

<details>
<summary><code>SWAYSOCK is not set</code></summary>

<br>

The shell is outside Sway, or the user service started before Sway imported the socket.

```bash
echo "$SWAYSOCK"
systemctl --user import-environment SWAYSOCK
systemctl --user restart sway-lang.service
```

</details>

<details>
<summary>The layout did not change when I focused a window</summary>

<br>

You need at least two names in `xkb_layout`. One layout has nothing to switch to. Reload Sway after you edit it.

Also check you are not running two copies. `pgrep -a sway-lang` should show one process.

</details>

<details>
<summary>New windows open in Ukrainian</summary>

<br>

A window seen for the first time starts on layout 0. A window you already used still restores its own layout. If a new window opens in Ukrainian, the daemon was not running, or that window id was already remembered.

</details>

<details>
<summary>Install says run as your user, not root</summary>

<br>

The unit is a systemd **user** service. `sudo ./install.sh` installs it for root. Drop the sudo.

</details>

---

## 🛠️ Development

```bash
cargo test
cargo clippy -- -D warnings
cargo build --release
```

| Piece | Owns |
| --- | --- |
| `src/memory.rs` | Which window has which layout, and when to switch. |
| `src/main.rs` | Two Sway sockets. One listens. One runs `xkb_switch_layout`. |
| `install.sh` | Download, checksum, `~/.local/bin`, user unit. |
| `.github/workflows/release.yml` | amd64 and arm64 binaries on `v*` tags. |

`subscribe` in swayipc 4 consumes its socket, so commands use a second connection. The command is `input type:keyboard xkb_switch_layout`. `input *` also hits pointers.

---

<div align="center">

**if this stopped you switching layouts by hand, a ⭐ is enough.**

</div>
