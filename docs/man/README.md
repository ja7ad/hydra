# Manual pages

Eight section-1 pages (plus `hya.1`, a symlink onto `hydra.1` for the CLI's short
name), written in `man(7)` and verified against the release binary
rather than against the source comments. `mandoc -Tlint` is clean on all of them.

| Page | Covers |
|---|---|
| `hydra.1` (`hya.1`) | the download command: every top-level flag, protocols, adaptive streams (HLS/DASH quality, container, live recording), wget/curl compatibility, JSON schema, exit codes, environment, files |
| `hydra-interactive.1` | the queue manager, its keys, and what backgrounding actually does |
| `hydra-checksum.1` | advertised-digest retrieval and the trust boundary around it |
| `hydra-parity.1` | at-rest Reed–Solomon parity, and why digests come first |
| `hydra-formats.1` | the format catalogue and the category→directory mapping |
| `hydra-bench.1` | the measurement harnesses |
| `hydra-completions.1` | shell completion scripts: `completions` (print) vs `install-completions` (write + report the remaining manual step per shell) |
| `hydra-host.1` | the browser bridge behind media sniffing: native-messaging framing, the WebSocket vs host transports, registration, the request types (including `stream`), and what it can and cannot launch |

## Portability

Plain `man(7)` macros only — no `mdoc`, no GNU extensions, no UTF-8 in the source.
The macro set used is `.TH .SH .SS .TP .PP .IP .RS .RE .RI .RB .BR .BI .IB .IR .B
.I .br .nf .fi .TS .TE`, which renders identically under groff (Linux) and mandoc
(macOS, \*BSD, illumos). `hydra.1` and `hydra-formats.1` contain `tbl` tables and
therefore start with the `'\" t` preprocessor line.

## Install

```sh
./install.sh                  # into /usr/local/share/man/man1
PREFIX=~/.local ./install.sh  # into a home prefix
./install.sh --check          # lint and render only, install nothing
```

`--check` fails non-zero on any non-STYLE diagnostic, so it is usable as a CI step.
The installer refuses to install pages that do not lint.

## Distribution

End users get these pages without ever seeing this directory:

- **release tarballs** ship them as `man/*.1`; the repo-root `install.sh` copies
  them to `<prefix>/share/man/man1` and `uninstall.sh` removes them by exact
  name (never by glob — `hydra*.1` could hit the unrelated THC hydra page)
- **deb / rpm** install them gzipped under `/usr/share/man/man1`
  (`scripts/package-linux.sh`)
- **pkg** (macOS) installs them to `/usr/local/share/man/man1`, which is on the
  default man path (`scripts/package-macos-pkg.sh`)
- **DMG** carries them inside the bundle at
  `Hydra Download Manager.app/Contents/Resources/man/man1`
  (`scripts/macos-app-bundle.sh`); drag-installed apps have no install step, so
  they stay off the man path until the user copies them

Adding a page means updating the `pages=` list in `install.sh` here, the
`MAN_PAGES` list in the repo-root `uninstall.sh`, and this table — the
packaging scripts glob `docs/man/*.1` and pick it up automatically.

## Keeping them honest

The pages document **observed** behaviour. Every claim about exit codes, output
files, JSON fields, and flag effects was checked by running the release binary,
which is how the `BUGS` sections got written: nine flags parse but do nothing,
`--max-redirs` ignores its value, `--fail` still writes the error body, and
multi-file runs do not deduplicate colliding basenames. None of those are visible
from the `--help` text, and two of them (`--fail`, basename collision) are the
project's recurring failure shape — a file that exists and looks plausible.

When a flag is wired up, delete its "accepted but not implemented" note *and* its
`BUGS` paragraph. When a new flag is added, `hydra --help` is the starting point
but not the authority; run it before documenting what it does.
