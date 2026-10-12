#!/bin/sh
# A stand-in for macOS's `security` and Linux's `secret-tool`, by the name it's run as, so tests
# never touch a real keychain. Items are files in `items/` beside it, named service-account; every
# run's arguments are appended to `calls`, so a test can check a token never was one.
dir=$(dirname "$0")
items="$dir/items"
mkdir -p "$items"
echo "$(basename "$0") $*" >>"$dir/calls"
# A file `fail` makes every run fail with its text, as a keychain that's missing or locked does.
[ ! -f "$dir/fail" ] || { cat "$dir/fail" >&2; exit 1; }

# The value after `$1` among the rest of the arguments.
after() {
    want=$1
    shift
    while [ $# -gt 1 ]; do
        [ "$1" = "$want" ] && { printf %s "$2"; return; }
        shift
    done
}

security_command() {
    cmd=$1
    shift
    service=$(after -s "$@")
    account=$(after -a "$@")
    item="$items/$service-$account"
    case $cmd in
    add-generic-password) printf %s "$(after -w "$@")" >"$item" ;;
    find-generic-password)
        [ -f "$item" ] || { echo "security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain." >&2; exit 44; }
        cat "$item"
        echo
        ;;
    delete-generic-password)
        [ -f "$item" ] || { echo "security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain." >&2; exit 44; }
        rm "$item"
        ;;
    default-keychain) echo '    "/Users/tester/Library/Keychains/login.keychain-db"' ;;
    *) echo "security: unknown command $cmd" >&2; exit 1 ;;
    esac
}

case $(basename "$0") in
security)
    if [ "$1" = "-i" ]; then
        # One command a line, as `security -i` reads them; the tests' tokens hold no `$` or quotes.
        while IFS= read -r line; do
            [ -n "$line" ] || continue
            echo "stdin: $line" >>"$dir/stdin"
            eval "set -- $line"
            security_command "$@"
        done
    else
        security_command "$@"
    fi
    ;;
secret-tool)
    cmd=$1
    shift
    item="$items/$(after service "$@")-$(after account "$@")"
    case $cmd in
    store) cat >"$item" ;;
    lookup) [ -f "$item" ] && cat "$item" || exit 1 ;;
    clear) rm -f "$item" ;;
    *) echo "secret-tool: unknown command $cmd" >&2; exit 1 ;;
    esac
    ;;
esac
