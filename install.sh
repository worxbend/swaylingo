#!/usr/bin/env bash
# Install sway-lang from GitHub releases and write the systemd user unit.
#
# The binary lands in ~/.local/bin/sway-lang. The unit is
# $XDG_CONFIG_HOME/systemd/user/sway-lang.service, or
# ~/.config/systemd/user/sway-lang.service when that variable is unset.
# The man page is ~/.local/share/man/man1/sway-lang.1. The service is not enabled:
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
  ${XDG_CONFIG_HOME:-~/.config}/systemd/user/sway-lang.service
  ~/.local/share/man/man1/sway-lang.1

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
  for tool in curl sha256sum install mkdir getent; do
    command -v "$tool" >/dev/null 2>&1 || die "missing command: $tool"
  done
}

# HOME must be the passwd home for this uid. A borrowed HOME would install
# the unit and the binary into someone else's files.
require_own_home() {
  local entry passwd_home
  entry=$(getent passwd "$EUID") || die "no passwd entry for uid ${EUID}"
  IFS=: read -r _ _ _ _ _ passwd_home _ <<<"$entry"
  if [[ -z "${passwd_home}" || "${HOME}" != "${passwd_home}" ]]; then
    die "HOME is not the home directory for uid ${EUID}"
  fi
}

# keep_release_sums SUMS ASSET INCLUDE_MAN DEST
# INCLUDE_MAN is 1 when sway-lang.1 was downloaded into the temp dir.
# The dest file has one line per kept name. A missing binary line fails.
keep_release_sums() {
  local sums=$1
  local asset=$2
  local include_man=$3
  local dest=$4
  local line hex rest name base
  local found_asset=0
  local found_man=0
  : >"$dest"
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ -z "${line//[[:space:]]/}" ]] && continue
    if [[ ${#line} -lt 66 ]]; then
      continue
    fi
    hex=${line:0:64}
    if [[ ! "$hex" =~ ^[0-9a-fA-F]{64}$ ]]; then
      continue
    fi
    rest=${line:64}
    if [[ "$rest" != " "* ]]; then
      continue
    fi
    rest=${rest:1}
    if [[ "$rest" == " "* || "$rest" == \** ]]; then
      rest=${rest:1}
    fi
    name=$rest
    base=${name##*/}
    if [[ "$base" != "$asset" && ( "$include_man" != 1 || "$base" != "sway-lang.1" ) ]]; then
      continue
    fi
    printf '%s  %s\n' "$hex" "$base" >>"$dest"
    if [[ "$base" == "$asset" ]]; then
      found_asset=1
    fi
    if [[ "$base" == "sway-lang.1" ]]; then
      found_man=1
    fi
  done <"$sums"
  if [[ "$found_asset" -ne 1 ]]; then
    printf 'SHA256SUMS has no line for %s\n' "$asset" >&2
    return 1
  fi
  if [[ "$include_man" == 1 && "$found_man" -ne 1 ]]; then
    printf 'SHA256SUMS has no line for sway-lang.1\n' >&2
    return 1
  fi
}

check_release_files() {
  local tmp=$1
  local asset=$2
  local include_man=$3
  keep_release_sums "${tmp}/SHA256SUMS" "$asset" "$include_man" "${tmp}/SHA256SUMS.checked"
  (
    cd -- "$tmp"
    sha256sum --check --strict -- SHA256SUMS.checked
  )
}

# verify_detached_sums TMP KEY
# The key is the repo file, not a release asset.
verify_detached_sums() {
  local tmp=$1
  local key=$2
  local gnupg
  [[ -f "$key" ]] || die "public key not found: ${key}"
  command -v gpg >/dev/null 2>&1 || die "missing command: gpg"
  gnupg="${tmp}/gnupg"
  mkdir -p -- "$gnupg"
  chmod 700 "$gnupg"
  gpg --batch --yes --homedir "$gnupg" --import -- "$key" >/dev/null
  gpg --batch --homedir "$gnupg" --verify -- "${tmp}/SHA256SUMS.asc" "${tmp}/SHA256SUMS"
}

# download_asc URL DEST
# Returns 0 when the file was saved. Returns 1 on HTTP 404.
download_asc() {
  local url=$1
  local dest=$2
  local code
  code=$(
    curl --silent --show-error --location --proto '=https' --tlsv1.2 \
      --retry 3 --retry-delay 2 \
      --output "$dest" --write-out '%{http_code}' -- "$url" || true
  )
  if [[ "$code" == "200" ]]; then
    return 0
  fi
  rm -f -- "$dest"
  if [[ "$code" == "404" ]]; then
    return 1
  fi
  die "could not download SHA256SUMS.asc (HTTP ${code:-none})"
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
  local tmp bin_url sums_url bindir unitdir mandir script_dir man_src man_url
  local include_man=0 asc_url pubkey

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

  script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
  man_src="${script_dir}/man/sway-lang.1"
  if [[ ! -f "$man_src" ]]; then
    man_src="${tmp}/sway-lang.1"
    man_url=$(release_url "$tag" "sway-lang.1")
    printf 'downloading %s\n' "$man_url"
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
      --retry 3 --retry-delay 2 --output "$man_src" -- "$man_url"
    include_man=1
  fi

  asc_url=$(release_url "$tag" "SHA256SUMS.asc")
  pubkey="${script_dir}/install/sway-lang.asc.pub"
  # 404 is "not published yet". download_asc exits on any other HTTP failure.
  download_asc "$asc_url" "${tmp}/SHA256SUMS.asc" || true
  check_release_files "$tmp" "$asset" "$include_man"
  if [[ -f "${tmp}/SHA256SUMS.asc" ]]; then
    verify_detached_sums "$tmp" "$pubkey"
  else
    printf 'SHA256SUMS.asc is absent; continuing after the checksum\n' >&2
  fi

  bindir="${HOME}/.local/bin"
  unitdir="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user"
  mandir="${HOME}/.local/share/man/man1"
  mkdir -p -- "$bindir" "$unitdir" "$mandir"
  install -m 755 -- "${tmp}/${asset}" "${bindir}/sway-lang"
  write_unit "${tmp}/sway-lang.service"
  install -m 644 -- "${tmp}/sway-lang.service" "${unitdir}/sway-lang.service"
  install -m 644 -- "$man_src" "${mandir}/sway-lang.1"

  if command -v systemctl >/dev/null 2>&1 && [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
    systemctl --user daemon-reload
  else
    printf 'user systemd is not available in this shell. After you log in, run: systemctl --user daemon-reload\n'
  fi

  printf 'installed %s\n' "${bindir}/sway-lang"
  printf 'installed %s\n' "${unitdir}/sway-lang.service"
  printf 'installed %s\n' "${mandir}/sway-lang.1"
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
      [[ -f "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/man/sway-lang.1" ]]
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
      self_check_sums
      self_check_signature_needs_the_repo_key
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
  require_own_home
  install_release "$tag" "$asset"
}

self_check_sums() {
  local tmp asset
  tmp=$(mktemp -d)
  asset=sway-lang-linux-amd64
  printf 'one\n' >"${tmp}/${asset}"
  printf 'man\n' >"${tmp}/sway-lang.1"
  printf 'other\n' >"${tmp}/sway-lang-linux-arm64"
  (
    cd -- "$tmp"
    sha256sum "$asset" sway-lang.1 sway-lang-linux-arm64 >SHA256SUMS
  )
  keep_release_sums "${tmp}/SHA256SUMS" "$asset" 1 "${tmp}/checked"
  [[ "$(wc -l <"${tmp}/checked")" -eq 2 ]]
  if grep -q -- 'arm64' "${tmp}/checked"; then
    rm -rf -- "$tmp"
    exit 1
  fi
  (
    cd -- "$tmp"
    sha256sum --check --strict --quiet -- checked
  )
  keep_release_sums "${tmp}/SHA256SUMS" "$asset" 0 "${tmp}/binary-only"
  [[ "$(wc -l <"${tmp}/binary-only")" -eq 1 ]]
  grep -q -- "$asset" "${tmp}/binary-only"
  if grep -q -- 'sway-lang.1' "${tmp}/binary-only"; then
    rm -rf -- "$tmp"
    exit 1
  fi
  if grep -q -- 'arm64' "${tmp}/binary-only"; then
    rm -rf -- "$tmp"
    exit 1
  fi
  if keep_release_sums "${tmp}/SHA256SUMS" missing-binary 0 "${tmp}/nope" >/dev/null 2>&1; then
    rm -rf -- "$tmp"
    exit 1
  fi
  rm -rf -- "$tmp"
}

self_check_signature_needs_the_repo_key() {
  local tmp
  tmp=$(mktemp -d)
  printf 'x\n' >"${tmp}/SHA256SUMS"
  printf 'x\n' >"${tmp}/SHA256SUMS.asc"
  if (verify_detached_sums "$tmp" "${tmp}/no-such.pub" >/dev/null 2>&1); then
    rm -rf -- "$tmp"
    exit 1
  fi
  rm -rf -- "$tmp"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
