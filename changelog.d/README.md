# Changelog entries

A pull request that changes something users would notice adds a file here, rather than a line in [CHANGELOG.md](../CHANGELOG.md): when every pull request edits the same place in one file, each merge leaves the others conflicting. At a release, `scripts/release.sh` moves the files' lines into CHANGELOG.md under the new version and date, in the order the files were merged, and deletes them; that section is the release's notes on GitHub.

- **Name:** what the change is about, in a few words, such as `desktop-notifications.md`, so it's unlikely to be another pull request's.
- **Content:** the line as it will read in CHANGELOG.md, starting with `- `: what changed for people using taskman, short, plain and specific, one line per paragraph. A change that needs two bullets can have both in its file. The release script refuses a file that doesn't start with `- `.
- A change nobody using taskman would notice (a refactor, tests, CI) needs none.
