#!/bin/sh
# Install (or check) hydra's manual pages.
#
#   ./install.sh                 install under $PREFIX (default /usr/local)
#   ./install.sh --check         lint and render only, install nothing
#   PREFIX=~/.local ./install.sh install into a home prefix
#
# Section 1, subdirectory man1. Both layouts in the wild put user-command pages
# there; nothing here needs an OS conditional.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
prefix=${PREFIX:-/usr/local}
mandir=${MANDIR:-$prefix/share/man}/man1
# hya.1 is a symlink to hydra.1: the CLI answers to both names, and `man hya`
# should reach the same page.
pages="hydra.1 hya.1 hydra-interactive.1 hydra-checksum.1 hydra-parity.1 hydra-formats.1 hydra-bench.1 hydra-completions.1 hydra-host.1"

# mandoc is the stricter of the two renderers and ships with macOS and the BSDs;
# groff is the Linux default. Use whichever exists, prefer mandoc for lint.
lint() {
    if command -v mandoc >/dev/null 2>&1; then
        rc=0
        for p in $pages; do
            out=$(mandoc -Tlint "$here/$p" 2>&1 | grep -v 'STYLE:' || true)
            if [ -n "$out" ]; then printf '%s\n' "$out"; rc=1; fi
        done
        return $rc
    elif command -v groff >/dev/null 2>&1; then
        rc=0
        for p in $pages; do
            # -ww turns on all warnings; anything on stderr is a defect.
            out=$(groff -man -Tascii -ww -z "$here/$p" 2>&1 || true)
            if [ -n "$out" ]; then printf '%s\n' "$out"; rc=1; fi
        done
        return $rc
    else
        echo "install.sh: neither mandoc nor groff found; skipping lint" >&2
        return 0
    fi
}

if [ "${1:-}" = "--check" ]; then
    lint && echo "man pages lint clean ($pages)"
    exit $?
fi

lint || { echo "install.sh: refusing to install pages that do not lint" >&2; exit 1; }

mkdir -p "$mandir"
for p in $pages; do
    cp "$here/$p" "$mandir/$p"
    chmod 644 "$mandir/$p"
done

echo "installed into $mandir"

# A page nobody can find is a page nobody reads. On systems with an index, tell
# the user how to rebuild it rather than doing it for them: mandb and makewhatis
# want privileges this script may not have, and a failed index is not a reason to
# unwind a successful install.
if command -v mandb >/dev/null 2>&1; then
    echo "run 'mandb' (or 'sudo mandb') to index them for apropos(1)"
elif command -v makewhatis >/dev/null 2>&1; then
    echo "run 'makewhatis $mandir' to index them for apropos(1)"
fi

case ":${MANPATH:-}:" in
    *":${MANDIR:-$prefix/share/man}:"*) ;;
    *)
        if [ "$prefix" != "/usr/local" ] && [ "$prefix" != "/usr" ]; then
            echo "note: add ${MANDIR:-$prefix/share/man} to MANPATH to make 'man hydra' work"
        fi
        ;;
esac
