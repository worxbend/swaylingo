#!/usr/bin/env bash
# Install sway-lang from GitHub releases and write the systemd user unit.
#
# The binary lands in ~/.local/bin/sway-lang. The unit is
# ~/.config/systemd/user/sway-lang.service. The service is not enabled:
# SWAYSOCK contains Sway's pid, so start it only after Sway is up.
#
# Usage: install.sh [latest|vX.Y.Z]
# Exit: 0 installed, 1 failure, 2 bad arguments.

set -Eeuo pipefail
shopt -s inherit_errexit
IFS=$'\n\t'

readonly repo="worxbend/sway-lang-source"
readonly script_name="install.sh"

usage() {
  cat <<'EOF'
Usage: install.sh [latest|vX.Y.Z]

Download the Linux binary for this machine from the GitHub release,
check SHA256SUMS, and install:

  ~/.local/bin/sway-lang
  ~/.config/systemd/user/sway-lang.service

The unit is not enabled. After Sway is running:

  systemctl --user import-environment SWAYSOCK
  systemctl --user restart sway-lang.service

Exit codes: 0 installed, 1 failure, 2 bad arguments.
EOF
}

die() {
  printf 'install.sh: %s\n' "$1" >&2
  exit 1
}

# asset_for MACHINE
# Prints the release asset name for a `uname -m` value.
asset_for() {
  local machine=$1
  case "$machine" in
    x86_64)
      printf '%s\n' sway-lang-linux-amd64
      ;;
    aarch64 | arm64)
      printf '%s\n' sway-lang-linux-arm64
      ;;
    *)
      printf 'unsupported machine: %s\n' "$machine" >&2
      return 1
      ;;
  esac
}

# normalize_tag RAW
# Prints `latest` or `vX.Y.Z`. Rejects anything that could change the URL path.
normalize_tag() {
  local raw=$1
  if [[ "$raw" == "latest" ]]; then
    printf '%s\n' latest
    return 0
  fi
  if [[ "$raw" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
    printf 'v%s\n' "${BASH_REMATCH[1]}"
    return 0
  fi
  if [[ "$raw" =~ ^([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
    printf 'v%s\n' "${BASH_REMATCH[1]}"
    return 0
  fi
  printf 'version must be latest or vX.Y.Z\n' >&2
  return 2
}

# release_url TAG NAME
release_url() {
  local tag=$1
  local name=$2
  if [[ "$tag" == "latest" ]]; then
    printf 'https://github.com/%s/releases/latest/download/%s\n' "$repo" "$name"
    return 0
  fi
  printf 'https://github.com/%s/releases/download/%s/%s\n' "$repo" "$tag" "$name"
}

require_tools() {
  local tool
  for tool in curl sha256sum install mkdir; do
    command -v "$tool" >/dev/null 2>&1 || die "missing command: $tool"
  done
}

write_unit() {
  local dest=$1
  cat >"$dest" <<'EOF'
[Unit]
Description=Per-window keyboard layout for Sway

[Service]
Type=simple
# SWAYSOCK contains Sway's pid. Import it after Sway starts, then restart
# this service. A copy started at login, before that import, cannot see Sway.
ExecStart=%h/.local/bin/sway-lang
Restart=on-failure
RestartSec=2
EOF
}

install_release() {
  local tag=$1
  local asset=$2
  local tmp bin_url sums_url bindir unitdir

  tmp=$(mktemp -d) || die "could not create a temp directory"
  # shellcheck disable=SC2064 # tmp is set; the trap must keep this directory.
  trap "rm -rf -- ${tmp@Q}" EXIT

  bin_url=$(release_url "$tag" "$asset")
  sums_url=$(release_url "$tag" "SHA256SUMS")
  printf 'downloading %s\n' "$bin_url"
  curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    --retry 3 --retry-delay 2 --output "${tmp}/${asset}" -- "$bin_url"
  curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
    --retry 3 --retry-delay 2 --output "${tmp}/SHA256SUMS" -- "$sums_url"

  (
    cd -- "$tmp"
    sha256sum --check --ignore-missing -- SHA256SUMS
  )

  bindir="${HOME}/.local/bin"
  unitdir="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user"
  mkdir -p -- "$bindir" "$unitdir"
  install -m 755 -- "${tmp}/${asset}" "${bindir}/sway-lang"
  write_unit "${tmp}/sway-lang.service"
  install -m 644 -- "${tmp}/sway-lang.service" "${unitdir}/sway-lang.service"

  if command -v systemctl >/dev/null 2>&1 && [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
    systemctl --user daemon-reload
  else
    printf 'user systemd is not available in this shell. After you log in, run: systemctl --user daemon-reload\n'
  fi

  printf 'installed %s\n' "${bindir}/sway-lang"
  printf 'installed %s\n' "${unitdir}/sway-lang.service"
  printf 'The service is not started. After Sway is up:\n'
  printf '  systemctl --user import-environment SWAYSOCK\n'
  printf '  systemctl --user restart sway-lang.service\n'
}

main() {
  local requested tag asset
  case "${1:-}" in
    -h | --help)
      usage
      exit 0
      ;;
    --self-check)
      [[ "$(asset_for x86_64)" == "sway-lang-linux-amd64" ]]
      [[ "$(asset_for aarch64)" == "sway-lang-linux-arm64" ]]
      [[ "$(asset_for arm64)" == "sway-lang-linux-arm64" ]]
      [[ "$(normalize_tag latest)" == "latest" ]]
      [[ "$(normalize_tag 0.1.0)" == "v0.1.0" ]]
      [[ "$(normalize_tag v0.1.0)" == "v0.1.0" ]]
      if asset_for sparc >/dev/null 2>&1; then
        exit 1
      fi
      if normalize_tag '../evil' >/dev/null 2>&1; then
        exit 1
      fi
      printf 'ok\n'
      exit 0
      ;;
    "")
      requested="latest"
      ;;
    -*)
      usage >&2
      exit 2
      ;;
    *)
      if [[ "$#" -ne 1 ]]; then
        usage >&2
        exit 2
      fi
      requested=$1
      ;;
  esac

  if [[ "$(uname -s)" != "Linux" ]]; then
    die "Linux only"
  fi
  if [[ "${EUID}" -eq 0 ]]; then
    die "run as your user, not root. The unit is a systemd user service"
  fi

  tag=$(normalize_tag "$requested") || exit 2
  asset=$(asset_for "$(uname -m)") || exit 1
  require_tools
  install_release "$tag" "$asset"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
