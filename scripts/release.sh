#!/usr/bin/env bash
# Cuts a taskman release: waits for CI on main to pass, joins the changelog's entries under the
# version and date, bumps the version, checks the build, commits, tags and pushes. A failure
# before the push leaves the checkout as it was. Pushing the tag starts the Release workflow,
# which builds the binaries and publishes the release with the version's changelog section as its
# notes once CI has passed on the release commit too; if GitHub doesn't start it within three
# minutes, the script starts it by hand for the tag.
#
# Usage: scripts/release.sh 0.2.0
set -euo pipefail

version="${1:-}"
if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: scripts/release.sh X.Y.Z   (current: $(cargo pkgid | sed 's/.*[#@]//'))" >&2
  exit 1
fi
tag="v$version"
cd "$(dirname "$0")/.."

[ "$(git branch --show-current)" = main ] || { echo "Release from main." >&2; exit 1; }
[ -z "$(git status --porcelain)" ] || { echo "Commit or stash your changes first." >&2; exit 1; }
git fetch -q origin
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || { echo "main isn't in sync with origin/main; pull or push first." >&2; exit 1; }
! git rev-parse -q --verify "refs/tags/$tag" >/dev/null || { echo "$tag already exists." >&2; exit 1; }
# The release notes are this version's entries: a file for each change in changelog.d/, so pull
# requests don't all edit one place in CHANGELOG.md, and any lines left under "## Unreleased" there,
# where they were added before. So there must be some.
# The files are listed in the order they were merged: by the commit on main that last added each,
# oldest first, then by name (a merge commit adds what its branch added; a file renamed on a
# branch counts as added by the merge, not as renamed).
fragments() {
  git log --reverse --first-parent -m --no-renames --diff-filter=A --name-only --format= \
    -- changelog.d | perl -e '
    while (<STDIN>) { chomp; $at{$_} = $n++ if length }
    chomp(my @files = `git ls-files changelog.d`);
    print "$_\n" for sort { ($at{$a} // $n) <=> ($at{$b} // $n) or $a cmp $b }
      grep { $_ ne "changelog.d/README.md" } @files;'
}
unreleased="$(awk '/^## /{p = ($0 == "## Unreleased"); next} p' CHANGELOG.md)"
if [ -z "$(fragments)" ] && ! grep -q '^- ' <<<"$unreleased"; then
  echo "Nothing to release: changelog.d/ has no entries. Add what changed first (see changelog.d/README.md)." >&2
  exit 1
fi
# Each file is one or more lines of the notes' list.
bad="$(fragments | perl -ne 'chomp; open my $f, "<", $_ or die "$_: $!\n"; local $/;
  print "  $_\n" unless <$f> =~ /\A\s*- \S/')"
if [ -n "$bad" ]; then
  printf 'These files in changelog.d/ must start with "- ", as a line of CHANGELOG.md does:\n%s\n' "$bad" >&2
  exit 1
fi

# OWNER/NAME, from Cargo.toml's `repository`.
repo="$(sed -n 's|^repository = "https://github.com/\(.*\)"|\1|p' Cargo.toml)"

# "id|status|conclusion|url" of a CI run on main at a commit, pushed or started by hand
# ("null|null|null|null" when there's none): one that passed, else one still going, else the
# latest. As release.yml reads it.
ci_run() {
  gh run list -R "$repo" --workflow ci.yml --commit "$1" --branch main --limit 10 \
    --json databaseId,event,status,conclusion,url --jq '
      map(select(.event == "push" or .event == "workflow_dispatch"))
      | (map(select(.conclusion == "success")) + map(select(.status != "completed")) + .)[0] // {}
      | "\(.databaseId)|\(.status)|\(.conclusion)|\(.url)"'
}

# Waits for CI on main to pass on a commit, or exits saying why it didn't. GitHub sometimes drops
# a push event: with no run after two minutes, this starts one by hand on main, so the commit must
# be main's latest. While GitHub has trouble its answers flap: a run it listed can be missing from
# the next answer, so once a run has been seen, an empty answer or a failed gh is no news. A run
# can also stay in progress for hours after its last job ended; one whose jobs have all passed,
# and still hasn't ended ten minutes later, has passed.
wait_for_ci() {
  local sha="$1" start now run id status conclusion url passed
  local seen="" none_since="" dispatched="" passed_at="" last=""
  start=$(date +%s)
  none_since=$start
  while :; do
    run="$(ci_run "$sha")" || run=""
    IFS='|' read -r id status conclusion url <<<"$run"
    now=$(date +%s)
    case "$status" in
      completed)
        [ "$conclusion" = success ] && return
        echo "CI on main didn't pass at $sha ($conclusion): $url. Fix it or re-run it first." >&2
        exit 1
        ;;
      "" | null)
        if [ -z "$seen" ] && [ $((now - none_since)) -gt 120 ]; then
          if [ -n "$dispatched" ]; then
            echo "No CI run on main for $sha, even started by hand. Check https://www.githubstatus.com, then run this again." >&2
            exit 1
          fi
          echo "No CI run on main for $sha; starting one by hand."
          gh workflow run ci.yml -R "$repo" --ref main
          dispatched=1
          none_since=$now
        fi
        ;;
      *)
        seen="$url"
        passed="$(gh api "repos/$repo/actions/runs/$id/jobs?per_page=100" --jq '.total_count > 0
          and all(.jobs[]; .status == "completed" and (.conclusion | IN("success", "skipped")))')" \
          || passed=""
        if [ "$passed" = true ]; then
          passed_at="${passed_at:-$now}"
          if [ $((now - passed_at)) -ge 600 ]; then
            echo "Every job of CI on main passed at $sha, though GitHub still says the run is $status: $url"
            return
          fi
        else
          passed_at=""
        fi
        ;;
    esac
    if [ $((now - start)) -gt 3600 ]; then
      echo "CI on main hasn't finished at $sha: ${seen:-no run}" >&2
      exit 1
    fi
    if [ "${seen:-not started yet}" != "$last" ]; then
      last="${seen:-not started yet}"
      echo "Waiting for CI on main at $sha: $last"
    fi
    sleep 20
  done
}

# CI on main must have passed on the commit being released, main's latest (checked above). The
# release commit only adds the version and the changelog; the Release workflow waits for CI on
# that one before it publishes.
sha="$(git rev-parse HEAD)"
wait_for_ci "$sha"
echo "CI passed on main at $sha."
# main may have moved on while CI ran, and a release commit on an old main couldn't be pushed.
git fetch -q origin
[ "$(git rev-parse origin/main)" = "$sha" ] || { echo "main moved on origin while CI ran; pull and run this again." >&2; exit 1; }

# From here until the push, a failure or an interrupt puts the checkout back as it was, so the
# next run can start: the edited and deleted files, and the release commit and tag once made.
undo() {
  git tag -d "$tag" >/dev/null 2>&1 || :
  git reset -q --keep "$sha" || :
  git checkout -q "$sha" -- Cargo.toml Cargo.lock CHANGELOG.md changelog.d || :
  echo "Nothing was released; the checkout is back at $sha." >&2
}
trap undo EXIT

# The version line in [package] is the first one in Cargo.toml (perl: same behaviour on macOS and Linux).
perl -0pi -e "s/^version = \"[^\"]*\"/version = \"$version\"/m" Cargo.toml
# This version's section of CHANGELOG.md, dated, above the last one: the lines left under
# "## Unreleased", which goes, then the files in changelog.d/ in the order above, which go too.
fragments | perl -e '
  my ($version, $date) = @ARGV;
  chomp(my @files = <STDIN>);
  local $/;
  open my $in, "<", "CHANGELOG.md" or die "CHANGELOG.md: $!\n";
  my $log = <$in>;
  my @entries;
  push @entries, $1 if $log =~ s/^## Unreleased\n(.*?)(?=^## |\z)//ms;
  for (@files) { open my $f, "<", $_ or die "$_: $!\n"; push @entries, scalar <$f> }
  my $notes = join "\n", grep { length } map { s/\A\s+|\s+\z//gr } @entries;
  $log =~ s/^(?=## )/## $version - $date\n\n$notes\n\n/m
    or $log .= "\n## $version - $date\n\n$notes\n";
  open my $out, ">", "CHANGELOG.md" or die "CHANGELOG.md: $!\n";
  print $out $log;
  unlink(@files) == @files or die "Could not delete the files in changelog.d/.\n";
' "$version" "$(date +%Y-%m-%d)"
# Also updates Cargo.lock, which the release build checks with --locked. No tests: CI passed on
# this code, the release commit changes only the version and the changelog, and the Release
# workflow publishes only once CI has passed on it too. Run here, the terminal tests could fail on
# a busy machine without the rerun CI gives them, after CI had passed on the same code.
cargo build --quiet
got="$(./target/debug/taskman --version)"
[ "$got" = "taskman $version" ] || { echo "The build says \"$got\", not taskman $version." >&2; exit 1; }

git add Cargo.toml Cargo.lock CHANGELOG.md changelog.d
git commit -q -m "Release $tag"
git tag -a "$tag" -m "taskman $tag"
# Both or neither: a tag pushed without main would be a release of a commit that isn't on main.
git push -q --atomic origin main "$tag"
trap - EXIT
echo "Pushed $tag."

# The tag push should start the Release workflow. If no run for the tag has started within three
# minutes (GitHub sometimes starts it late, or not at all, for a tag push), start one by hand. A
# late push's run then finds the tag released and builds nothing.
started=""
for _ in $(seq 1 36); do
  started="$(gh run list -R "$repo" --workflow release.yml --limit 20 --json displayTitle,event \
    --jq ".[] | select(.displayTitle == \"Release $tag\") | .event" | head -1)" || started=""
  [ -n "$started" ] && break
  sleep 5
done
if [ "$started" = push ]; then
  echo "The tag push started the Release workflow."
elif [ -n "$started" ]; then
  echo "The Release workflow is already running for $tag ($started)."
else
  echo "The tag push didn't start the Release workflow; starting it by hand for $tag."
  gh workflow run release.yml -R "$repo" -f tag="$tag"
fi

# The push to main should start CI on the release commit, which the Release workflow waits for
# (and gives up on after 10 minutes with no run). If it hasn't within two minutes, start it by hand.
sha="$(git rev-parse HEAD)"
ci_started=""
for _ in $(seq 1 24); do
  case "$(ci_run "$sha" || :)" in "" | null\|*) ;; *) ci_started=1; break ;; esac
  sleep 5
done
if [ -z "$ci_started" ]; then
  echo "The push to main didn't start CI; starting it by hand on main."
  gh workflow run ci.yml -R "$repo" --ref main
fi
echo "It publishes once CI passes on the release commit."
echo "Follow the build: gh run list -R $repo --workflow release.yml --limit 1   (then gh run watch <id>)"
