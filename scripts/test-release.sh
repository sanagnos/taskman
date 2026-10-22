#!/usr/bin/env bash
# Tests scripts/release.sh on a copy of a small repository whose origin is a local bare one, with
# gh, cargo, sleep and date faked: gh answers from a queue set by each test, cargo builds a fake
# binary (or fails), and sleep moves a fake clock that date reads, so an hour's wait takes no time.
# A release goes out, also when GitHub's answers flap, its CI run never ends or it starts none
# (then CI is started by hand); one that fails (main moved, a failed or interrupted build, a
# refused push) leaves the checkout as it was and releases nothing; and the Release workflow is
# started by hand only when no run for the tag is.
#
# Usage: scripts/test-release.sh   (run from anywhere)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
real_date="$(command -v date)"

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=Tester GIT_AUTHOR_EMAIL=tester@example.com
export GIT_COMMITTER_NAME=Tester GIT_COMMITTER_EMAIL=tester@example.com
export WORK="$work" REAL_DATE="$real_date"

# ---- fakes -------------------------------------------------------------------------------------
mkdir "$work/bin"
cat >"$work/bin/sleep" <<'EOF'
#!/usr/bin/env bash
echo $(( $(cat "$WORK/clock") + ${1%.*} )) >"$WORK/clock"
EOF
cat >"$work/bin/date" <<'EOF'
#!/usr/bin/env bash
if [ "$*" = +%s ]; then cat "$WORK/clock"; else exec "$REAL_DATE" "$@"; fi
EOF
# Answers from $WORK/<queue>, a line per call, the last one repeated; FAIL fails as gh would.
# A script at $WORK/on-<queue> runs once, before the first answer.
cat >"$work/bin/gh" <<'EOF'
#!/usr/bin/env bash
echo "gh $*" >>"$WORK/gh.log"
answer() {
  if [ -f "$WORK/on-$1" ]; then bash "$WORK/on-$1"; rm "$WORK/on-$1"; fi
  local line
  line="$(head -1 "$WORK/$1")"
  if [ "$(wc -l <"$WORK/$1")" -gt 1 ]; then tail -n +2 "$WORK/$1" >"$WORK/$1.next"; mv "$WORK/$1.next" "$WORK/$1"; fi
  if [ "$line" = FAIL ]; then echo "HTTP 502: Bad Gateway" >&2; exit 1; fi
  [ -z "$line" ] || echo "$line"
}
case "$*" in
  "run list"*"--workflow ci.yml"*) answer ci ;;
  "run list"*"--workflow release.yml"*) answer release ;;
  "api repos/"*"/jobs"*) answer jobs ;;
  "workflow run"*) echo "$*" >>"$WORK/dispatched" ;;
  *) echo "fake gh: unexpected: $*" >&2; exit 1 ;;
esac
EOF
# `cargo build`: the version's binary and Cargo.lock, unless $WORK/build-fails exists. A script at
# $WORK/on-build runs first.
cat >"$work/bin/cargo" <<'EOF'
#!/usr/bin/env bash
[ "$1" = build ] || { echo "fake cargo: unexpected: $*" >&2; exit 1; }
if [ -f "$WORK/on-build" ]; then bash "$WORK/on-build"; fi
if [ -f "$WORK/build-fails" ]; then echo "error: could not compile" >&2; exit 101; fi
v="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
printf 'version = "%s"\n' "$v" >Cargo.lock
mkdir -p target/debug
printf '#!/bin/sh\necho "taskman %s"\n' "$v" >target/debug/taskman
chmod +x target/debug/taskman
EOF
chmod +x "$work/bin/"*
export PATH="$work/bin:$PATH"

# ---- a repository to release from --------------------------------------------------------------
# A fresh origin and checkout, the fakes reset: CI passed, and the tag push starts the workflow.
fresh() {
  rm -rf "$work/origin.git" "$work/repo" "$work/other" "$work"/on-* "$work/build-fails" \
    "$work/dispatched" "$work/gh.log"
  echo 1000000 >"$work/clock"
  echo "7|completed|success|https://example.com/runs/7" >"$work/ci"
  echo true >"$work/jobs"
  echo push >"$work/release"
  git init -q --bare -b main "$work/origin.git"
  git init -q -b main "$work/repo"
  (
    cd "$work/repo"
    mkdir scripts changelog.d
    cp "$root/scripts/release.sh" scripts/
    printf '[package]\nname = "taskman-app"\nversion = "0.1.0"\nrepository = "https://github.com/acme/taskman"\n' >Cargo.toml
    printf 'version = "0.1.0"\n' >Cargo.lock
    printf '# Changelog\n\n## 0.1.0 - 2026-01-01\n\n- First.\n' >CHANGELOG.md
    echo "How to add an entry." >changelog.d/README.md
    echo "- Fixed a thing." >changelog.d/fixed-a-thing.md
    echo /target >.gitignore
    git add -A
    git commit -q -m "Start"
    git remote add origin "$work/origin.git"
    git push -q origin main
  )
  start="$(git -C "$work/repo" rev-parse HEAD)"
}

# Pushes a commit to origin's main from another checkout, as a merge while releasing would.
cat >"$work/move-main" <<'EOF'
git clone -q "$WORK/origin.git" "$WORK/other"
git -C "$WORK/other" commit -q --allow-empty -m "Merged meanwhile"
git -C "$WORK/other" push -q origin main
EOF

release() {
  set +e
  (cd "$work/repo" && bash scripts/release.sh 0.2.0) >"$work/out" 2>&1
  status=$?
  set -e
}

fail() {
  echo "FAIL: $*" >&2
  echo "--- release.sh said:" >&2
  cat "$work/out" >&2
  exit 1
}
passed() { echo "ok: $*"; }
said() { grep -qF -- "$1" "$work/out" || fail "$name: expected the output to say: $1"; }
waited() { echo $(( $(cat "$work/clock") - 1000000 )); }

released() {
  [ "$status" = 0 ] || fail "$name: release.sh exited $status"
  local head
  head="$(git -C "$work/origin.git" rev-parse main)"
  [ "$(git -C "$work/origin.git" rev-parse "v0.2.0^{commit}" 2>/dev/null)" = "$head" ] \
    || fail "$name: origin's v0.2.0 isn't its main"
  [ "$(git -C "$work/origin.git" log -1 --format=%s main)" = "Release v0.2.0" ] \
    || fail "$name: origin's main isn't the release commit"
  git -C "$work/origin.git" show main:CHANGELOG.md | grep -q '^## 0.2.0 - ' \
    || fail "$name: no 0.2.0 section in CHANGELOG.md"
  git -C "$work/origin.git" show main:CHANGELOG.md | grep -qx -- '- Fixed a thing.' \
    || fail "$name: the entry isn't in CHANGELOG.md"
  ! git -C "$work/origin.git" cat-file -e main:changelog.d/fixed-a-thing.md 2>/dev/null \
    || fail "$name: changelog.d/fixed-a-thing.md is still there"
  git -C "$work/origin.git" show main:Cargo.lock | grep -q '"0.2.0"' \
    || fail "$name: Cargo.lock wasn't updated"
}

# Nothing was released, and the checkout is as it was, so the next run can start.
untouched() {
  [ "$status" != 0 ] || fail "$name: release.sh succeeded"
  [ -z "$(git -C "$work/repo" status --porcelain)" ] \
    || fail "$name: the checkout was left changed: $(git -C "$work/repo" status --porcelain | xargs)"
  [ "$(git -C "$work/repo" rev-parse HEAD)" = "$start" ] || fail "$name: HEAD moved"
  ! git -C "$work/repo" rev-parse -q --verify refs/tags/v0.2.0 >/dev/null \
    || fail "$name: a local tag v0.2.0 was left"
  ! git -C "$work/origin.git" rev-parse -q --verify refs/tags/v0.2.0 >/dev/null \
    || fail "$name: origin has a tag v0.2.0"
}

# ---- tests -------------------------------------------------------------------------------------
name="a release"
fresh
release
released
said "CI passed on main at $start"
said "The tag push started the Release workflow."
[ ! -f "$work/dispatched" ] || fail "$name: started a workflow by hand: $(cat "$work/dispatched")"
passed "$name"

name="a CI run that GitHub stops listing for a while"
fresh
{
  echo "7|in_progress||https://example.com/runs/7"
  echo FAIL
  for _ in $(seq 1 20); do echo "null|null|null|null"; done
  echo "7|completed|success|https://example.com/runs/7"
} >"$work/ci"
echo false >"$work/jobs"
release
released
[ "$(waited)" -gt 300 ] || fail "$name: the run was missing for only $(waited)s"
[ ! -f "$work/dispatched" ] || fail "$name: started a workflow by hand: $(cat "$work/dispatched")"
said "Waiting for CI on main at $start: https://example.com/runs/7"
! grep -q "not started" "$work/out" || fail "$name: said the run hadn't started after seeing it"
passed "$name"

name="no CI run, then one started by hand"
fresh
{
  for _ in $(seq 1 8); do echo "null|null|null|null"; done
  echo "8|completed|success|https://example.com/runs/8"
} >"$work/ci"
release
released
said "No CI run on main for $start; starting one by hand."
[ "$(grep -c 'workflow run ci.yml -R acme/taskman --ref main' "$work/dispatched")" = 1 ] \
  || fail "$name: didn't start CI by hand once"
passed "$name"

name="no CI run, even started by hand"
fresh
echo "null|null|null|null" >"$work/ci"
release
untouched
said "No CI run on main for $start, even started by hand."
passed "$name"

name="a push to main that starts no CI run on the release commit"
fresh
printf '%s\n' "7|completed|success|https://example.com/runs/7" "null|null|null|null" >"$work/ci"
release
released
said "The push to main didn't start CI; starting it by hand on main."
grep -qx -- "workflow run ci.yml -R acme/taskman --ref main" "$work/dispatched" \
  || fail "$name: didn't start CI by hand"
passed "$name"

name="a CI run left in progress after its jobs passed"
fresh
echo "7|in_progress||https://example.com/runs/7" >"$work/ci"
release
released
said "Every job of CI on main passed at $start, though GitHub still says the run is in_progress"
[ "$(waited)" -ge 600 ] || fail "$name: passed after $(waited)s, before ten minutes of jobs passed"
passed "$name"

name="a CI run in progress with a job not passed"
fresh
echo "7|in_progress||https://example.com/runs/7" >"$work/ci"
echo false >"$work/jobs"
release
untouched
said "CI on main hasn't finished at $start: https://example.com/runs/7"
passed "$name"

name="a CI run that failed"
fresh
echo "7|completed|failure|https://example.com/runs/7" >"$work/ci"
release
untouched
said "CI on main didn't pass at $start (failure)"
passed "$name"

name="main moved while CI ran"
fresh
cp "$work/move-main" "$work/on-ci"
release
untouched
said "main moved on origin while CI ran"
passed "$name"

# Without --atomic the tag would land on origin, on a commit that isn't on main.
name="main moved while building"
fresh
cp "$work/move-main" "$work/on-build"
release
untouched
[ "$(git -C "$work/origin.git" log -1 --format=%s main)" = "Merged meanwhile" ] \
  || fail "$name: origin's main changed"
said "Nothing was released; the checkout is back at $start."
passed "$name"

name="a failed build, then a release"
fresh
touch "$work/build-fails"
release
untouched
said "error: could not compile"
said "Nothing was released; the checkout is back at $start."
rm "$work/build-fails"
release
released
passed "$name"

name="an interrupted build"
fresh
# Stops release.sh, the fake cargo's parent, while it builds.
echo 'kill -TERM "$PPID"' >"$work/on-build"
release
untouched
passed "$name"

name="a tag push that starts no Release run"
fresh
: >"$work/release"
release
released
grep -qx -- "workflow run release.yml -R acme/taskman -f tag=v0.2.0" "$work/dispatched" \
  || fail "$name: didn't start the Release workflow by hand for v0.2.0"
[ "$(waited)" -ge 180 ] || fail "$name: started it by hand after $(waited)s"
passed "$name"

name="a Release run already started by hand"
fresh
echo workflow_dispatch >"$work/release"
release
released
! grep -q release.yml "$work/dispatched" 2>/dev/null \
  || fail "$name: started the Release workflow by hand again"
said "The Release workflow is already running for v0.2.0 (workflow_dispatch)."
passed "$name"

echo "All release.sh tests passed."
