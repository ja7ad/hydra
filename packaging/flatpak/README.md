# Flatpak Packaging for Hydra

This directory contains the Flatpak package definition and AppStream metadata for **Hydra Download Manager** (`io.github.ja7ad.hydra`).

## Files

- `io.github.ja7ad.hydra.yml`: Flatpak manifest for building with `flatpak-builder`.
- `io.github.ja7ad.hydra.metainfo.xml`: AppStream 1.0+ metadata for Flathub, GNOME Software, and KDE Discover.
- `io.github.ja7ad.hydra.desktop`: XDG Desktop entry for Flatpak environments.

## Building Locally

### Prerequisites

Install Flatpak, `flatpak-builder`, and the Freedesktop 24.08 runtime and SDK:

```bash
flatpak install flathub org.freedesktop.Platform//24.08 org.freedesktop.Sdk//24.08 org.freedesktop.Sdk.Extension.rust-stable//24.08
```

### Build and Install

To build and install the Flatpak into your user installation:

```bash
flatpak-builder --user --install --force-clean build-dir packaging/flatpak/io.github.ja7ad.hydra.yml
```

### Run

```bash
flatpak run io.github.ja7ad.hydra
```

Or run via CLI:

```bash
flatpak run --command=hydra io.github.ja7ad.hydra --help
```

## Creating a Standalone Bundle (.flatpak)

To build a standalone `.flatpak` single-file installer:

```bash
# Build into local repo
flatpak-builder --repo=repo --force-clean build-dir packaging/flatpak/io.github.ja7ad.hydra.yml

# Export single-file bundle
flatpak build-bundle repo Hydra-0.4.2-x86_64.flatpak io.github.ja7ad.hydra
```

Or use the helper script:

```bash
scripts/package-flatpak.sh --bundle
```

## Flathub Submission & Maintenance

1. Fork the [Flathub repository](https://github.com/flathub/flathub).
2. Generate `cargo-sources.json` for offline cargo dependencies:
   ```bash
   flatpak-cargo-generator Cargo.lock -o packaging/flatpak/cargo-sources.json
   ```
3. Update the `sources` in `io.github.ja7ad.hydra.yml` to reference the release git tag and `cargo-sources.json`.
4. Submit a pull request to `flathub/flathub` with the `io.github.ja7ad.hydra` branch/directory.
