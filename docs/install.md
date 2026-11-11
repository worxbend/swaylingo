# Install sway-lang

Linux on amd64 or arm64. Sway is already running. You install a release binary and a systemd user unit. The unit is not enabled. Sway still owns the keymap.

## 1. Give Sway more than one layout

sway-lang switches an index. It does not create layouts. The first name in `xkb_layout` is layout 0, and that is the default for a new window and for an empty workspace.

```
input type:keyboard {
    xkb_layout us,ua
    xkb_options grp:win_space_toggle
}
```

Reload Sway after you change this. `us,ua` means index 0 is English and index 1 is Ukrainian. Super+Space still changes the layout. The daemon only remembers which index belonged to which window.

## 2. Install the release

You need bash 4.4 or newer, `curl`, `sha256sum`, and `install`. Run this as your user, not as root. The unit is a user service.

From a clone:

```
./install.sh
```

Or pin a release:

```
./install.sh v0.1.2
```

From the release asset, without a clone:

```
curl --fail --location --proto '=https' --tlsv1.2 \
  --output install.sh \
  https://github.com/worxbend/sway-lang-source/releases/latest/download/install.sh
bash install.sh
```

The script picks the asset for this machine.

| Machine | File |
| --- | --- |
| `x86_64` | `sway-lang-linux-amd64` |
| `aarch64` or `arm64` | `sway-lang-linux-arm64` |

It checks `SHA256SUMS` before it copies anything. Then it writes:

```
~/.local/bin/sway-lang
~/.config/systemd/user/sway-lang.service
```

and runs `systemctl --user daemon-reload` when a user session is available. It does not enable the service and it does not start it.

`~/.local/bin` must be on `PATH` if you want to run `sway-lang` from a shell. The unit uses the full path, so systemd does not need that.

## 3. Start it after Sway

`SWAYSOCK` contains Sway’s pid. The path changes every time Sway starts. A service that starts at login, before that variable is copied into the user manager, exits and stays down.

In a terminal that Sway started, or any shell where `echo "$SWAYSOCK"` prints a path:

```
systemctl --user import-environment SWAYSOCK
systemctl --user restart sway-lang.service
```

To do that on every Sway start, add this line to the Sway config. It restarts the unit you already installed. It does not replace the unit.

```
exec_always "systemctl --user import-environment SWAYSOCK && systemctl --user restart sway-lang.service"
```

## 4. Check

1. Focus window A. Press Super+Space until the layout is Ukrainian.
2. Focus window B. Press Super+Space until the layout is English.
3. Focus window A. The layout returns to Ukrainian.
4. Open a new window. It starts on layout 0.
5. Switch to an empty workspace. The layout returns to layout 0. Window A is still Ukrainian when you go back.

`systemctl --user status sway-lang.service` should show the unit running. `journalctl --user -u sway-lang.service` shows its stderr. Start it with `--verbose` only while you are looking: edit is not required. Run `~/.local/bin/sway-lang --verbose` in a terminal if you want the focus lines on screen, and stop the service first so two copies are not both switching.

## Upgrade

```
./install.sh
systemctl --user import-environment SWAYSOCK
systemctl --user restart sway-lang.service
```

The same command replaces the binary and the unit file.

## Remove

```
systemctl --user disable --now sway-lang.service
rm -- ~/.local/bin/sway-lang
rm -- ~/.config/systemd/user/sway-lang.service
systemctl --user daemon-reload
```

`disable` is harmless if the unit was never enabled.

## Build from source instead

Sway must be running, and the shell must have `SWAYSOCK`.

```
cargo test
cargo build --release
./target/release/sway-lang
```

That binary is not copied into `~/.local/bin`. Use `install.sh` when you want the release and the unit.
