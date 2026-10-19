#!/usr/bin/env bash
# Tests scripts/install.sh, and `taskman update` when target/debug/taskman is built (or the one in
# $CARGO_TARGET_DIR), against fake releases served from a local web server laid out like GitHub's
# release pages (TASKMAN_RELEASES_URL): a good release, an older one with only <archive>.sha256,
# and broken ones that must install nothing. Then install.sh through a fake gh, its fallback when the
# HTTPS download fails, which must skip a draft. Attestations are checked with a fake gh too,
# which insists on the release workflow's identity, GitHub-hosted runners and a private folder for
# the archive: verified, failed, signed by another workflow, unreachable, missing (both where a
# release may have none and where it must have one), a gh too old for them, a logged-out gh, none at
# all, and skipped on request. Then which build install.sh picks on each OS, architecture and glibc
# (faked), and a breaking release, which `taskman update` installs when asked.
#
# Usage: scripts/test-install.sh   (needs python3; run from anywhere)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
server_pid=""
trap '[ -z "$server_pid" ] || kill "$server_pid" 2>/dev/null; rm -rf "$work"' EXIT

# This machine's build, as install.sh picks it: glibc's from install.sh's glibc_min on, else musl's.
glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null | sed -n 's/^glibc //p')" || glibc=""
glibc_min="$(sed -n 's/^ *glibc_min=//p' "$root/scripts/install.sh")"
libc=musl
if [ -n "$glibc" ] && [ "$(printf '%s\n' "$glibc_min" "$glibc" | sort -t. -k1,1n -k2,2n | head -1)" = "$glibc_min" ]; then
  libc=gnu
fi
case "$(uname -s)/$(uname -m)" in
  Darwin/*) if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ] || [ "$(uname -m)" = arm64 ]; then
              target=aarch64-apple-darwin; else target=x86_64-apple-darwin; fi ;;
  Linux/x86_64) target=x86_64-unknown-linux-$libc ;;
  Linux/aarch64) target=aarch64-unknown-linux-$libc ;;
  *) echo "skipped: no release build for this platform"; exit 0 ;;
esac
# Every release build: the fake releases have an archive for each, all running here.
targets="aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu x86_64-unknown-linux-musl aarch64-unknown-linux-gnu aarch64-unknown-linux-musl"

if command -v sha256sum >/dev/null 2>&1; then sum() { sha256sum "$@"; }; else sum() { shasum -a 256 "$@"; }; fi

# ---- fake releases -----------------------------------------------------------------------
rel="$work/releases"
# The license files a release archive carries beside the binary; an install leaves them out.
licenses="LICENSE-MIT LICENSE-APACHE LICENSE-THIRD-PARTY"
# release TAG BINARY_SCRIPT: an archive for each build holding a `taskman` that runs BINARY_SCRIPT.
release() {
  local tag="$1" dir="$rel/$1" build="$work/build/$1" t
  mkdir -p "$dir" "$build"
  printf '#!/bin/sh\n%s\n' "$2" > "$build/taskman"
  chmod 755 "$build/taskman"
  for f in $licenses; do echo "$f of $tag" > "$build/$f"; done
  for t in $targets; do
    # shellcheck disable=SC2086
    tar czf "$dir/taskman-$tag-$t.tar.gz" -C "$build" taskman $licenses
  done
}
sums() { (cd "$rel/$1" && sum taskman-*.tar.gz > SHA256SUMS); }

release v1.2.0 'echo "taskman 1.2.0"'; sums v1.2.0
# An older release, from before SHA256SUMS: one .sha256 per archive.
release v1.1.0 'echo "taskman 1.1.0"'
(cd "$rel/v1.1.0" && sum "taskman-v1.1.0-$target.tar.gz" > "taskman-v1.1.0-$target.tar.gz.sha256")
release v1.0.1 'echo "taskman 1.0.1"'                     # no checksums at all
release v1.0.2 'echo "taskman 1.0.2"'; sums v1.0.2
printf 'tampered' >> "$rel/v1.0.2/taskman-v1.0.2-$target.tar.gz"   # doesn't match its checksum
release v1.0.3 'exit 1'; sums v1.0.3                     # doesn't run
release v1.0.4 'echo "taskman 0.0.1"'; sums v1.0.4        # isn't the release it says
release v1.3.0 'echo "taskman 1.3.0"'; sums v1.3.0        # a draft, which only gh shows
touch "$rel/v1.3.0/.draft"

# GitHub redirects releases/latest to releases/tag/<tag>, and downloads to another host. The
# server answers HTTP/1.0 and closes each connection, and says so: a client that kept one open for
# the redirect would find it closed under it.
cat > "$work/server.py" <<'EOF'
import http.server, os, sys
root, latest_file = sys.argv[1], sys.argv[2]
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def send(self, code, body=b"", location=None):
        self.send_response(code)
        if location: self.send_header("Location", location)
        self.send_header("Connection", "close")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command == "GET": self.wfile.write(body)
    def do_GET(self):
        parts = self.path.split("?")[0].strip("/").split("/")
        if parts == ["releases", "latest"]:
            return self.send(302, location="/releases/tag/" + open(latest_file).read().strip())
        if parts[:2] == ["releases", "download"] and len(parts) == 4:
            return self.send(302, location="/assets/%s/%s" % (parts[2], parts[3]))
        if parts[0] == "assets" and len(parts) == 3 and ".." not in parts:
            f = os.path.join(root, parts[1], parts[2])
            if os.path.isfile(f):
                return self.send(200, open(f, "rb").read())
        if parts[:2] == ["releases", "tag"]:
            return self.send(200, b"release page")
        self.send(404)
    do_HEAD = do_GET
s = http.server.ThreadingHTTPServer(("127.0.0.1", 0), H)
print(s.server_address[1], flush=True)
s.serve_forever()
EOF
# The latest release, which a test can change: the server reads it at every request.
latest() { echo "$1" > "$work/latest"; }
latest v1.2.0
python3 -I "$work/server.py" "$rel" "$work/latest" > "$work/port" &
server_pid=$!
for _ in $(seq 50); do [ -s "$work/port" ] && break; sleep 0.1; done
export TASKMAN_RELEASES_URL="http://127.0.0.1:$(cat "$work/port")/releases"

# gh stands in for a GitHub CLI that isn't logged in, as gh says it, so only HTTPS can work and
# attestations go unchecked.
mkdir -p "$work/fakebin"
printf '#!/bin/sh\necho "To get started with GitHub CLI, please run:  gh auth login" >&2\nexit 4\n' \
  > "$work/fakebin/gh"
chmod 755 "$work/fakebin/gh"
export PATH="$work/fakebin:$PATH"
repo="$(sed -n 's/^ *repo=//p' "$root/scripts/install.sh")"

# A logged-in gh's `gh attestation verify ARCHIVE --repo REPO --cert-identity-regex IDENTITY
# --deny-self-hosted-runners`, answering as $FAKE_ATTEST says, in gh's own words. The identity is
# the release workflow run from a version tag or main; the archive must be in a folder only its
# owner can open.
repo_re="$(printf '%s' "$repo" | sed 's/[.]/\\./g')"
printf '%s\n' "^https://github\\.com/$repo_re/\\.github/workflows/release\\.yml@refs/(tags/v[0-9]+\\.[0-9]+\\.[0-9]+|heads/main)\$" \
  > "$work/identity"
mkdir -p "$work/attestbin"
printf '#!/bin/sh\nrepo="%s"\nidentity_file="%s"\n' "$repo" "$work/identity" > "$work/attestbin/gh"
cat >> "$work/attestbin/gh" <<'GH'
if [ "$1 $2" != "attestation verify" ] || [ ! -f "$3" ] || [ "$4 $5" != "--repo $repo" ] \
  || [ "$6" != "--cert-identity-regex" ] || [ "$7" != "$(cat "$identity_file")" ] \
  || [ "$8" != "--deny-self-hosted-runners" ] || [ $# -ne 8 ]; then
  echo "fake gh: unexpected arguments: $*" >&2; exit 1
fi
# install.sh verifies in its own mktemp -d folder, taskman in one it makes for the archive.
case "$(ls -ld "$(dirname "$3")")" in
  drwx------*) ;;
  *) echo "fake gh: $3 isn't in a private folder: $(ls -ld "$(dirname "$3")")" >&2; exit 1 ;;
esac
case "$FAKE_ATTEST" in
  verified) printf 'Loaded digest sha256:0 for file://%s\n✓ Verification succeeded!\n' "$3" ;;
  failed) printf '✗ Verification failed\n\nError: verifying with issuer "sigstore.dev"\n' >&2; exit 1 ;;
  none) printf '\nError: no attestations found\n' >&2; exit 1 ;;
  private) printf '\nError: HTTP 404: Not Found (https://api.github.com/repos/%s/attestations/sha256:0)\n' "$repo" >&2; exit 1 ;;
  old) printf 'unknown command "attestation" for "gh"\n\nUsage:  gh <command> <subcommand> [flags]\n' >&2; exit 1 ;;
  # Built by another workflow, or another repository, named to look like a network error.
  signer) printf '✗ Verification failed\n\nError: verifying with issuer "sigstore.dev": failed to verify certificate identity: expected SAN to match "https://github.com/%s/.github/workflows/release.yml", got "https://github.com/acme/dial-tcp/.github/workflows/no-such-host.yml"\n' "$repo" >&2; exit 1 ;;
  network) printf 'Error: failed to create TUF client: Get "https://tuf-repo-cdn.sigstore.dev/": dial tcp: lookup tuf-repo-cdn.sigstore.dev: no such host\n' >&2; exit 1 ;;
  busy) printf '\nError: HTTP 503: Service Unavailable (https://api.github.com/repos/%s/attestations/sha256:0)\n' "$repo" >&2; exit 1 ;;
  limited) printf '\nError: HTTP 429: Too Many Requests (https://api.github.com/)\n' >&2; exit 1 ;;
  *) echo "fake gh: FAKE_ATTEST is '$FAKE_ATTEST'" >&2; exit 1 ;;
esac
GH
chmod 755 "$work/attestbin/gh"

# Only the tools the script and taskman update use: with curl and wget, without gh.
tools="bash uname sysctl getconf sha256sum shasum cut sed tr tail mktemp rm awk tar gzip mkdir install xattr mv"
mkdir -p "$work/nogh"
for t in $tools curl wget; do
  p="$(command -v "$t" || true)"
  [ -z "$p" ] || ln -s "$p" "$work/nogh/$t"
done

failures=0
check() { # NAME EXPECTED_STATUS EXPECTED_VERSION_AFTER COMMAND...
  local name="$1" want="$2" version="$3"; shift 3
  checked="$name"
  local status=0
  "$@" > "$work/out" 2>&1 || status=$?
  local got; got="$("$bin/${bin_name:-taskman}" --version 2>/dev/null || echo none)"
  if [ "$status" -ne "$want" ] || [ "$got" != "$version" ]; then
    echo "FAIL $name: exit $status (want $want), installed '$got' (want '$version')"
    sed 's/^/    /' "$work/out"
    failures=$((failures + 1))
  else
    echo "ok   $name: $(tail -1 "$work/out")"
  fi
}
said() { # TEXT: what the last check's output must include
  if ! grep -qF -- "$1" "$work/out"; then
    echo "FAIL $checked: didn't say '$1'"
    sed 's/^/    /' "$work/out"
    failures=$((failures + 1))
  fi
}
not_said() { # TEXT: what the last check's output must not include
  if grep -qF -- "$1" "$work/out"; then
    echo "FAIL $checked: said '$1'"
    sed 's/^/    /' "$work/out"
    failures=$((failures + 1))
  fi
}
skip_var=TASKMAN_INSECURE_SKIP_ATTESTATION
# earlier vX.Y.Z: a release before it.
earlier() {
  local v="${1#v}"
  local x="${v%%.*}" rest="${v#*.}"
  local y="${rest%%.*}" z="${rest#*.}"
  if [ "$z" -gt 0 ]; then echo "v$x.$y.$((z - 1))"
  elif [ "$y" -gt 0 ]; then echo "v$x.$((y - 1)).99"
  else echo "v$((x - 1)).99.99"; fi
}

# ---- install.sh --------------------------------------------------------------------------
bin="$work/bin"
inst() { env TASKMAN_BIN_DIR="$bin" "$@" bash "${script:-$root/scripts/install.sh}"; }
# The script's `require_from`, the first release that must have an attestation.
require_from="$(sed -n 's/^ *require_from=//p' "$root/scripts/install.sh")"
# requiring VALUE: install.sh from now on is a copy that requires attestations from release VALUE
# on, or from none when it's empty; the same script otherwise.
requiring() {
  script="$work/install-requiring-${1:-none}.sh"
  perl -pe "s/^(\\s*)require_from=.*\$/\${1}require_from=$1/" "$root/scripts/install.sh" > "$script"
  if [ "$(sed -n 's/^ *require_from=//p' "$script")" != "$1" ]; then
    echo "FAIL install.sh has no require_from= line to set to '$1'"
    failures=$((failures + 1))
  fi
}
check "installs the latest" 0 "taskman 1.2.0" inst
said "Checked against its checksum alone: gh isn't logged in"
check "installs an older release by its .sha256" 0 "taskman 1.1.0" inst TASKMAN_VERSION=v1.1.0
check "refuses a release without checksums" 1 "taskman 1.1.0" inst TASKMAN_VERSION=v1.0.1
check "refuses a download that doesn't match" 1 "taskman 1.1.0" inst TASKMAN_VERSION=v1.0.2
check "keeps the old binary when the new one fails" 1 "taskman 1.1.0" inst TASKMAN_VERSION=v1.0.3
check "keeps the old binary when the new one is another version" 1 "taskman 1.1.0" inst TASKMAN_VERSION=v1.0.4
check "says when a release doesn't exist" 1 "taskman 1.1.0" inst TASKMAN_VERSION=v9.9.9
if command -v wget >/dev/null 2>&1; then
  # Only wget: the tools the script uses, without curl.
  mkdir -p "$work/nocurl"
  for t in $tools wget; do
    p="$(command -v "$t" || true)"
    [ -z "$p" ] || ln -s "$p" "$work/nocurl/$t"
  done
  ln -s "$work/fakebin/gh" "$work/nocurl/gh"
  check "installs the latest with wget" 0 "taskman 1.2.0" inst PATH="$work/nocurl"
fi
# A gh that can read the repository and serves the fake releases (the latest is v1.2.0), while
# HTTPS reaches nothing.
mkdir -p "$work/ghbin"
printf '#!/bin/sh\nrel="%s"\n' "$rel" > "$work/ghbin/gh"
cat >> "$work/ghbin/gh" <<'GH'
case "$1 $2" in
  "release view")
    case "$3" in -R) tag=v1.2.0 ;; *) tag="$3" ;; esac
    [ -d "$rel/$tag" ] || { echo "release not found" >&2; exit 1; }
    case "$*" in
      *isDraft*) if [ -e "$rel/$tag/.draft" ]; then echo true; else echo false; fi ;;
      *tagName*) echo "$tag" ;;
    esac ;;
  "release download")
    tag="$3"; shift 3
    while [ $# -gt 0 ]; do
      case "$1" in -p) name="$2"; shift 2 ;; -D) dir="$2"; shift 2 ;; *) shift ;; esac
    done
    cp "$rel/$tag/$name" "$dir/" ;;
  # A private repository has no attestations.
  "attestation verify") echo "Error: HTTP 404: Not Found (https://api.github.com/)" >&2; exit 1 ;;
  *) exit 1 ;;
esac
GH
chmod 755 "$work/ghbin/gh"
viagh() { inst TASKMAN_RELEASES_URL=http://127.0.0.1:1/releases PATH="$work/ghbin:$PATH" "$@"; }
# Where a release may have no attestation, which the fake releases (v1.x, all from require_from on)
# don't have: the script as it was while the repository was private.
requiring ""
check "installs a release through gh" 0 "taskman 1.1.0" viagh TASKMAN_VERSION=v1.1.0
said "release v1.1.0 has no attestation to verify"
check "installs the latest through gh" 0 "taskman 1.2.0" viagh
check "skips a draft through gh" 1 "taskman 1.2.0" viagh TASKMAN_VERSION=v1.3.0
# Attestations, through a logged-in gh.
attest() { local mode="$1"; shift; inst PATH="$work/attestbin:$PATH" FAKE_ATTEST="$mode" "$@"; }
check "verifies the attestation with gh" 0 "taskman 1.1.0" attest verified TASKMAN_VERSION=v1.1.0
said "Verified its attestation with gh: $repo's release workflow built it."
check "refuses an archive that fails its attestation" 1 "taskman 1.1.0" attest failed
said "gh couldn't verify taskman-v1.2.0-$target.tar.gz's attestation (Error: verifying with issuer"
not_said "$skip_var"
check "refuses an archive another workflow built" 1 "taskman 1.1.0" attest signer
said "failed to verify certificate identity"
not_said "$skip_var"
check "refuses when gh can't reach Sigstore, and names the way around" 1 "taskman 1.1.0" attest network
said "no such host); nothing was installed. Try again later; if your network blocks gh, $skip_var=1 installs on the checksum alone."
check "refuses when GitHub fails, and names the way around" 1 "taskman 1.1.0" attest busy
said "$skip_var=1"
check "refuses when GitHub limits gh, and names the way around" 1 "taskman 1.1.0" attest limited
said "$skip_var=1"
check "installs on the checksum alone when told to skip the attestation" 0 "taskman 1.2.0" attest failed "$skip_var=1"
said "Warning: $skip_var is set, so its attestation wasn't verified: only its checksum was."
check "still checks the attestation when the skip is 0" 1 "taskman 1.2.0" attest failed "$skip_var=0" TASKMAN_VERSION=v1.1.0
check "still refuses a mismatched checksum when skipping the attestation" 1 "taskman 1.2.0" attest none "$skip_var=1" TASKMAN_VERSION=v1.0.2
check "installs on the checksum alone without an attestation" 0 "taskman 1.2.0" attest none
said "Checked against its checksum alone: release v1.2.0 has no attestation to verify."
check "installs on the checksum alone from a private repository" 0 "taskman 1.1.0" attest private TASKMAN_VERSION=v1.1.0
said "release v1.1.0 has no attestation to verify"
check "installs on the checksum alone with a gh too old to verify" 0 "taskman 1.2.0" attest old
said "this gh has no attestation command"
check "installs on the checksum alone without gh" 0 "taskman 1.1.0" inst PATH="$work/nogh" TASKMAN_VERSION=v1.1.0
said "the GitHub CLI (gh) isn't installed to verify its attestation"
check "still refuses a mismatched checksum when gh verifies" 1 "taskman 1.1.0" attest verified TASKMAN_VERSION=v1.0.2
# Where every release from v1.2.0 on must have one, so v1.1.0 is from before.
requiring v1.2.0
check "refuses a release with no attestation once they're required" 1 "taskman 1.1.0" attest none
said "(release v1.2.0 has none, though every release from v1.2.0 on has one); nothing was installed."
not_said "$skip_var"
check "refuses a private repository's release once they're required" 1 "taskman 1.1.0" attest private
check "installs an attested release once they're required" 0 "taskman 1.2.0" attest verified
check "installs an older release without one once they're required" 0 "taskman 1.1.0" attest none TASKMAN_VERSION=v1.1.0
said "release v1.1.0 has no attestation to verify"
check "installs on the checksum alone without gh once they're required" 0 "taskman 1.2.0" inst PATH="$work/nogh"
script=""
# The script as it is: it requires them.
[ -n "$require_from" ] || { echo "FAIL install.sh doesn't require attestations (require_from is empty)"; failures=$((failures + 1)); }
check "refuses a release with no attestation from require_from on" 1 "taskman 1.2.0" attest none TASKMAN_VERSION=v1.1.0
said "(release v1.1.0 has none, though every release from $require_from on has one); nothing was installed."
[ ! -e "$bin"/.taskman.new.* ] || { echo "FAIL a staged binary was left behind"; failures=$((failures + 1)); }
[ ! -e "$bin"/LICENSE-* ] || { echo "FAIL the archive's license files were installed beside the binary"; failures=$((failures + 1)); }

# Which build install.sh picks, on machines faked with uname, getconf and sysctl: $FAKE_OS and
# $FAKE_ARCH for uname -s and -m, glibc $FAKE_GLIBC (none when empty, as with musl), and Apple
# Silicon under Rosetta when $FAKE_ARM64 is 1.
mkdir -p "$work/platform"
cat > "$work/platform/uname" <<'SH'
#!/bin/sh
case "$1" in -s) echo "$FAKE_OS" ;; -m) echo "$FAKE_ARCH" ;; *) echo "fake uname: $*" >&2; exit 1 ;; esac
SH
cat > "$work/platform/getconf" <<'SH'
#!/bin/sh
if [ "$1" = GNU_LIBC_VERSION ] && [ -n "$FAKE_GLIBC" ]; then echo "glibc $FAKE_GLIBC"; exit 0; fi
echo "getconf: $1: unknown variable" >&2; exit 1
SH
cat > "$work/platform/sysctl" <<'SH'
#!/bin/sh
if [ "$*" = "-n hw.optional.arm64" ] && [ "$FAKE_OS" = Darwin ]; then echo "${FAKE_ARM64:-0}"; exit 0; fi
echo "sysctl: unknown oid '$*'" >&2; exit 1
SH
chmod 755 "$work/platform/uname" "$work/platform/getconf" "$work/platform/sysctl"
bin="$work/platformbin"
on() { # OS ARCH GLIBC ARM64
  rm -f "$bin/taskman"
  inst PATH="$work/platform:$PATH" FAKE_OS="$1" FAKE_ARCH="$2" FAKE_GLIBC="$3" FAKE_ARM64="$4"
}
picks() { # TARGET OS ARCH GLIBC [ARM64]
  check "picks $1 on $2/$3${4:+ with glibc $4}${5:+ under Rosetta}" 0 "taskman 1.2.0" on "$2" "$3" "$4" "${5:-}"
  said "Installing taskman v1.2.0 ($1) into"
}
picks x86_64-unknown-linux-gnu Linux x86_64 2.39
picks x86_64-unknown-linux-gnu Linux x86_64 "$glibc_min"
picks x86_64-unknown-linux-gnu Linux x86_64 2.100
picks x86_64-unknown-linux-musl Linux x86_64 2.31
picks x86_64-unknown-linux-musl Linux x86_64 ""
picks aarch64-unknown-linux-gnu Linux aarch64 2.35
picks aarch64-unknown-linux-musl Linux aarch64 2.26
picks aarch64-unknown-linux-musl Linux aarch64 ""
picks aarch64-apple-darwin Darwin arm64 ""
picks aarch64-apple-darwin Darwin x86_64 "" 1
picks x86_64-apple-darwin Darwin x86_64 ""
check "refuses a platform with no build" 1 none on Linux armv7l 2.36 ""
said "there's no prebuilt taskman for Linux/armv7l yet. Build it from source instead"
check "refuses an OS with no build" 1 none on FreeBSD amd64 "" ""
said "there's no prebuilt taskman for FreeBSD/amd64 yet."

# ---- taskman update ----------------------------------------------------------------------
exe="${CARGO_TARGET_DIR:-$root/target}/debug/taskman"
if [ -x "$exe" ]; then
  version="$("$exe" --version)"; tag="v${version#taskman }"
  mkdir -p "$rel/$tag"
  # shellcheck disable=SC2086
  tar czf "$rel/$tag/taskman-$tag-$target.tar.gz" -C "$(dirname "$exe")" taskman -C "$root" $licenses
  sums "$tag"
  latest "$tag"

  # A copy outside target/, so it isn't a development build; its state kept apart.
  bin="$work/updbin"; mkdir -p "$bin" "$work/state"
  cp "$exe" "$bin/taskman"
  upd() { env XDG_STATE_HOME="$work/state" "$bin/taskman" update --force; }
  check "taskman update installs the latest" 0 "$version" upd
  said "Checked against its checksum alone: gh isn't logged in"
  [ ! -e "$bin"/LICENSE-* ] || { echo "FAIL taskman update put the archive's license files beside the binary"; failures=$((failures + 1)); }
  printf '%064d  %s\n' 0 "taskman-$tag-$target.tar.gz" > "$rel/$tag/SHA256SUMS"
  check "taskman update refuses a download that doesn't match" 1 "$version" upd
  rm "$rel/$tag/SHA256SUMS"
  check "taskman update refuses a release without checksums" 1 "$version" upd
  sums "$tag"

  # Attestations: what gh says, or that there's no gh. A refused archive leaves the file as it was.
  upd_attest() { env FAKE_ATTEST="$1" PATH="$2" XDG_STATE_HOME="$work/state" "$bin/taskman" update --force; }
  inode() { ls -i "$bin/${bin_name:-taskman}" | awk '{print $1}'; }
  check "taskman update verifies the attestation with gh" 0 "$version" upd_attest verified "$work/attestbin:$PATH"
  said "Verified its attestation with gh: $repo's release workflow built it."
  before="$(inode)"
  check "taskman update refuses an archive that fails its attestation" 1 "$version" upd_attest failed "$work/attestbin:$PATH"
  said "gh couldn't verify taskman-$tag-$target.tar.gz's attestation (Error: verifying with issuer"
  not_said "$skip_var"
  [ "$(inode)" = "$before" ] || { echo "FAIL taskman update replaced the binary after a failed attestation"; failures=$((failures + 1)); }
  check "taskman update installs on the checksum alone with a gh too old to verify" 0 "$version" upd_attest old "$work/attestbin:$PATH"
  said "this gh has no attestation command"
  check "taskman update installs on the checksum alone without gh" 0 "$version" upd_attest "" "$work/nogh"
  said "the GitHub CLI (gh) isn't installed to verify its attestation"
  before="$(inode)"
  check "taskman update refuses an archive another workflow built" 1 "$version" upd_attest signer "$work/attestbin:$PATH"
  said "failed to verify certificate identity"
  not_said "$skip_var"
  check "taskman update refuses when gh can't reach Sigstore, and names the way around" 1 "$version" upd_attest network "$work/attestbin:$PATH"
  said "no such host); nothing was installed. Try again later; if your network blocks gh, $skip_var=1 installs on the checksum alone"
  check "taskman update refuses when GitHub fails, and names the way around" 1 "$version" upd_attest busy "$work/attestbin:$PATH"
  said "$skip_var=1"
  [ "$(inode)" = "$before" ] || { echo "FAIL taskman update replaced the binary after a refused attestation"; failures=$((failures + 1)); }
  skip_upd() { env "$skip_var=1" FAKE_ATTEST=failed PATH="$work/attestbin:$PATH" XDG_STATE_HOME="$work/state" "$bin/taskman" update --force; }
  check "taskman update installs on the checksum alone when told to skip the attestation" 0 "$version" skip_upd
  said "Warning: $skip_var is set, so its attestation wasn't verified: only its checksum was."

  # A release with no attestation: refused or not by its tag against REQUIRE_ATTESTATION_FROM,
  # which the binary has compiled in and nothing can change, and which a unit test keeps equal to
  # install.sh's require_from. So the releases are what changes: one from before it, and the first
  # that must have one. Their binaries are stand-ins that only say their version, so each update
  # starts from the real one again.
  if [ -n "$require_from" ]; then
    before_require="$(earlier "$require_from")"
    for t in "$before_require" "$require_from"; do
      [ -d "$rel/$t" ] || { release "$t" "echo \"taskman ${t#v}\""; sums "$t"; }
    done
    from() { latest "$1"; cp "$exe" "$bin/taskman"; }
    from "$before_require"
    check "taskman update installs a release from before require_from on the checksum alone" 0 "taskman ${before_require#v}" upd_attest none "$work/attestbin:$PATH"
    said "Checked against its checksum alone: release $before_require has no attestation to verify."
    from "$require_from"
    before="$(inode)"
    check "taskman update refuses a release from require_from on with no attestation" 1 "$version" upd_attest none "$work/attestbin:$PATH"
    said "gh couldn't verify taskman-$require_from-$target.tar.gz's attestation (release $require_from has none, though every release from $require_from on has one); nothing was installed"
    not_said "$skip_var"
    check "taskman update refuses a private repository's release from require_from on" 1 "$version" upd_attest private "$work/attestbin:$PATH"
    [ "$(inode)" = "$before" ] || { echo "FAIL taskman update replaced the binary after a missing attestation"; failures=$((failures + 1)); }
    check "taskman update installs an attested release from require_from on" 0 "taskman ${require_from#v}" upd_attest verified "$work/attestbin:$PATH"
    from "$require_from"
    check "taskman update installs a release from require_from on without gh" 0 "taskman ${require_from#v}" upd_attest "" "$work/nogh"
    said "the GitHub CLI (gh) isn't installed to verify its attestation"
    from "$tag"
  fi

  # A new minor version is a breaking release: taskman update installs it when asked, and says so.
  x="${tag#v}"; minor="${x#*.}"; minor="${minor%%.*}"
  next="v${x%%.*}.$((minor + 1)).0"
  release "$next" "echo \"taskman ${next#v}\""; sums "$next"
  latest "$next"
  check "taskman update installs a breaking release when asked" 0 "taskman ${next#v}" upd_attest verified "$work/attestbin:$PATH"
  said "$next has breaking changes: see its notes,"
  latest "$tag"; cp "$exe" "$bin/taskman"

  # A copy in cargo's bin folder is cargo's to update, unless --force.
  bin="$work/cargo/bin"; mkdir -p "$bin"
  cp "$exe" "$bin/taskman"
  before="$(inode)"
  cargo_upd() { env CARGO_HOME="$work/cargo" XDG_STATE_HOME="$work/state" "$bin/taskman" update "$@"; }
  check "taskman update leaves a cargo install to cargo" 1 "$version" cargo_upd
  if ! grep -q "cargo install" "$work/out" || [ "$(inode)" != "$before" ]; then
    echo "FAIL taskman update replaced a cargo install, or didn't say how to update it"
    failures=$((failures + 1))
  fi
  check "taskman update --force replaces a cargo install" 0 "$version" cargo_upd --force
  if [ "$(inode)" = "$before" ]; then
    echo "FAIL taskman update --force didn't replace the cargo install"
    failures=$((failures + 1))
  fi

  # A copy installed under another name updates that file, and leaves no `taskman` beside it.
  bin="$work/renamed"; mkdir -p "$bin"
  cp "$exe" "$bin/tm"
  bin_name=tm
  renamed_upd() { env XDG_STATE_HOME="$work/state" "$bin/tm" update --force; }
  check "taskman update keeps the binary's own name" 0 "$version" renamed_upd
  if [ -e "$bin/taskman" ] || [ -n "$(ls -A "$bin" | grep -v '^tm$')" ]; then
    echo "FAIL taskman update left something beside the renamed binary: $(ls -A "$bin" | tr '\n' ' ')"
    failures=$((failures + 1))
  fi
  bin_name=taskman
fi

[ "$failures" -eq 0 ] || { echo "$failures failed"; exit 1; }
echo "all passed"
