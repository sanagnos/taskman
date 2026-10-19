#!/usr/bin/env bash
# Installs the latest taskman release for this machine into ~/.local/bin.
#
#   curl -fsSL https://raw.githubusercontent.com/sanagnos/taskman/main/scripts/install.sh | bash
#
# It downloads the release over HTTPS with curl or wget, and through the GitHub CLI (gh) when that
# fails and gh is installed, which can also read a private copy of the repository.
#
# The download must match the release's checksum and the new binary must run before it replaces
# the installed one. When gh is installed and logged in, it also verifies the release's attestation.
# Run it again to update.
#
# Environment: TASKMAN_VERSION       a release tag to install instead of the latest (v1.2.3)
#              TASKMAN_BIN_DIR       where to install (default ~/.local/bin)
#              TASKMAN_INSECURE_SKIP_ATTESTATION
#                                    1: don't verify the attestation, for a network that blocks gh
#              TASKMAN_UPDATE        skip the first-run advice (older `taskman update`s set it)
#              TASKMAN_RELEASES_URL  where the release pages are (testing; default GitHub's)
set -euo pipefail

# Everything is in main, called on the last line: a download cut short runs nothing.
main() {
  # The repository: renaming it means changing this line and Cargo.toml's `repository`.
  repo=sanagnos/taskman
  # The first release that must have an attestation (vX.Y.Z): one without is refused rather than
  # installed on its checksum, as a replaced archive would have none. v0.1.0 is the first built on
  # the public repository; older ones have none and still fall back. Empty would let any release
  # fall back. REQUIRE_ATTESTATION_FROM in src/update.rs is the same switch.
  require_from=v0.1.0
  # The refs the release workflow may have run from when it signed an attestation, as a regular
  # expression: a version tag, or main, where release.sh starts it by hand for a tag. SIGNER_REFS in
  # src/update.rs is the same.
  signer_refs='refs/(tags/v[0-9]+\.[0-9]+\.[0-9]+|heads/main)'
  base="${TASKMAN_RELEASES_URL:-https://github.com/$repo/releases}"
  base="${base%/}"
  bin_dir="${TASKMAN_BIN_DIR:-$HOME/.local/bin}"
  tag="${TASKMAN_VERSION:-}"

  say() { printf '%s\n' "$*"; }
  fail() { printf 'taskman install: %s\n' "$*" >&2; exit 1; }

  # ---- which build -----------------------------------------------------------------------
  os="$(uname -s)"
  arch="$(uname -m)"
  # A shell under Rosetta reports x86_64 on Apple Silicon; the native build is the right one.
  if [ "$os" = Darwin ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ]; then
    arch=arm64
  fi
  case "$os/$arch" in
    Darwin/arm64) target=aarch64-apple-darwin ;;
    Darwin/x86_64) target=x86_64-apple-darwin ;;
    Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
    *) fail "there's no prebuilt taskman for $os/$arch yet. Build it from source instead: see https://github.com/$repo/blob/main/docs/guide.md#from-source" ;;
  esac

  # ---- tools -----------------------------------------------------------------------------
  if command -v curl >/dev/null 2>&1; then
    http=curl
  elif command -v wget >/dev/null 2>&1; then
    http=wget
  else
    http=""
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
  elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
  else
    fail "neither sha256sum nor shasum is installed, so the download can't be checked. Install coreutils (or perl's shasum), then run this again."
  fi

  # HTTPS only, redirects included; plain HTTP just for the local fake releases that
  # scripts/test-install.sh serves through TASKMAN_RELEASES_URL.
  proto='=https'
  [ -z "${TASKMAN_RELEASES_URL:-}" ] || proto='=https,http'
  # wget can't limit protocols like curl's --proto: refuse a URL that isn't HTTPS before it starts.
  # GitHub's redirects stay on HTTPS, and the checksum check follows anyway.
  https() { [ "$proto" != '=https' ] || case "$1" in https://*) ;; *) return 1 ;; esac; }

  # GET $1 into file $2.
  fetch() {
    case "$http" in
      curl) curl -fsSL --proto "$proto" -o "$2" "$1" ;;
      wget) https "$1" && wget -q -O "$2" "$1" ;;
      *) return 1 ;;
    esac
  }

  # Where $1 redirects to (GitHub's releases/latest → releases/tag/<tag>).
  location() {
    case "$http" in
      curl) curl -fsS --proto "$proto" -o /dev/null -w '%{redirect_url}' "$1" ;;
      # wget fails when it stops at a redirect; the headers it printed are what's wanted.
      wget) https "$1" || return 1; { wget -q -S --max-redirect=0 -O /dev/null "$1" 2>&1 || true; } | sed -n 's/^ *[Ll]ocation: *//p' | tr -d '\r' | tail -1 ;;
      *) return 1 ;;
    esac
  }

  # Whether release $1 is release $2 or later (vX.Y.Z both).
  at_least() {
    awk -v a="${1#v}" -v b="${2#v}" 'BEGIN {
      split(a, x, "."); split(b, y, ".")
      for (i = 1; i <= 3; i++) if (x[i] + 0 != y[i] + 0) exit !(x[i] + 0 > y[i] + 0)
      exit 0
    }'
  }

  # ---- which release, and how --------------------------------------------------------------
  tmp="$(mktemp -d)"
  staged=""
  trap 'rm -rf "$tmp"; [ -z "$staged" ] || rm -f "$staged"' EXIT

  # HTTPS needs nothing but curl or wget; gh is the fallback, and can also read a private copy.
  via=""
  if [ -z "$tag" ] && latest="$(location "$base/latest" 2>/dev/null)"; then
    tag="$(printf '%s' "$latest" | sed -n 's|.*/releases/tag/\([^/?#]*\).*|\1|p')"
  fi
  archive="taskman-$tag-$target.tar.gz"
  if [ -n "$tag" ] && fetch "$base/download/$tag/$archive" "$tmp/$archive" 2>/dev/null; then
    via=https
  # gh finds a draft by its tag for an account that can write to the repository: skip one, as
  # its files may not all be there yet.
  elif command -v gh >/dev/null 2>&1 \
    && [ "$(gh release view ${tag:+"$tag"} -R "$repo" --json isDraft --jq .isDraft 2>/dev/null)" \
      = false ]; then
    via=gh
    [ -n "$tag" ] || tag="$(gh release view -R "$repo" --json tagName --jq .tagName)"
    archive="taskman-$tag-$target.tar.gz"
    gh release download "$tag" -R "$repo" -p "$archive" -D "$tmp" >/dev/null 2>&1 \
      || fail "couldn't download $archive from release $tag through gh. Check your connection and run this again; if it keeps failing, the release may have no build for $target."
  else
    if [ -n "${TASKMAN_VERSION:-}" ]; then what="release $TASKMAN_VERSION"; else what="the latest release"; fi
    [ -n "$http" ] || fail "neither curl nor wget is installed. Install one, then run this again."
    if command -v gh >/dev/null 2>&1; then
      fail "couldn't download $what for $target from $base, nor through gh. Check your connection and that the release exists: gh release list -R $repo"
    fi
    fail "couldn't download $what for $target from $base. Check your connection and that the release exists, then run this again."
  fi
  case "$tag" in
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "couldn't tell which release to install (got '$tag')." ;;
  esac
  if [ "$via" = gh ]; then
    say "Installing taskman $tag ($target) into $bin_dir, downloaded through gh"
  else
    say "Installing taskman $tag ($target) into $bin_dir"
  fi

  # ---- verify, install -------------------------------------------------------------------
  # Downloads release asset $1 into $tmp, the same way as the archive.
  get() {
    if [ "$via" = gh ]; then
      gh release download "$tag" -R "$repo" -p "$1" -D "$tmp" >/dev/null 2>&1
    else
      fetch "$base/download/$tag/$1" "$tmp/$1" 2>/dev/null
    fi
  }

  # One SHA256SUMS per release; builds from before it have <archive>.sha256. Without either,
  # nothing is installed: a check that can be skipped checks nothing.
  if get SHA256SUMS; then
    sums="$tmp/SHA256SUMS"
  elif get "$archive.sha256"; then
    sums="$tmp/$archive.sha256"
  else
    fail "release $tag has no checksums for $archive, so it can't be checked; nothing was installed."
  fi
  expected="$(awk -v f="$archive" '$2 == f || $2 == "*" f { print tolower($1); exit }' "$sums")"
  [ -n "$expected" ] || fail "release $tag's checksums don't list $archive; nothing was installed."
  actual="$(sha256 "$tmp/$archive")"
  [ "$actual" = "$expected" ] || fail "the download doesn't match its checksum; nothing was installed."

  # The checksum comes from the same release; the attestation shows that the repository's release
  # workflow built the archive. Without gh to check it, logged out, with a gh too old for it, or for
  # a release with no attestation (before require_from), the checksum stands alone. Anything else gh
  # says refuses the archive.
  skip="${TASKMAN_INSECURE_SKIP_ATTESTATION:-}"
  unchecked=""
  if [ -n "$skip" ] && [ "$skip" != 0 ]; then
    say "Warning: TASKMAN_INSECURE_SKIP_ATTESTATION is set, so its attestation wasn't verified: only its checksum was."
  elif command -v gh >/dev/null 2>&1; then
    # Signed by the release workflow, run from a version tag or main on a GitHub-hosted runner.
    workflow_re="$(printf '%s' "https://github.com/$repo/.github/workflows/release.yml" | sed 's/[.]/\\./g')"
    status=0
    err="$(gh attestation verify "$tmp/$archive" --repo "$repo" \
      --cert-identity-regex "^$workflow_re@$signer_refs\$" --deny-self-hosted-runners 2>&1 >/dev/null)" \
      || status=$?
    if [ "$status" -ne 0 ]; then
      case "$status/$err" in
        4/* | *"HTTP 401"*) unchecked="gh isn't logged in (gh auth login) to verify its attestation" ;;
        *'unknown command "attestation"'*) unchecked="this gh has no attestation command; update gh to verify it" ;;
        # A public repository with none for this archive, and a private one (none at all).
        *"no attestations found"* | *"HTTP 404"*)
          if [ -n "$require_from" ] && at_least "$tag" "$require_from"; then
            fail "gh couldn't verify $archive's attestation (release $tag has none, though every release from $require_from on has one); nothing was installed."
          fi
          unchecked="release $tag has no attestation to verify"
          ;;
        *)
          last="$(printf '%s\n' "$err" | sed '/^[[:space:]]*$/d' | tail -1)"
          hint=""
          case "$err" in
            # gh's verdict on the archive: its signature, signer or digest.
            *"Verification failed"* | *"verifying with issuer"* | *"certificate identity"* | *"SAN"* | *"digest"*) ;;
            # Something between here and GitHub or Sigstore: the archive may well be fine.
            *"HTTP 5"[0-9][0-9]* | *"HTTP 429"* | *"rate limit"* | *"dial tcp"* | *"no such host"* | *"connection refused"* \
              | *"connection reset"* | *"i/o timeout"* | *"TLS handshake"* | *"x509:"* | *"deadline exceeded"* \
              | *"proxyconnect"* | *"network is unreachable"*)
              hint=" Try again later; if your network blocks gh, TASKMAN_INSECURE_SKIP_ATTESTATION=1 installs on the checksum alone."
              ;;
          esac
          fail "gh couldn't verify $archive's attestation (${last:-it failed without saying why}); nothing was installed.$hint"
          ;;
      esac
    fi
    [ -n "$unchecked" ] || say "Verified its attestation with gh: $repo's release workflow built it."
  else
    unchecked="the GitHub CLI (gh) isn't installed to verify its attestation"
  fi
  [ -z "$unchecked" ] || say "Checked against its checksum alone: $unchecked."

  tar xzf "$tmp/$archive" -C "$tmp" taskman || fail "couldn't unpack $archive; nothing was installed."
  mkdir -p "$bin_dir" || fail "couldn't create $bin_dir."
  # A running taskman may be using the old binary, and its sessions run it for their hooks. Write
  # the new one beside it and rename it over, so the switch is atomic and never edits the file a
  # process has open. Check it runs and is the right release first, so a bad build leaves the old
  # one.
  staged="$bin_dir/.taskman.new.$$"
  install -m 755 "$tmp/taskman" "$staged" || fail "couldn't write to $bin_dir."
  # Files a browser downloads get quarantined on macOS; curl's don't, but clearing it is harmless.
  if [ "$os" = Darwin ]; then xattr -d com.apple.quarantine "$staged" 2>/dev/null || true; fi
  installed="$("$staged" --version 2>/dev/null)" \
    || fail "the new binary doesn't run on this machine; the installed one is unchanged."
  [ "$installed" = "taskman ${tag#v}" ] \
    || fail "the new binary says it's '$installed', not taskman ${tag#v}; the installed one is unchanged."
  mv -f "$staged" "$bin_dir/taskman" || fail "couldn't replace $bin_dir/taskman."
  staged=""
  say "Installed: $installed ($bin_dir/taskman)"

  # ---- what's left to do -----------------------------------------------------------------
  case ":$PATH:" in
    *":$bin_dir:"*)
      found="$(command -v taskman || true)"
      if [ -n "$found" ] && [ "$found" != "$bin_dir/taskman" ]; then
        say "Warning: another taskman comes first on your PATH ($found)."
        say "  Remove it (for a cargo install: cargo uninstall taskman-app) or put $bin_dir earlier on PATH."
      fi
      ;;
    *)
      case "${SHELL:-}" in
        */zsh) rc="~/.zshrc" ;;
        *) rc="~/.bashrc" ;;
      esac
      say "Note: $bin_dir isn't on your PATH yet. Add it, then open a new terminal:"
      say "  echo 'export PATH=\"$bin_dir:\$PATH\"' >> $rc"
      ;;
  esac
  if ! command -v claude >/dev/null 2>&1; then
    say "Note: Claude Code (claude) isn't on your PATH. taskman runs it in every pane: install it and run 'claude' once to log in."
  fi
  # Older versions of taskman update ran this script; first-run advice is only for a first install.
  [ -n "${TASKMAN_UPDATE:-}" ] || say "Next: run 'taskman' in a wide terminal and press ctrl+a P to add your first project."
}

main "$@"
