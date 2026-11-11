# ADR-001: Remember keyboard layouts in a user daemon

## Status

Accepted

## Date

2026-11-11

## Context

Sway applies one XKB layout to the seat. `xkb_layout us,ua` plus Super+Space
switches that layout for every window. macOS and GNOME keep a layout per window.
Current `sway-input(5)` has no seat option that does this.

The socket path in `SWAYSOCK` contains Sway’s pid, so it changes when Sway
restarts. Container ids also die with the window.

## Decision

A Rust process, `skbd`, subscribes to `window`, `input`, and `workspace`
events. It stores `container id → layout index` in memory.

- A layout change while a window is focused is stored on that window.
- Focusing a known window runs `input type:keyboard xkb_switch_layout <index>`.
- A window seen for the first time uses the app rule from the config, or
  the fallback. The fallback is index 0 when the config does not set one.
  That is the first name in `xkb_layout`. For `us,ua`, it is English. An
  empty desktop uses the fallback. A window already in the map keeps its
  stored layout.
- The window that is already focused when the daemon starts keeps the layout
  Sway is using at that moment.
- Nothing is written to disk.

Commands use a second IPC connection. `Connection::subscribe` in swayipc 4
consumes the socket it is called on.

`pending` records every index this process requested, including one a later
request replaced. Only those `xkb_layout` echoes are ignored. Any other index
is a user change on the focused window, and it clears the outstanding requests.

## Alternatives considered

### Go

A Go client can speak the same socket. swayipc in Rust already types the
window and input events this program needs.

### fcitx5 or ibus

English and Ukrainian are XKB layouts. An input-method daemon is a second
product.

### Remembering the previous window’s layout for new windows

That is what the first version did. An empty workspace, and every new window
on a busy workspace, then opened in Ukrainian after one Ukrainian window.
Index 0 is the default instead.

## Consequences

- Reload Sway and the map is empty. Window ids from the old process are useless.
- A systemd user service must import `SWAYSOCK` after Sway is up, then start.
- Browser tabs share a layout, because Firefox is one Sway window.
