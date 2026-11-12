# Official Hydra plugins

Official plugin folders are standalone Rust workspaces targeting `wasm32-wasip1`.
Language SDKs are in [sdk](sdk/README.md); `hydra-plugin` scaffolds Rust, Python,
JavaScript, C and Go projects.
Official release builds bundle signed plugins. Installers synchronize them into
Hydra's user configuration directory with their declared permissions. The deb,
rpm and Flatpak packages do this on first launch for each user. CLI and GUI load them automatically. Application upgrades
install newer bundled plugin versions without network access, preserving settings,
data and disabled state. Removed plugins stay removed across application upgrades;
manual installation remains available. Development overrides and packages signed
by other publishers are never replaced automatically.

Packaging stages signed release packages with
`python3 scripts/plugins/stage-official.py --version VERSION`. Release CI fills
`plugins/bundled/` with the packages `docs/plugins.json` lists instead
(`--from-catalog`), checking each against the catalog's checksum. `HYDRA_OFFICIAL_PLUGIN_DIR`
selects an explicit bundle and fails the build if empty. Ordinary source builds
remain usable without bundled plugins. The installer command
`hydra plugin sync-official --require-bundled` detects an incomplete build.
`uninstall.sh` retains plugin settings and data by default; `--purge` removes the
shared profile, including plugins (and respects `HYDRA_CONFIG_DIR`).
Hydra's core crates contain no service-specific extractors or host lists.

Build and package a plugin (Linux, macOS, Windows):

```sh
rustup target add wasm32-wasip1
python3 plugins/build.py hydra-youtube
hydra plugin inspect plugins/hydra-youtube/youtube-download_*.hyaplugin
hydra plugin install plugins/hydra-youtube/youtube-download_*.hyaplugin --accept-permissions
```

A package is named `{package-name}_{version}.hyaplugin`, or
`{package-name}_{version}_{os}_{arch}.hyaplugin` when it carries a native
library (`torrent-download_0.1.0_linux_x86_64.hyaplugin`). The name comes from
`[package.metadata.hydra] package-name` in the plugin's `Cargo.toml` and the
`version` in its `hydra-plugin.toml`;
`python3 scripts/plugins/plugin_release.py name PLUGIN_FOLDER` prints it.

On Windows, use `python` if that is the installed Python command. Alternatively,
run the Cargo build command from the plugin README, copy the Wasm file beside
its manifest as `plugin.wasm`, then use `hydra plugin pack FOLDER OUTPUT.hyaplugin`.
The GUI accepts the same packages and development folders in Options → Extensions & Plugins → Plugins.

GUI and CLI share installed plugins under `~/.config/hydra/plugins/{plugin_id}`
on Unix and `%APPDATA%\hydra\plugins\{plugin_id}` on Windows. `HYDRA_CONFIG_DIR`
selects an isolated profile for tests. Both fronts use the same typed settings;
the GUI presents controls and the CLI presents terminal choices.

A manifest may include `author = "Your name"` for CLI, TUI and GUI display.
This display name is separate from the publisher signing key.

A manifest may include optional top-level `welcome = "Setup instructions…"`.
Hydra shows this text after a successful installation and retains it in plugin info.
Use it for required tools or first-run steps. It is plain text, bounded like other
manifest strings, and never executes setup commands.

Installed packages can be opened from the file manager when the packaged Hydra GUI
is registered as the `.hyaplugin` handler. File opening starts permission review.
It also works with `hydra-gui --install-plugin FILE.hyaplugin` or a bare package path.

## Testing and releasing official plugins

`.github/workflows/plugins.yml` owns official plugins; the application release
workflow neither builds nor tests them.

- A pull request or a push to `main` that touches a plugin's folder runs that
  plugin's tests and builds its package. Nothing is published from a pull request.
- A plugin is released when the `version` in its `hydra-plugin.toml` is ahead
  of the one `docs/plugins.json` lists. Bump it together with `Cargo.toml` and
  `Cargo.lock`; the workflow fails if the three disagree or if a package's file
  name does not carry the manifest version.
- A release signs the packages, uploads them to the latest GitHub release,
  commits the new links, version and checksums to `docs/plugins.json` on
  `main`, and then deletes that plugin's previous packages from the release.
  Links name that release's tag, so they keep working after a newer
  application release is published.
  The catalog commit touches no path the workflow watches, so it does not start
  another run.
- Run it by hand from Actions → Plugins → Run workflow, choosing one plugin or
  all. `force_release` republishes the current version.
- `main` only accepts pull requests, so the catalog commit is pushed with the
  `PLUGIN_RELEASE_TOKEN` secret (a token whose owner may bypass the ruleset).
  Without it the workflow falls back to its own token, which is accepted only
  if GitHub Actions is on the ruleset's bypass list.

An application release downloads the packages the catalog lists, verifies their
checksums and signatures, bundles them into the installers and attaches them to
the new release next to `hydra-official-plugins.zip` and
`hydra-official-plugins.pub`.

## Official release signing

`official.pub` is the official publisher's public key. The matching
private key lives in the repository Actions secret `HYDRA_PLUGIN_SIGNING_KEY`;
it is exposed only to the signing step. Missing or mismatched keys fail the job.
CI tests signing with disposable keys and never needs the official secret.

To create a new publisher key (requires [Minisign](https://jedisct1.github.io/minisign/)):

```sh
mkdir -p ~/.config/hydra/signing
chmod 700 ~/.config/hydra/signing
minisign -G -W -p plugins/official.pub -s ~/.config/hydra/signing/official.key
chmod 600 ~/.config/hydra/signing/official.key
gh secret set HYDRA_PLUGIN_SIGNING_KEY --repo ja7ad/hydra < ~/.config/hydra/signing/official.key
```

This creates an unencrypted private key for unattended signing. Keep a secure
backup outside the repository. Do not regenerate it for each release: changing
the public key changes the publisher fingerprint shown to users on updates.
Never add private keys to Git. Key generation refuses to overwrite existing keys.

Build and sign locally with the same command used by Actions:

```sh
python3 plugins/build.py hydra-youtube
package=$(python3 scripts/plugins/plugin_release.py name hydra-youtube)
mkdir -p target/signed-plugins
HYDRA_PLUGIN_SIGNING_KEY="$(cat ~/.config/hydra/signing/official.key)" \
  target/debug/hydra-plugin sign "plugins/hydra-youtube/$package" \
    --output "target/signed-plugins/$package" \
    --public-key plugins/official.pub --key-env HYDRA_PLUGIN_SIGNING_KEY
target/debug/hydra-plugin validate "target/signed-plugins/$package"
python3 scripts/plugins/plugin_release.py check "target/signed-plugins/$package"
python3 scripts/plugins/signing-e2e.py
```

The command validates the input, adds the publisher key to a staged manifest,
recomputes all checksums, signs `SHA256SUMS`, checks the signature against
`official.pub`, and validates the final archive before writing the output.
It leaves the original manifest and unsigned package unchanged. A verified
signature proves possession of the declared publisher key; users can compare
its fingerprint with the official public key. Third-party signatures do not grant permissions; only the bundled official
publisher is trusted for automatic installation.

`hydra plugin info PLUGIN_ID` shows `Signed (verified)` and the publisher's
SHA-256 fingerprint, or `Unsigned`. Signed text is green in a terminal;
`NO_COLOR` disables color. Use `hydra plugin info PLUGIN_ID --json` for complete
metadata in scripts. The GUI's plugin information and installation review show
a verified badge and the same fingerprint. Development folders remain unsigned,
even if their manifest declares a publisher key.
