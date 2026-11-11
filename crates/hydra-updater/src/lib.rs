// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hydra self-update: release check, asset download, archive extraction and
//! the swap-in-place step.
//!
//! One library serves three callers:
//! - **hydra-gui** — startup check, the update dialog's download-with-progress,
//!   extraction, and launching the finisher binary.
//! - **hydra** (CLI) — `hydra update`, which only reports the newer version
//!   and the download link.
//! - **hydra-updater** (this crate's own bin) — the finisher that runs after
//!   the app exits and copies the new files into place.
//!
//! The release endpoint defaults to the real GitHub API but is overridable via
//! the `HYDRA_UPDATE_API` environment variable, which is how the whole flow is
//! exercised against a local mock server (see `examples/mock_server.rs` and
//! `tests/mock_flow.rs`) before it is pointed at production releases.

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub mod http;

/// GitHub repository the updater checks, `owner/name`.
pub const REPO: &str = "ja7ad/hydra";

/// Real release API. Override with `HYDRA_UPDATE_API=http://127.0.0.1:8642`
/// to point every updater consumer (GUI, CLI, tests) at a mock server.
pub const DEFAULT_API_BASE: &str = "https://api.github.com";

/// Environment variable that overrides [`DEFAULT_API_BASE`].
pub const API_ENV: &str = "HYDRA_UPDATE_API";

/// The API base currently in effect (env override or the GitHub default).
pub fn api_base() -> String {
    std::env::var(API_ENV)
        .ok()
        .and_then(|v| accepted_api_base(&v))
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
}

/// The base an `HYDRA_UPDATE_API` value amounts to, or `None` when it is
/// empty or not safe to honour. The variable is read in release builds too
/// (the mock flow depends on it), so a plain `http://` base is accepted on
/// loopback only: anywhere else, whoever controls that environment or the
/// network between could hand this process an executable to install.
fn accepted_api_base(raw: &str) -> Option<String> {
    let v = raw.trim().trim_end_matches('/');
    if v.is_empty() {
        return None;
    }
    let url = http::Url::parse(v).ok()?;
    (url.tls || url.is_loopback()).then(|| v.to_string())
}

// -------------------------------------------------------------- release data

/// One downloadable file attached to a release.
#[derive(Clone, Debug, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub size: u64,
}

/// The subset of the GitHub release object the updater needs.
#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    /// `v0.2.4` — the leading `v` is the tag convention, not the version.
    pub tag_name: String,
    /// GitHub's pre-release flag; `-rc` tags are published with it set.
    #[serde(default)]
    pub prerelease: bool,
    /// Human release title (`hydra 0.2.4`).
    #[serde(default)]
    pub name: String,
    /// Release notes, GitHub-flavoured markdown.
    #[serde(default)]
    pub body: String,
    /// Web page of the release, for "open in browser" links.
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub published_at: String,
    #[serde(default)]
    pub assets: Vec<ReleaseAsset>,
}

impl Release {
    /// Version without the tag's `v` prefix.
    pub fn version(&self) -> &str {
        self.tag_name.trim_start_matches('v')
    }

    /// A pre-release by GitHub's flag or by its tag: `v0.3.0-rc2` is one
    /// whether or not the box was ticked when it was published.
    pub fn is_prerelease(&self) -> bool {
        self.prerelease || parse_version(self.version()).is_some_and(|(_, rank)| rank != u64::MAX)
    }

    pub fn asset(&self, name: &str) -> Option<&ReleaseAsset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// The GUI bundle for this machine, `None` when the release ships none.
    pub fn gui_asset(&self) -> Option<&ReleaseAsset> {
        self.asset_for(gui_asset_name)
    }

    /// The Windows portable bundle for this machine, `None` when the release
    /// ships none.
    pub fn portable_asset(&self) -> Option<&ReleaseAsset> {
        self.asset_for(portable_asset_name)
    }

    /// The standalone CLI archive for this machine.
    pub fn cli_asset(&self) -> Option<&ReleaseAsset> {
        self.asset_for(cli_asset_name)
    }

    /// The installable this machine's packaging uses — `.deb`, `.rpm`,
    /// `.dmg`, `.pkg` or the Windows setup — for installs Hydra must not
    /// replace file by file.
    ///
    /// Matched by shape rather than by an exact name ([`PackageKind::asset_shape`]).
    pub fn package_asset(&self) -> Option<&ReleaseAsset> {
        let (prefix, ext, arch) = package_kind()?.asset_shape();
        self.package_asset_for(prefix, ext, arch)
    }

    /// The `.AppImage` for this machine, when the release ships one.
    ///
    /// Matched by shape like [`package_asset`](Self::package_asset) rather
    /// than by an exact name: the file carries the workspace version, which
    /// a pre-release tag does not have to agree with.
    pub fn appimage_asset(&self) -> Option<&ReleaseAsset> {
        self.package_asset_for("Hydra-", ".AppImage", appimage_arch())
    }

    fn package_asset_for(&self, prefix: &str, ext: &str, arch: &str) -> Option<&ReleaseAsset> {
        self.assets
            .iter()
            .find(|a| a.name.starts_with(prefix) && a.name.ends_with(ext) && a.name.contains(arch))
    }

    /// Asset lookup across both spellings of the release version: the tag's
    /// first, then the tag without its pre-release suffix. The pipeline
    /// names assets from the workspace manifest version, which may stay on
    /// the plain version while a candidate is cut from a `v0.3.2-rc` tag
    /// (.github/workflows/release.yml accepts either spelling) — that
    /// release ships `hydra-0.3.2-…` assets, which the tag version alone
    /// never matches.
    fn asset_for(&self, name_of: fn(&str) -> String) -> Option<&ReleaseAsset> {
        let v = self.version();
        if let Some(a) = self.asset(&name_of(v)) {
            return Some(a);
        }
        let core = version_core(v);
        (core != v).then(|| self.asset(&name_of(core))).flatten()
    }
}

/// `latest` is strictly newer than `current`, comparing dotted numeric parts
/// (`v` prefixes and any `+build` suffix are ignored). On an equal core a
/// stable release outranks any `-rc` build and rc builds order by their
/// number: `0.3.0 > 0.3.0-rc2 > 0.3.0-rc1`. Unparsable versions compare as
/// not-newer — an updater must never "upgrade" onto a version it cannot
/// read.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// `((maj, min, pat), pre_rank)` — `pre_rank` is `u64::MAX` for a stable
/// version and the suffix's number for a pre-release (`-rc2` → 2, bare
/// `-rc` → 0), so tuple order IS release order.
fn parse_version(v: &str) -> Option<((u64, u64, u64), u64)> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split('+').next().unwrap_or("");
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut it = core.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next().unwrap_or("0").parse().ok()?;
    let pat = it.next().unwrap_or("0").parse().ok()?;
    let rank = match pre {
        None => u64::MAX,
        Some(p) => {
            let digits: String = p.chars().filter(|c| c.is_ascii_digit()).collect();
            digits.parse().unwrap_or(0)
        }
    };
    Some(((maj, min, pat), rank))
}

// ----------------------------------------------------------------- platform

/// OS tag as the release pipeline spells it (`linux` | `macos` | `windows`).
pub fn os_tag() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// Architecture tag as the release pipeline spells it (`amd64` | `arm64`).
pub fn arch_tag() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    }
}

/// Archive format per platform: the pipeline zips Windows, tars the rest.
pub fn archive_ext() -> &'static str {
    if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    }
}

/// The Linux packages' name — not `hydra`, which is the THC login cracker
/// in distro repos (scripts/package-linux.sh).
const PACKAGE_NAME: &str = "hydra-download-manager";

/// The installable this machine's packaging uses, offered when Hydra cannot
/// replace its own files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    Deb,
    Rpm,
    Dmg,
    Pkg,
    Exe,
}

impl PackageKind {
    /// `(asset name prefix, asset name suffix, architecture token)` — how
    /// the release pipeline spells this format's file. The version sits
    /// between prefix and arch and is deliberately not matched: every format
    /// spells it its own way (`0.3.4~rc` for deb, a separate release field
    /// for rpm).
    fn asset_shape(self) -> (&'static str, &'static str, &'static str) {
        match self {
            PackageKind::Deb => (PACKAGE_NAME, ".deb", arch_tag()),
            PackageKind::Rpm => (PACKAGE_NAME, ".rpm", rpm_arch()),
            PackageKind::Dmg => ("Hydra-", ".dmg", macos_arch()),
            PackageKind::Pkg => ("Hydra-", ".pkg", macos_arch()),
            PackageKind::Exe => ("hydra-", "-setup.exe", windows_arch()),
        }
    }
}

/// The installable format that owns this machine's copy of Hydra.
///
/// Linux: whichever package database exists. macOS: the `.pkg` when its
/// receipt is on disk, the `.dmg` otherwise — both deliver a whole
/// `Hydra Download Manager.app`, which is the only correct way to replace
/// one. Windows: the setup installer.
pub fn package_kind() -> Option<PackageKind> {
    if cfg!(target_os = "linux") {
        if Path::new("/var/lib/dpkg/status").exists() {
            Some(PackageKind::Deb)
        } else if Path::new("/var/lib/rpm").exists() {
            Some(PackageKind::Rpm)
        } else {
            None
        }
    } else if cfg!(target_os = "macos") {
        // `pkgutil` records an installer receipt per package identifier
        // (scripts/package-macos-pkg.sh); a dragged .dmg leaves none.
        if Path::new("/var/db/receipts/io.github.ja7ad.hydra.plist").exists() {
            Some(PackageKind::Pkg)
        } else {
            Some(PackageKind::Dmg)
        }
    } else if cfg!(target_os = "windows") {
        Some(PackageKind::Exe)
    } else {
        None
    }
}

/// Architecture as rpm spells it (`uname -m`), unlike dpkg's and the
/// archives' [`arch_tag`].
fn rpm_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// Architecture as the macOS disk image and installer name it: Apple's
/// `arm64`, but `uname`'s `x86_64` on Intel.
fn macos_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    }
}

/// Architecture as the Windows installer names it (`x64`, not `amd64`).
fn windows_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    }
}

// ------------------------------------------------------------------- AppImage

/// Architecture as an AppImage names itself (`uname -m`), which is neither
/// the archives' [`arch_tag`] nor dpkg's.
pub fn appimage_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// The `.AppImage` file this process is running from, or `None` when it is
/// not running from one.
///
/// The AppImage runtime exports `APPIMAGE` (the image file the user
/// launched) and `APPDIR` (the read-only mount it was expanded into) into
/// the application it starts. `current_exe()` reports a path inside that
/// mount, which is useless to an updater: it is read-only, its name changes
/// on every run, and it ceases to exist when the process does. The single
/// file that IS the install is `APPIMAGE`, and replacing it is the whole
/// update.
///
/// Both variables are required: `APPIMAGE` alone can be inherited by a
/// child process that is not itself an AppImage.
pub fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPDIR")?;
    let p = PathBuf::from(std::env::var_os("APPIMAGE")?);
    p.is_file().then_some(p)
}

/// Release asset holding the AppImage for this machine:
/// `Hydra-0.3.4-x86_64.AppImage`.
pub fn appimage_asset_name(version: &str) -> String {
    format!("Hydra-{version}-{}.AppImage", appimage_arch())
}

/// Whether replacing this AppImage will need an authorisation prompt: the
/// image sits in a directory this user cannot write (`/opt`, `/usr/local/bin`).
///
/// The finisher does not consult this — it tries the swap and elevates on
/// `PermissionDenied`. It exists so the dialog can say so before the
/// download rather than after it.
pub fn appimage_needs_auth() -> bool {
    match appimage_path() {
        Some(img) => !img.parent().is_some_and(dir_is_writable),
        None => false,
    }
}

/// Install `src` as the AppImage at `dest`.
///
/// An AppImage install is one executable file, so this is [`apply_with`]'s
/// whole job reduced to a single [`replace_file`]: the old image is renamed
/// aside, the new one is copied into its name, and it inherits the old
/// file's mode (an AppImage that is not executable is not an install). The
/// running image is still FUSE-mounted from the old inode while this
/// happens, which is exactly why it is renamed rather than truncated.
pub fn replace_appimage(src: &Path, dest: &Path) -> io::Result<ApplyReport> {
    if !src.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no AppImage to install at {}", src.display()),
        ));
    }
    if !dest.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no installed AppImage at {}", dest.display()),
        ));
    }
    replace_file(src, dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dest)?.permissions().mode();
        // The mode came from the old image, but a copy that lost its exec
        // bits would leave the user with a file the desktop cannot start.
        if mode & 0o111 == 0 {
            std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(ApplyReport {
        replaced: vec![dest
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()],
        ..ApplyReport::default()
    })
}

/// The macOS application bundle `dir` sits inside, if any: the nearest
/// enclosing `*.app` directory.
///
/// A bundle is not a flat install directory. Its GUI executable is named
/// after the product (`Contents/MacOS/Hydra Download Manager`) while the
/// release archive ships `hydra-gui`, and `Contents/Info.plist` carries the
/// version macOS shows — so an update has to be told about the layout
/// ([`apply_with`]) instead of copying names on top of names.
pub fn app_bundle_root(dir: &Path) -> Option<PathBuf> {
    let mut root = PathBuf::new();
    let mut found = None;
    for c in dir.components() {
        root.push(c);
        if Path::new(&c)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"))
        {
            found = Some(root.clone());
        }
    }
    found
}

/// True when `dir` sits inside a macOS application bundle.
pub fn in_app_bundle(dir: &Path) -> bool {
    app_bundle_root(dir).is_some()
}

/// The directory a Windows portable bundle keeps its program files in,
/// relative to the `HydraPortable` root.
pub const PORTABLE_APP_DIR: &str = "App/Hydra";

/// Whether `dir` (the directory holding the running executable) is a
/// PortableApps-style bundle: `HydraPortable\App\Hydra` beside the
/// `HydraPortable.exe` launcher. Its release asset is the portable zip, whose
/// program files sit under [`PORTABLE_APP_DIR`] rather than at the archive
/// root.
pub fn is_portable_install(dir: &Path) -> bool {
    let is_named = |p: &Path, name: &str| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    };
    let Some(app) = dir.parent().filter(|_| is_named(dir, "Hydra")) else {
        return false;
    };
    let Some(root) = app.parent().filter(|_| is_named(app, "App")) else {
        return false;
    };
    root.join("HydraPortable.exe").is_file()
}

/// The directory holding a bundle's executables.
pub fn bundle_bin_dir(bundle: &Path) -> PathBuf {
    bundle.join("Contents").join("MacOS")
}

/// How an install can take an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateMethod {
    /// Hydra owns these files and the finisher can replace them directly.
    InPlace,
    /// Root owns the files but no package manager does — a tarball unpacked
    /// into `/usr/local/bin` with sudo, or an app copied to `/Applications`
    /// by another admin. The finisher re-runs itself through an
    /// authorisation prompt ([`elevation`]).
    Elevated,
    /// A package manager owns this install; only it may rewrite these files,
    /// so the update is offered as its own `.deb`/`.rpm`/`.pkg`/setup.
    Package,
    /// The install is a single self-contained `.AppImage`. There are no
    /// installed files to swap — the image file itself is replaced
    /// ([`replace_appimage`]), which is also what keeps the thing portable:
    /// wherever the user put it, that is what gets updated.
    AppImage,
}

impl UpdateMethod {
    /// Whether Hydra can install this update itself, with or without an
    /// authorisation prompt.
    pub fn is_self_update(self) -> bool {
        !matches!(self, UpdateMethod::Package)
    }
}

/// How the install in `dir` (the directory holding the running executable)
/// can be updated.
///
/// Ownership decides, and it is probed rather than guessed: a tarball under
/// `~/.local/bin` and a deb's `/usr/bin` differ only in who owns them. Only
/// a package-manager install is refused outright — rewriting `/usr/bin`
/// behind dpkg's back leaves its database describing files that are no
/// longer there.
pub fn update_method(dir: &Path) -> UpdateMethod {
    // An AppImage answers first, and without consulting `dir`: `dir` is the
    // read-only mount the runtime made, so its ownership describes a
    // temporary directory rather than the install. Whether the image file
    // can be written is the finisher's problem, and it has an
    // authorisation prompt for the answer.
    if appimage_path().is_some() {
        return UpdateMethod::AppImage;
    }
    if package_managed(dir) {
        return UpdateMethod::Package;
    }
    if update_target_is_writable(dir) {
        return UpdateMethod::InPlace;
    }
    match elevation() {
        Some(_) => UpdateMethod::Elevated,
        None => UpdateMethod::Package,
    }
}

/// True when running inside a Flatpak sandbox (`FLATPAK_ID` set or `/.flatpak-info` exists).
pub fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists()
}

/// Whether a package manager owns the install in `dir`.
///
/// Linux: the deb and rpm both install into `/usr/bin` (scripts/package-linux.sh),
/// Flatpak runs from an immutable `/app` mount, while `/usr/local` is by convention
/// exactly the part of the filesystem no package manager touches — except for
/// Homebrew, whose Cellar and Caskroom live under its prefix and whose files are
/// every bit as owned as dpkg's ([`brew_owned`]). macOS: the `.pkg` records a
/// receipt per package identifier and lands in `/Applications`; so does the
/// Homebrew cask, which leaves its payload directory in the Caskroom instead; a
/// dragged `.dmg` leaves neither, which is what separates the three installs that
/// otherwise look identical. Windows: the setup installer owns whatever it wrote,
/// and there is no unprivileged way to rewrite `Program Files`.
fn package_managed(dir: &Path) -> bool {
    if is_flatpak() || brew_owned(dir).is_some() {
        return true;
    }
    if cfg!(target_os = "linux") {
        dir.starts_with("/usr") && !dir.starts_with("/usr/local")
    } else if cfg!(target_os = "macos") {
        dir.starts_with("/Applications")
            && (Path::new("/var/db/receipts/io.github.ja7ad.hydra.plist").exists()
                || cask_installed())
    } else {
        // Windows: no elevation path here (a UAC re-launch of the finisher
        // would prompt with the app already gone), so an install this
        // process cannot write is the installer's to replace.
        !update_target_is_writable(dir)
    }
}

/// Which Homebrew artefact owns `dir`, going by Homebrew's own directories:
/// a formula's keg under `Cellar`, a cask's payload under `Caskroom`, and
/// on Linux everything under linuxbrew's single prefix. Updating those in
/// place works until the next `brew upgrade`, which then reinstalls the
/// version brew believes is there.
fn brew_owned(dir: &Path) -> Option<Brew> {
    for prefix in BREW_PREFIXES {
        if dir.starts_with(format!("{prefix}/Caskroom")) {
            return Some(Brew::Cask);
        }
        if dir.starts_with(format!("{prefix}/Cellar")) {
            return Some(Brew::Formula);
        }
    }
    dir.starts_with(LINUXBREW_PREFIX).then_some(Brew::Formula)
}

const LINUXBREW_PREFIX: &str = "/home/linuxbrew/.linuxbrew";
const BREW_PREFIXES: [&str; 3] = ["/opt/homebrew", "/usr/local", LINUXBREW_PREFIX];

/// The two things Homebrew installs Hydra as: the `hydra` formula (the CLI,
/// built from source into a keg) and the `hydra` cask (the `.dmg`'s app,
/// copied to `/Applications`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Brew {
    Cask,
    Formula,
}

/// Whether the Hydra cask is installed on this machine: its payload
/// directory in the Caskroom is what `brew` leaves behind when it copies the
/// app to `/Applications`, where the bundle itself is indistinguishable from
/// a dragged one.
fn cask_installed() -> bool {
    BREW_PREFIXES
        .iter()
        .any(|p| Path::new(p).join("Caskroom").join("hydra").is_dir())
}

/// The command that updates a package-manager install, for the managers
/// Hydra knows the spelling for, so a dialog that cannot update in place can
/// say what will. `None` for installs Hydra updates itself and for managers
/// with no single command (a `.deb` from the release page).
pub fn package_manager_hint(dir: &Path) -> Option<&'static str> {
    match brew_owned(dir) {
        Some(Brew::Formula) => Some("brew upgrade ja7ad/tap/hydra"),
        Some(Brew::Cask) => Some("brew upgrade --cask ja7ad/tap/hydra"),
        None if cfg!(target_os = "macos")
            && dir.starts_with("/Applications")
            && cask_installed() =>
        {
            Some("brew upgrade --cask ja7ad/tap/hydra")
        }
        None => None,
    }
}

/// Whether this process can rewrite everything an update touches for an
/// install in `dir`: the executables, and for a macOS bundle also
/// `Contents/`, which holds `Info.plist` and the code signature.
pub fn update_target_is_writable(dir: &Path) -> bool {
    match app_bundle_root(dir) {
        Some(bundle) => {
            dir_is_writable(&bundle_bin_dir(&bundle)) && dir_is_writable(&bundle.join("Contents"))
        }
        None => dir_is_writable(dir),
    }
}

/// Whether this process can create files in `dir`.
///
/// Creating a file is the only honest test — Unix write permission on a
/// directory is not implied by anything readable.
pub fn dir_is_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".hydra-write-test-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// How this machine can ask the user to authorise a privileged file swap,
/// or `None` when it cannot.
///
/// macOS has `osascript`'s `with administrator privileges`, which shows the
/// system authorisation panel. Linux has `pkexec`, whose polkit agent is
/// part of every desktop session. Neither `sudo` nor `doas` qualifies: the
/// finisher runs detached with no terminal to type a password into.
pub fn elevation() -> Option<Elevation> {
    if cfg!(target_os = "macos") {
        Path::new("/usr/bin/osascript")
            .exists()
            .then_some(Elevation::Osascript)
    } else if cfg!(target_os = "linux") {
        which("pkexec").map(Elevation::Pkexec)
    } else {
        None
    }
}

/// A way to run one command as root after the user authorises it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Elevation {
    /// macOS: `osascript -e 'do shell script "…" with administrator privileges'`.
    Osascript,
    /// Linux: `pkexec …`, at the resolved path.
    Pkexec(PathBuf),
}

/// First `PATH` entry holding an executable named `name`.
fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Run `argv` as root behind the platform's authorisation prompt, waiting
/// for it to finish.
///
/// The prompt is the point: the finisher runs after the app has exited, so
/// there is no window to host a password field and no terminal to type into.
/// macOS draws its own authorisation panel, and `pkexec` hands the request to
/// the desktop's polkit agent.
pub fn run_elevated(elevation: &Elevation, argv: &[&std::ffi::OsStr]) -> io::Result<()> {
    let status = match elevation {
        Elevation::Pkexec(pkexec) => std::process::Command::new(pkexec).args(argv).status()?,
        Elevation::Osascript => {
            let script = format!(
                "do shell script {} with administrator privileges",
                applescript_string(&shell_command(argv))
            );
            std::process::Command::new("/usr/bin/osascript")
                .arg("-e")
                .arg(script)
                .status()?
        }
    };
    if status.success() {
        Ok(())
    } else {
        // A cancelled macOS prompt exits 1 the same way a failed swap does;
        // the elevated run logs which of the two it was.
        Err(io::Error::other(format!(
            "elevated update finisher exited with {status}"
        )))
    }
}

/// `argv` as one `/bin/sh` command line, every word single-quoted.
fn shell_command(argv: &[&std::ffi::OsStr]) -> String {
    argv.iter()
        .map(|a| format!("'{}'", a.to_string_lossy().replace('\'', r"'\''")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `s` as an AppleScript string literal.
fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

/// The version without its pre-release or build suffix (`0.3.2-rc` →
/// `0.3.2`), the spelling the release pipeline names assets with whenever
/// the workspace manifest stays on the plain version.
pub fn version_core(version: &str) -> &str {
    let v = version.trim().trim_start_matches('v');
    v.split(['-', '+']).next().unwrap_or(v)
}

/// Release asset holding the GUI bundle for this machine:
/// `hydra-0.2.4-macos-arm64.tar.gz`.
pub fn gui_asset_name(version: &str) -> String {
    format!(
        "hydra-{version}-{}-{}.{}",
        os_tag(),
        arch_tag(),
        archive_ext()
    )
}

/// Release asset holding the Windows portable bundle for this machine:
/// `hydra-0.2.4-windows-portable-amd64.zip`.
pub fn portable_asset_name(version: &str) -> String {
    format!("hydra-{version}-windows-portable-{}.zip", arch_tag())
}

/// Release asset holding the standalone CLI for this machine:
/// `hydra-cli-0.2.4-macos-arm64.tar.gz`.
pub fn cli_asset_name(version: &str) -> String {
    format!(
        "hydra-cli-{version}-{}-{}.{}",
        os_tag(),
        arch_tag(),
        archive_ext()
    )
}

// -------------------------------------------------------------------- checks

/// Fetch the latest release from the effective API base ([`api_base`]).
pub async fn check_latest(user_agent: &str) -> io::Result<Release> {
    check_latest_at(&api_base(), REPO, user_agent).await
}

/// Fetch the latest release from an explicit API base — the mock-test entry
/// point ([`check_latest`] is this with the env-resolved base).
pub async fn check_latest_at(base: &str, repo: &str, user_agent: &str) -> io::Result<Release> {
    let url = format!("{base}/repos/{repo}/releases/latest");
    // Release JSON with notes and a dozen assets runs tens of KB; 4 MB is a
    // refusal threshold, not a size expectation.
    let body = http::get_json(&url, user_agent, 4 * 1024 * 1024).await?;
    serde_json::from_slice(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("release JSON: {e}")))
}

/// The most recent releases, newest first as GitHub lists them.
async fn release_list(base: &str, repo: &str, user_agent: &str) -> io::Result<Vec<Release>> {
    let url = format!("{base}/repos/{repo}/releases?per_page=30");
    let body = http::get_json(&url, user_agent, 8 * 1024 * 1024).await?;
    serde_json::from_slice(&body).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("release list JSON: {e}"),
        )
    })
}

/// Fetch the release the given channel should offer.
///
/// Stable (`beta == false`) is [`check_latest`]: `/releases/latest`, where
/// GitHub lists no release flagged pre-release. The flag is the only thing
/// it goes by, so an `-rc` tag published without it IS `latest` — the stable
/// channel then looks past it to the newest stable release in the list
/// rather than offer a candidate to people who chose not to get them. The
/// beta channel additionally scans the release list for the newest `-rc`
/// pre-release and offers it only while it is ahead of the stable release —
/// once stable catches up (`0.3.0` after `0.3.0-rc2`), beta serves the
/// stable release again.
pub async fn check_channel(user_agent: &str, beta: bool) -> io::Result<Release> {
    check_channel_at(&api_base(), REPO, user_agent, beta).await
}

/// [`check_channel`] against an explicit API base — the mock-test entry
/// point.
pub async fn check_channel_at(
    base: &str,
    repo: &str,
    user_agent: &str,
    beta: bool,
) -> io::Result<Release> {
    let latest = check_latest_at(base, repo, user_agent).await;
    if !beta {
        return match latest {
            Ok(r) if r.is_prerelease() => {
                release_list(base, repo, user_agent).await.and_then(|list| {
                    newest_stable(list)
                        .ok_or_else(|| io::Error::other("no stable release published yet"))
                })
            }
            other => other,
        };
    }
    // A failed or unparsable list degrades beta to stable behaviour rather
    // than blocking updates: the pre-release is an extra offer, not a
    // dependency.
    let pre = release_list(base, repo, user_agent)
        .await
        .ok()
        .and_then(newest_prerelease);
    match (latest, pre) {
        (Ok(stable), Some(pre)) if is_newer(pre.version(), stable.version()) => Ok(pre),
        (Ok(stable), _) => Ok(stable),
        // A repo whose only releases are pre-releases has no `latest`
        // (GitHub 404s); the rc is still a valid beta offer.
        (Err(_), Some(pre)) => Ok(pre),
        (Err(e), None) => Err(e),
    }
}

/// The highest-versioned pre-release in a release list. The GitHub flag is
/// authoritative; a `-rc` tag counts too, so a release someone forgot to
/// mark pre-release still reaches the beta channel.
fn newest_prerelease(list: Vec<Release>) -> Option<Release> {
    newest(list.into_iter().filter(Release::is_prerelease))
}

/// The highest-versioned release in the list that is a pre-release by
/// neither flag nor tag.
fn newest_stable(list: Vec<Release>) -> Option<Release> {
    newest(list.into_iter().filter(|r| !r.is_prerelease()))
}

fn newest(releases: impl Iterator<Item = Release>) -> Option<Release> {
    releases.fold(None::<Release>, |best, r| match best {
        Some(b) if !is_newer(r.version(), b.version()) => Some(b),
        _ => Some(r),
    })
}

/// Release notes with HTML comments stripped and repeated sections dropped.
///
/// GitHub's generated notes open with a `<!-- Release notes generated using
/// configuration in .github/release.yml at v0.3.4-rc -->` marker. It is
/// plumbing: the CLI dumps notes as plain text and the GUI renders them as
/// simple styled lines, so neither hides it the way a markdown renderer
/// would. A body that carries the generated block more than once (the
/// release job appends rather than replaces when it runs again over a tag
/// that already has a release) is collapsed back to one copy by
/// [`dedupe_sections`].
pub fn clean_notes(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        rest = match rest[start..].find("-->") {
            Some(end) => &rest[start + end + "-->".len()..],
            // An unterminated comment swallows the rest, as a renderer would.
            None => "",
        };
    }
    out.push_str(rest);
    // Removing a comment leaves the blank line it sat on; collapse the runs
    // it opens up so the notes do not start or read as full of holes.
    let mut notes = String::with_capacity(out.len());
    let mut blanks = 0;
    for line in out.trim().lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        notes.push_str(line.trim_end());
        notes.push('\n');
    }
    dedupe_sections(notes.trim_end())
}

/// Drop repeated copies of the same generated block.
///
/// A release job that is re-run (or a release edited by
/// `generate_release_notes` a second time) appends another copy of the
/// generated notes to the body instead of replacing it, so v0.3.10 shipped
/// with its "What's Changed" section five times over. The dialog is 500 px
/// wide and shows the body verbatim, so the repeats are all the reader sees.
/// Sections are split at their `#` heading and matched on their content, and
/// only an exact repeat of one already kept is dropped — notes that genuinely
/// say different things under the same heading survive.
fn dedupe_sections(notes: &str) -> String {
    let mut sections: Vec<Vec<&str>> = Vec::new();
    for line in notes.lines() {
        // A top-level (`#`/`##`) heading opens a section; deeper ones
        // (`### 🚀 Features`) belong to the section they sit under.
        let heading = line.starts_with("# ") || line.starts_with("## ");
        if heading || sections.is_empty() {
            sections.push(Vec::new());
        }
        sections
            .last_mut()
            .expect("a section exists: one is pushed above when empty")
            .push(line);
    }
    let mut seen: Vec<String> = Vec::new();
    let mut kept: Vec<&str> = Vec::new();
    for section in sections {
        // Compare on the meaningful lines only, so a copy that differs by
        // the blank line a stripped comment left behind still counts as one.
        let key = section
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if key.is_empty() || !seen.contains(&key) {
            if !key.is_empty() {
                seen.push(key);
            }
            kept.extend(section);
        }
    }
    // Dropping a repeat leaves the blank lines that separated it behind.
    let mut out = String::with_capacity(notes.len());
    let mut blanks = 0;
    for line in kept {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks > 1 || out.is_empty() {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

// ----------------------------------------------------------------- checksums

/// Extract the sha256 hex digest for `asset_name` from a `SHA256SUMS.txt`
/// body (`<hex>  <name>` lines, `sha256sum` format).
pub fn sum_for(sums: &str, asset_name: &str) -> Option<String> {
    for line in sums.lines() {
        let mut it = line.split_whitespace();
        let (Some(hex), Some(name)) = (it.next(), it.next()) else {
            continue;
        };
        // `sha256sum` writes `<hex> *<name>` in binary mode.
        if name.trim_start_matches('*') == asset_name && hex.len() == 64 {
            return Some(hex.to_ascii_lowercase());
        }
    }
    None
}

/// How a downloaded archive stood up to the release's published checksums.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verification {
    /// The archive's digest matches the published one.
    Verified,
    /// The release publishes no checksum file at all. The archive was
    /// accepted on transport security alone; the caller should say so.
    Unpublished,
}

/// Check `archive` (published as `asset_name`) against `sums`, the body of
/// the release's `SHA256SUMS.txt` when it has one.
///
/// A sums file that exists but lists no entry for this asset is a refusal,
/// not a pass: it is exactly what a mislabelled or tampered release looks
/// like, and quietly accepting the archive was the gap. Whatever fails, the
/// archive is removed so a retry starts clean.
pub fn verify_archive(
    archive: &Path,
    asset_name: &str,
    sums: Option<&str>,
) -> io::Result<Verification> {
    let Some(sums) = sums else {
        return Ok(Verification::Unpublished);
    };
    let refuse = |msg: String| {
        let _ = std::fs::remove_file(archive);
        Err(io::Error::new(io::ErrorKind::InvalidData, msg))
    };
    let Some(want) = sum_for(sums, asset_name) else {
        return refuse(format!(
            "SHA256SUMS.txt has no entry for {asset_name} — the archive was discarded"
        ));
    };
    if file_sha256(archive)? != want {
        return refuse("checksum mismatch — the downloaded archive was discarded".to_string());
    }
    Ok(Verification::Verified)
}

/// sha256 of a file, lowercase hex.
pub fn file_sha256(path: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let out = h.finalize();
    let mut hex = String::with_capacity(64);
    for b in out {
        hex.push_str(&format!("{b:02x}"));
    }
    Ok(hex)
}

// ---------------------------------------------------------------- extraction

/// Unpack a release archive (`.tar.gz` or `.zip`, decided by the file name)
/// into `dest` and return the bundle root: the single top-level directory
/// when there is exactly one (the pipeline's layout), `dest` itself otherwise.
pub fn extract(archive: &Path, dest: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dest)?;
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with(".zip") {
        let f = std::fs::File::open(archive)?;
        let mut z = zip::ZipArchive::new(f).map_err(io::Error::other)?;
        // `extract` sanitises entry names (no absolute paths, no `..`).
        z.extract(dest).map_err(io::Error::other)?;
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let f = std::fs::File::open(archive)?;
        let gz = flate2::read::GzDecoder::new(f);
        let mut t = tar::Archive::new(gz);
        // tar::Archive::unpack refuses entries that escape `dest`.
        t.unpack(dest)?;
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unrecognised archive format: {name}"),
        ));
    }
    // The pipeline stages everything under one `hydra-<ver>-<os>-<arch>/`
    // directory; when that is the whole archive, that directory is the root.
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dest)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    if entries.len() == 1 && entries[0].is_dir() {
        return Ok(entries.remove(0));
    }
    Ok(dest.to_path_buf())
}

// -------------------------------------------------------------------- apply

/// What the swap step did, for the log.
#[derive(Debug, Default)]
pub struct ApplyReport {
    pub replaced: Vec<String>,
    pub skipped: Vec<String>,
    /// macOS: notes about the bundle steps that follow the file swap
    /// (`Info.plist` version, re-signing).
    pub notes: Vec<String>,
}

/// Knobs for [`apply_with`].
#[derive(Clone, Debug, Default)]
pub struct ApplyOptions {
    /// Version the new files carry. Only a macOS bundle needs it: its
    /// `Info.plist` is what the Finder, the Dock and "About This Mac >
    /// Software" read, and it would otherwise keep describing the version
    /// the bundle was built as.
    pub version: Option<String>,
}

/// [`apply_with`] with default options.
pub fn apply(src_root: &Path, install_dir: &Path) -> io::Result<ApplyReport> {
    apply_with(src_root, install_dir, &ApplyOptions::default())
}

/// Copy the new release's files over the installed ones.
///
/// Only files that ALREADY EXIST at the destination are replaced: a plain
/// unpacked-archive install gets every binary refreshed, while a partial
/// install never gains stray files it did not have. Everything else in the
/// release bundle (extensions/, scripts/, licences) is left alone and
/// reported in `skipped`.
///
/// A macOS `.app` install is written through [`bundle_dest`], which maps the
/// archive's `hydra-gui` onto the bundle's product-named executable; the
/// bundle then has its `Info.plist` version rewritten and is re-signed, so
/// what macOS reports about the app matches what is inside it.
///
/// The swap is all or none ([`replace_all`]): every old file is renamed
/// aside before any new one is copied in, and a failure part-way puts the
/// renamed files back, so the install is never left as half of each version.
/// Each rename retries with backoff for up to ~20 s: on Windows the old
/// process's executable stays locked until it has fully exited, and the retry
/// loop IS the "wait for exit" mechanism — no PID polling required. The
/// running file may not be overwritten, but it may be renamed away, and a
/// fresh copy takes its name.
pub fn apply_with(
    src_root: &Path,
    install_dir: &Path,
    opts: &ApplyOptions,
) -> io::Result<ApplyReport> {
    let mut report = ApplyReport::default();
    let bundle = app_bundle_root(install_dir);
    let exec_name = bundle.as_deref().map(bundle_exec_name);
    let mut plan: Vec<(PathBuf, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(src_root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            report
                .skipped
                .push(entry.file_name().to_string_lossy().into_owned());
            continue;
        }
        let name = entry.file_name();
        let dest = match (&bundle, &exec_name) {
            (Some(b), Some(exec)) => bundle_dest(b, exec, &name.to_string_lossy()),
            _ => Some(install_dir.join(&name)),
        };
        let Some(dest) = dest.filter(|d| d.exists()) else {
            report.skipped.push(name.to_string_lossy().into_owned());
            continue;
        };
        plan.push((path, dest));
    }
    replace_all(&plan)?;
    for (src, dest) in &plan {
        let name = src.file_name().unwrap_or_default();
        report.replaced.push(name.to_string_lossy().into_owned());
        if name == cli_file_name() {
            match refresh_cli_alias(dest) {
                Ok(true) => report.notes.push(format!("{CLI_ALIAS} refreshed")),
                Ok(false) => {}
                Err(e) => report
                    .notes
                    .push(format!("{CLI_ALIAS} refresh failed: {e}")),
            }
        }
    }
    if let Some(bundle) = &bundle {
        finish_bundle(bundle, opts.version.as_deref(), &mut report);
    }
    if let Some(version) = opts.version.as_deref() {
        match set_uninstall_version(install_dir, version) {
            Ok(true) => report.notes.push(format!("DisplayVersion -> {version}")),
            Ok(false) => {}
            Err(e) => report
                .notes
                .push(format!("DisplayVersion update failed: {e}")),
        }
    }
    Ok(report)
}

/// Restamp `DisplayVersion` in the setup installer's uninstall entry, which
/// Settings > Apps and every inventory tool read; a swap in place would
/// otherwise leave it naming the version that was first installed.
/// `Ok(false)` off Windows, or when the entry belongs to another install.
#[cfg(windows)]
fn set_uninstall_version(install_dir: &Path, version: &str) -> io::Result<bool> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    // The Add/Remove Programs entry `hydra-installer.nsi` writes.
    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Hydra";
    let key = match winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(UNINSTALL_KEY, KEY_READ | KEY_SET_VALUE)
    {
        Ok(key) => key,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let location: String = key.get_value("InstallLocation").unwrap_or_default();
    if !same_install_dir(&location, install_dir) {
        return Ok(false);
    }
    key.set_value("DisplayVersion", &version)?;
    Ok(true)
}

#[cfg(not(windows))]
fn set_uninstall_version(_install_dir: &Path, _version: &str) -> io::Result<bool> {
    Ok(false)
}

/// Whether the installer's recorded `InstallLocation` names `install_dir`.
/// Windows paths compare without case, and NSIS may or may not leave a
/// trailing separator on `$INSTDIR`.
#[cfg_attr(not(windows), allow(dead_code))]
fn same_install_dir(location: &str, install_dir: &Path) -> bool {
    let trim = |s: &str| s.trim_end_matches(['\\', '/']).replace('/', "\\");
    let location = trim(location);
    !location.is_empty() && location.eq_ignore_ascii_case(&trim(&install_dir.to_string_lossy()))
}

/// The short second name every Hydra installer puts next to the CLI, because
/// `hydra` is also THC-Hydra and three letters types better.
pub const CLI_ALIAS: &str = if cfg!(windows) { "hya.exe" } else { "hya" };

/// The CLI's file name in a release archive.
fn cli_file_name() -> &'static str {
    if cfg!(windows) {
        "hydra.exe"
    } else {
        "hydra"
    }
}

/// Put the `hya` shorthand back in step with the CLI at `cli`, which was just
/// replaced.
///
/// On Unix the installers link it (`hya -> hydra`), so it follows the new
/// binary on its own and this does nothing. Windows hands out a symlink only
/// to an elevated shell or with Developer Mode on, so `install.ps1` may have
/// left a hard link or a plain copy — and both still carry the OLD bits after
/// [`try_replace`] renamed the file aside and wrote a new one in its place.
///
/// Only an alias that is already there is touched: an install that never had
/// the short name does not gain one from an update.
fn refresh_cli_alias(cli: &Path) -> io::Result<bool> {
    let Some(alias) = cli.parent().map(|d| d.join(CLI_ALIAS)) else {
        return Ok(false);
    };
    // symlink_metadata, so a link is seen as a link rather than followed.
    match std::fs::symlink_metadata(&alias) {
        Ok(meta) if meta.file_type().is_symlink() => Ok(false),
        Ok(_) => {
            replace_file(cli, &alias)?;
            Ok(true)
        }
        Err(_) => Ok(false),
    }
}

/// Where a release file lands inside a macOS application bundle, or `None`
/// when the bundle has no place for it.
///
/// The GUI is the one rename: the archive ships `hydra-gui`, the bundle runs
/// it as its `CFBundleExecutable`. The CLI, the native-messaging host and
/// the finisher keep their names in `Contents/MacOS/`; everything else
/// (`logo.png`, licences, the extension tree) belongs to the archive layout,
/// not the bundle, and is skipped.
pub fn bundle_dest(bundle: &Path, exec_name: &str, file: &str) -> Option<PathBuf> {
    let bin = bundle_bin_dir(bundle);
    match file {
        "hydra-gui" => Some(bin.join(exec_name)),
        "hydra" | "hydra-host" | "hydra-updater" => Some(bin.join(file)),
        _ => None,
    }
}

/// A bundle's `CFBundleExecutable`, falling back to the product name the
/// packaging scripts use.
pub fn bundle_exec_name(bundle: &Path) -> String {
    std::fs::read_to_string(bundle.join("Contents").join("Info.plist"))
        .ok()
        .and_then(|xml| plist_string(&xml, "CFBundleExecutable").map(|s| s.to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Hydra Download Manager".to_string())
}

/// Bundle bookkeeping after the executables are swapped: the version macOS
/// reads, then the signature that covers it.
fn finish_bundle(bundle: &Path, version: Option<&str>, report: &mut ApplyReport) {
    if let Some(version) = version {
        match set_bundle_version(bundle, version) {
            Ok(true) => report.notes.push(format!("Info.plist -> {version}")),
            Ok(false) => report
                .notes
                .push("Info.plist has no version keys; left alone".into()),
            Err(e) => report.notes.push(format!("Info.plist update failed: {e}")),
        }
    }
    // The ad-hoc signature seals Info.plist and the resource tree, both of
    // which just changed underneath it. macOS refuses to launch an app whose
    // seal is broken, so re-signing is not cosmetic. Best effort: an install
    // that was never signed has nothing to invalidate.
    match resign_bundle(bundle) {
        Ok(true) => report.notes.push("re-signed (ad-hoc)".into()),
        Ok(false) => {}
        Err(e) => report.notes.push(format!("codesign failed: {e}")),
    }
    // A bundle that was downloaded carries the quarantine attribute, and an
    // ad-hoc signature is not a notarised one: Gatekeeper reports the pair
    // as "damaged and can't be opened". The user chose to run this app when
    // they installed it; the update does not get to ask again.
    match strip_quarantine(bundle) {
        Ok(true) => report.notes.push("quarantine attribute removed".into()),
        Ok(false) => {}
        Err(e) => report.notes.push(format!("xattr failed: {e}")),
    }
}

/// Remove `com.apple.quarantine` from everything under `bundle`. `Ok(false)`
/// when the platform has no `xattr`.
fn strip_quarantine(bundle: &Path) -> io::Result<bool> {
    if !cfg!(target_os = "macos") || !Path::new("/usr/bin/xattr").exists() {
        return Ok(false);
    }
    // `-d` complains about every file that does not carry the attribute
    // and exits non-zero for it; with `-r` it still visits the rest, so the
    // exit status says nothing about whether the flagged files were cleared.
    std::process::Command::new("/usr/bin/xattr")
        .args(["-r", "-d", "com.apple.quarantine"])
        .arg(bundle)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    Ok(true)
}

/// Rewrite `CFBundleVersion` and `CFBundleShortVersionString` in a bundle's
/// `Info.plist`. Returns whether anything was written.
///
/// Both keys take period-separated integers only, so a pre-release suffix
/// (`0.4.0-rc1`) is trimmed to its numeric core the same way the packaging
/// scripts trim it.
pub fn set_bundle_version(bundle: &Path, version: &str) -> io::Result<bool> {
    let plist = bundle.join("Contents").join("Info.plist");
    let xml = std::fs::read_to_string(&plist)?;
    let core = version_core(version);
    let mut out = xml.clone();
    let mut changed = false;
    for key in ["CFBundleVersion", "CFBundleShortVersionString"] {
        if let Some(range) = plist_string_range(&out, key) {
            out.replace_range(range, core);
            changed = true;
        }
    }
    if !changed {
        return Ok(false);
    }
    // Write beside the target and rename: a half-written Info.plist is an
    // unlaunchable app.
    let tmp = plist.with_extension("plist.new");
    std::fs::write(&tmp, out.as_bytes())?;
    std::fs::rename(&tmp, &plist)?;
    Ok(true)
}

/// Pins the bundle's TCC identity to its identifier. A bare ad-hoc signature
/// designates the cdhash, which every update changes, so macOS keeps listing
/// the app under Full Disk Access while refusing the new binary.
const DESIGNATED_REQUIREMENT: &str = "-r=designated => identifier \"io.github.ja7ad.hydra\"";

/// Ad-hoc re-sign a bundle. `Ok(false)` when the platform has no codesign.
fn resign_bundle(bundle: &Path) -> io::Result<bool> {
    if !cfg!(target_os = "macos") || !Path::new("/usr/bin/codesign").exists() {
        return Ok(false);
    }
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--deep", "--sign", "-", DESIGNATED_REQUIREMENT])
        .arg(bundle)
        .output()?;
    if out.status.success() {
        return Ok(true);
    }
    Err(io::Error::other(
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    ))
}

/// The text of the `<string>` value following `<key>NAME</key>` in an XML
/// plist, as a range into `xml`.
///
/// A hand-rolled scan rather than a plist parser: this reads and writes two
/// version keys in a file the packaging scripts generate, and a dependency
/// that can decode binary plists and every value type buys nothing here.
fn plist_string_range(xml: &str, key: &str) -> Option<std::ops::Range<usize>> {
    let key_tag = format!("<key>{key}</key>");
    let after = xml.find(&key_tag)? + key_tag.len();
    let open = xml[after..].find("<string>")? + after + "<string>".len();
    let close = xml[open..].find("</string>")? + open;
    Some(open..close)
}

/// The `<string>` value following `<key>NAME</key>` in an XML plist.
fn plist_string<'a>(xml: &'a str, key: &str) -> Option<&'a str> {
    plist_string_range(xml, key).map(|r| &xml[r])
}

/// Replace `dest` with `src`: [`replace_all`] for one file.
fn replace_file(src: &Path, dest: &Path) -> io::Result<()> {
    replace_all(&[(src.to_path_buf(), dest.to_path_buf())])
}

/// Replace every `(src, dest)` pair, all or none.
///
/// Every old file is renamed aside first, then every new one is copied in,
/// and only then are the old copies deleted. A failure at any point removes
/// what was copied and puts back what was renamed, so an update that dies
/// on its third file leaves the first two as they were — an install made of
/// two versions at once is one that may not start at all.
fn replace_all(pairs: &[(PathBuf, PathBuf)]) -> io::Result<()> {
    let mut aside: Vec<(PathBuf, &Path)> = Vec::new();
    let mut copied: Vec<&Path> = Vec::new();
    let mut swap = || -> io::Result<()> {
        for (_, dest) in pairs {
            aside.push((rename_aside(dest)?, dest));
        }
        for ((src, dest), (old, _)) in pairs.iter().zip(&aside) {
            copy_in(src, dest, old)?;
            copied.push(dest);
        }
        Ok(())
    };
    if let Err(e) = swap() {
        for dest in copied {
            let _ = std::fs::remove_file(dest);
        }
        for (old, dest) in aside.iter().rev() {
            let _ = std::fs::rename(old, dest);
        }
        return Err(e);
    }
    // Best effort: on Windows the old exe may stay locked until process
    // teardown completes; the leftover `.old` is harmless and swept on the
    // next start.
    for (old, _) in &aside {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

/// The name a file is renamed to while it is being replaced.
fn old_name(dest: &Path) -> PathBuf {
    dest.with_extension(match dest.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.old"),
        None => "old".to_string(),
    })
}

/// Move `dest` out of its name, retrying while it is still locked by the
/// exiting process; the path it went to comes back.
///
/// Overwriting a running executable fails on Windows, but renaming it
/// succeeds — and the renamed file is the rollback copy until the swap is
/// through.
fn rename_aside(dest: &Path) -> io::Result<PathBuf> {
    let old = old_name(dest);
    let _ = std::fs::remove_file(&old);
    let mut last = None;
    for attempt in 0..40u32 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        match std::fs::rename(dest, &old) {
            Ok(()) => return Ok(old),
            // On Unix a permission error is the directory's owner, not a
            // lock that will clear: /usr/bin from a deb or rpm install can
            // never be rewritten by the user's own process, and retrying it
            // 40 times only delays the relaunch by 20 seconds. Windows
            // reports the same kind while the old exe is still mapped, so
            // there the loop still has to run.
            Err(e) if cfg!(unix) && e.kind() == io::ErrorKind::PermissionDenied => return Err(e),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("rename failed")))
}

/// Copy `src` to `dest`, which no longer exists, giving it the mode the
/// file it replaces (`old`) had.
fn copy_in(src: &Path, dest: &Path, old: &Path) -> io::Result<()> {
    std::fs::copy(src, dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(old)
            .map(|m| m.permissions())
            .unwrap_or_else(|_| std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::set_permissions(dest, mode);
    }
    #[cfg(not(unix))]
    let _ = old;
    Ok(())
}

/// Remove the one `.old` leftover a previous AppImage update may have left
/// beside the image.
///
/// Deliberately not [`sweep_old_files`]: an AppImage lives wherever the user
/// put it — `~/Downloads`, a USB stick, a directory full of their own files
/// — and deleting every `*.old` in it would be Hydra tidying up somebody
/// else's directory. Only the file this updater itself would have created is
/// removed.
pub fn sweep_appimage_leftover(image: &Path) {
    let old = old_name(image);
    if old != image {
        let _ = std::fs::remove_file(old);
    }
}

/// Remove `.old` leftovers a previous update could not delete (Windows keeps
/// the old exe locked until teardown finishes). Called on the next startup.
pub fn sweep_old_files(install_dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(install_dir) {
        for entry in rd.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().ends_with(".old") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Directory downloads and extractions are staged in, created on the way
/// out and, on Unix, made private to this user.
///
/// Not the OS temp directory: on Linux `/tmp` is shared by every account on
/// the machine and the name is predictable, so another user could plant the
/// archive — or the finisher binary the GUI copies there and then runs —
/// before this process does. `$XDG_RUNTIME_DIR` is per user by contract;
/// failing that, the user's own cache directory with mode 0700. Windows'
/// `%TEMP%` is already per user.
pub fn staging_dir() -> PathBuf {
    let dir = staging_dir_under(
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        cache_home(),
    );
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir
}

/// `$XDG_RUNTIME_DIR/hydra-update` when the runtime directory is there, else
/// `<cache>/hydra/update`; Windows keeps `%TEMP%\hydra-update`.
fn staging_dir_under(runtime: Option<PathBuf>, cache: Option<PathBuf>) -> PathBuf {
    if cfg!(windows) {
        return std::env::temp_dir().join("hydra-update");
    }
    match (runtime.filter(|r| r.is_dir()), cache) {
        (Some(r), _) => r.join("hydra-update"),
        (None, Some(c)) => c.join("hydra").join("update"),
        (None, None) => std::env::temp_dir().join(format!("hydra-update-{}", user_id())),
    }
}

/// The user's cache directory: `~/Library/Caches` on macOS, `$XDG_CACHE_HOME`
/// or `~/.cache` elsewhere. `None` without a home.
fn cache_home() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return Some(PathBuf::from(std::env::var_os("HOME")?).join("Library/Caches"));
    }
    if let Some(x) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(x));
    }
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".cache"))
}

/// Something per user for a temp-dir fallback name, when there is no home
/// to be private in.
fn user_id() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| std::process::id().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uninstall_entry_matches_its_own_install_dir_only() {
        let dir = Path::new(r"C:\Users\me\AppData\Local\Programs\Hydra");
        assert!(same_install_dir(
            r"C:\Users\me\AppData\Local\Programs\Hydra",
            dir
        ));
        assert!(same_install_dir(
            r"c:\users\ME\appdata\local\programs\hydra\",
            dir
        ));
        assert!(same_install_dir(
            "C:/Users/me/AppData/Local/Programs/Hydra",
            dir
        ));
        assert!(!same_install_dir(r"D:\Portable\Hydra", dir));
        assert!(!same_install_dir("", dir));
    }

    #[test]
    fn uninstall_version_is_left_alone_off_windows() {
        if cfg!(windows) {
            return;
        }
        assert!(!set_uninstall_version(Path::new("/opt/hydra"), "1.2.0").unwrap());
    }

    #[test]
    fn appimage_asset_is_matched_by_shape() {
        // Named from the workspace version, which a pre-release tag need not
        // agree with — so the lookup must not depend on the spelling. The
        // `.zsync` sidecar published next to it must never be picked up as
        // the image, and neither must the other architecture's file.
        let mut r = rel("v0.3.4-rc", true);
        for name in [
            "Hydra-0.3.4-x86_64.AppImage",
            "Hydra-0.3.4-aarch64.AppImage",
            "Hydra-0.3.4-x86_64.AppImage.zsync",
            "Hydra-0.3.4-aarch64.AppImage.zsync",
            "Hydra-0.3.4-x86_64.dmg",
        ] {
            r.assets.push(ReleaseAsset {
                name: name.into(),
                browser_download_url: String::new(),
                size: 1,
            });
        }
        assert_eq!(
            r.appimage_asset().map(|a| a.name.as_str()),
            Some(appimage_asset_name("0.3.4").as_str())
        );
        assert!(appimage_asset_name("0.3.4").ends_with(".AppImage"));
    }

    #[test]
    fn appimage_asset_absent_when_the_release_ships_none() {
        let r = rel("v0.3.4", false);
        assert!(r.appimage_asset().is_none());
    }

    #[test]
    fn appimage_method_ignores_the_mount_it_runs_from() {
        // Not an AppImage run: `update_method` must keep answering from the
        // directory, which is what every other install depends on.
        assert!(appimage_path().is_none(), "APPIMAGE set in the test env");
        assert!(!appimage_needs_auth());
        assert!(UpdateMethod::AppImage.is_self_update());
    }

    #[test]
    fn version_compare() {
        assert!(is_newer("v0.2.4", "0.2.3"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.2.10", "0.2.9"));
        assert!(!is_newer("0.2.3", "0.2.3"));
        assert!(!is_newer("v0.2.2", "0.2.3"));
        assert!(is_newer("0.3.0-rc1", "0.2.9"));
        // On an equal core, stable outranks rc and rc numbers order.
        assert!(is_newer("0.3.0", "0.3.0-rc2"));
        assert!(!is_newer("0.3.0-rc1", "0.3.0"));
        assert!(is_newer("0.3.0-rc2", "0.3.0-rc1"));
        assert!(is_newer("v0.3.0-rc1", "0.3.0-rc"));
        assert!(!is_newer("0.3.0-rc", "0.3.0-rc"));
        // Unparsable input must never look like an upgrade.
        assert!(!is_newer("nightly", "0.2.3"));
        assert!(!is_newer("0.3.0", "unknown"));
    }

    fn rel(tag: &str, prerelease: bool) -> Release {
        Release {
            tag_name: tag.into(),
            prerelease,
            name: String::new(),
            body: String::new(),
            html_url: String::new(),
            published_at: String::new(),
            assets: vec![],
        }
    }

    #[test]
    fn beta_channel_picks_the_newest_prerelease() {
        // Flagged pre-releases and unflagged `-rc` tags both qualify; the
        // highest version wins regardless of list order.
        let list = vec![
            rel("v0.2.4", false),
            rel("v0.3.0-rc1", true),
            rel("v0.3.0-rc2", false), // forgot the flag; the tag still counts
            rel("v0.2.3", false),
        ];
        assert_eq!(
            newest_prerelease(list).map(|r| r.tag_name),
            Some("v0.3.0-rc2".into())
        );
        assert!(newest_prerelease(vec![rel("v0.2.4", false)]).is_none());
    }

    #[test]
    fn asset_names_follow_the_release_pipeline() {
        let gui = gui_asset_name("0.2.4");
        let cli = cli_asset_name("0.2.4");
        assert!(gui.starts_with("hydra-0.2.4-"));
        assert!(cli.starts_with("hydra-cli-0.2.4-"));
        for n in [&gui, &cli] {
            assert!(n.contains(os_tag()) && n.contains(arch_tag()));
            assert!(n.ends_with(archive_ext()));
        }
    }

    #[test]
    fn the_portable_asset_is_not_the_regular_bundle() {
        let mut r = rel("v1.1.6", false);
        for name in [gui_asset_name("1.1.6"), portable_asset_name("1.1.6")] {
            r.assets.push(ReleaseAsset {
                name,
                browser_download_url: String::new(),
                size: 0,
            });
        }
        assert_eq!(
            portable_asset_name("1.1.6"),
            format!("hydra-1.1.6-windows-portable-{}.zip", arch_tag())
        );
        assert_eq!(
            r.portable_asset().map(|a| a.name.as_str()),
            Some(portable_asset_name("1.1.6").as_str())
        );
        assert_ne!(
            r.portable_asset().map(|a| &a.name),
            r.gui_asset().map(|a| &a.name)
        );
        assert!(rel("v1.1.6", false).portable_asset().is_none());
    }

    #[test]
    fn a_portable_install_is_hydra_under_app_beside_the_launcher() {
        let root = std::env::temp_dir().join(format!("hydra-portable-{}", std::process::id()));
        let app = root.join("HydraPortable").join("App").join("Hydra");
        std::fs::create_dir_all(&app).unwrap();

        assert!(!is_portable_install(&app), "no launcher yet");
        std::fs::write(root.join("HydraPortable").join("HydraPortable.exe"), b"").unwrap();
        assert!(is_portable_install(&app));

        let elsewhere = root.join("HydraPortable").join("App").join("Other");
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert!(!is_portable_install(&elsewhere));
        assert!(!is_portable_install(&root.join("HydraPortable")));
        assert!(!is_portable_install(Path::new("/")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rc_tag_finds_assets_named_from_the_manifest_version() {
        // The real pipeline: tag `v0.3.2-rc`, manifest still `0.3.2`, so
        // every asset is spelled without the suffix. Missing it here is
        // what made the beta channel report "you are on the latest
        // version" while an rc was published.
        let mut r = rel("v0.3.2-rc", true);
        for name in [gui_asset_name("0.3.2"), cli_asset_name("0.3.2")] {
            r.assets.push(ReleaseAsset {
                name,
                browser_download_url: String::new(),
                size: 0,
            });
        }
        assert_eq!(
            r.gui_asset().map(|a| a.name.as_str()),
            Some(gui_asset_name("0.3.2").as_str())
        );
        assert_eq!(
            r.cli_asset().map(|a| a.name.as_str()),
            Some(cli_asset_name("0.3.2").as_str())
        );

        // A manifest that does carry the suffix keeps working, and a
        // release without an asset for this machine still resolves to None.
        let mut suffixed = rel("v0.3.2-rc", true);
        suffixed.assets.push(ReleaseAsset {
            name: gui_asset_name("0.3.2-rc"),
            browser_download_url: String::new(),
            size: 0,
        });
        assert_eq!(
            suffixed.gui_asset().map(|a| a.name.as_str()),
            Some(gui_asset_name("0.3.2-rc").as_str())
        );
        assert!(suffixed.cli_asset().is_none());
        assert!(rel("v0.3.2", false).gui_asset().is_none());
    }

    #[test]
    fn version_core_strips_suffixes() {
        assert_eq!(version_core("v0.3.2-rc"), "0.3.2");
        assert_eq!(version_core("0.3.2-rc.2"), "0.3.2");
        assert_eq!(version_core("0.3.2+build7"), "0.3.2");
        assert_eq!(version_core("0.3.2"), "0.3.2");
    }

    #[test]
    fn notes_lose_html_comments() {
        let body = "<!-- Release notes generated using configuration in \
            .github/release.yml at v0.3.4-rc -->\n\n\n## What's Changed\n\
            - fix <!-- inline --> things\n\n\n\n- more\n\n";
        let out = clean_notes(body);
        assert!(!out.contains("<!--") && !out.contains("-->"));
        assert!(out.starts_with("## What's Changed"));
        assert_eq!(out, "## What's Changed\n- fix  things\n\n- more");
        // Nothing to strip leaves the notes as they were.
        assert_eq!(clean_notes("## Notes\n- one"), "## Notes\n- one");
        // An unterminated comment takes the rest with it.
        assert_eq!(clean_notes("keep\n<!-- dangling"), "keep");
    }

    #[test]
    fn notes_lose_repeated_sections() {
        // The real v0.3.10 body: the release job ran again over the tag and
        // `generate_release_notes` appended the same block each time.
        let once = "<!-- Release notes generated using configuration in \
            .github/release.yml at v0.3.10 -->\n\n## What's Changed\n\
            ### 🚀 Features\n* Add Firefox extension store link and update \
            localization strings by @ja7ad in https://github.com/ja7ad/hydra/pull/27\
            \n\n\n**Full Changelog**: \
            https://github.com/ja7ad/hydra/compare/v0.3.9...v0.3.10";
        let body = [once; 5].join("\n\n");
        let out = clean_notes(&body);
        assert_eq!(out, clean_notes(once));
        assert_eq!(out.matches("What's Changed").count(), 1);
        assert_eq!(out.matches("Full Changelog").count(), 1);
        assert!(out.starts_with("## What's Changed"));
        // Sections that only share a heading are both kept.
        let two = "## What's Changed\n- one\n\n## What's Changed\n- two";
        assert_eq!(clean_notes(two), two);
        // Notes without any heading are left alone, repeats and all.
        assert_eq!(clean_notes("- one\n- one"), "- one\n- one");
    }

    #[test]
    fn packaged_installs_match_their_own_naming() {
        // Real v0.3.4-rc names. Every format spells the version and the
        // architecture its own way, which is why the match is by shape.
        let mut r = rel("v0.3.4-rc", true);
        for name in [
            "hydra-download-manager_0.3.4_arm64.deb",
            "hydra-download-manager_0.3.4_amd64.deb",
            "hydra-download-manager-0.3.4-1.aarch64.rpm",
            "hydra-download-manager-0.3.4-1.x86_64.rpm",
            "Hydra-0.3.4-arm64.dmg",
            "Hydra-0.3.4-arm64.pkg",
            "Hydra-0.3.4-x86_64.dmg",
            "hydra-0.3.4-windows-arm64-setup.exe",
            "hydra-0.3.4-windows-x64-setup.exe",
            "hydra-0.3.4-linux-arm64.tar.gz",
        ] {
            r.assets.push(ReleaseAsset {
                name: name.into(),
                browser_download_url: String::new(),
                size: 0,
            });
        }
        for (kind, arch, want) in [
            (
                PackageKind::Deb,
                "arm64",
                "hydra-download-manager_0.3.4_arm64.deb",
            ),
            (
                PackageKind::Rpm,
                "aarch64",
                "hydra-download-manager-0.3.4-1.aarch64.rpm",
            ),
            (PackageKind::Dmg, "arm64", "Hydra-0.3.4-arm64.dmg"),
            (PackageKind::Pkg, "arm64", "Hydra-0.3.4-arm64.pkg"),
            (PackageKind::Dmg, "x86_64", "Hydra-0.3.4-x86_64.dmg"),
            (PackageKind::Exe, "x64", "hydra-0.3.4-windows-x64-setup.exe"),
        ] {
            let (prefix, ext, _) = kind.asset_shape();
            assert_eq!(
                r.package_asset_for(prefix, ext, arch)
                    .map(|a| a.name.as_str()),
                Some(want),
                "{kind:?} {arch}"
            );
        }
        // A release without installables offers none.
        assert!(rel("v0.3.4-rc", true)
            .package_asset_for(PACKAGE_NAME, ".deb", "amd64")
            .is_none());
    }

    #[test]
    fn app_bundles_are_recognised_and_mapped() {
        // The bundle's GUI is `Contents/MacOS/Hydra Download Manager`, not
        // the archive's `hydra-gui`: without the rename a per-file swap
        // replaces the CLI and relaunches the same old app.
        let app = Path::new("/Applications/Hydra Download Manager.app/Contents/MacOS");
        assert!(in_app_bundle(app));
        let bundle = app_bundle_root(app).unwrap();
        assert_eq!(
            bundle,
            Path::new("/Applications/Hydra Download Manager.app")
        );
        assert_eq!(bundle_bin_dir(&bundle), app);
        let exec = "Hydra Download Manager";
        assert_eq!(
            bundle_dest(&bundle, exec, "hydra-gui"),
            Some(app.join(exec))
        );
        for name in ["hydra", "hydra-host", "hydra-updater"] {
            assert_eq!(bundle_dest(&bundle, exec, name), Some(app.join(name)));
        }
        // Archive-layout files have no place in a bundle.
        for name in ["logo.png", "LICENSE", "README.md"] {
            assert_eq!(bundle_dest(&bundle, exec, name), None);
        }
        assert!(!in_app_bundle(Path::new("/usr/local/bin")));
        assert!(!in_app_bundle(Path::new("/home/j/apps/hydra")));
        assert_eq!(app_bundle_root(Path::new("/usr/local/bin")), None);
    }

    /// A minimal bundle, as the packaging scripts lay one out.
    fn fake_bundle(dir: &Path, exec: &str, version: &str) -> PathBuf {
        let bundle = dir.join("Hydra Download Manager.app");
        let bin = bundle_bin_dir(&bundle);
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(exec), b"old gui").unwrap();
        std::fs::write(bin.join("hydra"), b"old cli").unwrap();
        std::fs::write(
            bundle.join("Contents").join("Info.plist"),
            format!(
                "<plist version=\"1.0\">\n<dict>\n    \
                 <key>CFBundleExecutable</key><string>{exec}</string>\n    \
                 <key>CFBundleVersion</key><string>{version}</string>\n    \
                 <key>CFBundleShortVersionString</key>\n    <string>{version}</string>\n\
                 </dict>\n</plist>\n"
            ),
        )
        .unwrap();
        bundle
    }

    #[test]
    fn bundle_update_renames_the_gui_and_restamps_the_plist() {
        let tmp = std::env::temp_dir().join(format!("hydra-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let src = tmp.join("hydra-0.4.0-macos-arm64");
        std::fs::create_dir_all(&src).unwrap();
        for (name, body) in [
            ("hydra-gui", "new gui"),
            ("hydra", "new cli"),
            // Not in the bundle: skipped, never added.
            ("hydra-host", "new host"),
            ("logo.png", "png"),
        ] {
            std::fs::write(src.join(name), body).unwrap();
        }
        let exec = "Hydra Download Manager";
        let bundle = fake_bundle(&tmp, exec, "0.3.9");
        let bin = bundle_bin_dir(&bundle);

        let report = apply_with(
            &src,
            &bin,
            &ApplyOptions {
                version: Some("0.4.0-rc1".into()),
            },
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(bin.join(exec)).unwrap(), "new gui");
        assert_eq!(
            std::fs::read_to_string(bin.join("hydra")).unwrap(),
            "new cli"
        );
        assert!(!bin.join("hydra-host").exists());
        assert!(report.replaced.contains(&"hydra-gui".to_string()));
        assert!(report.skipped.contains(&"logo.png".to_string()));

        // Both version keys are restamped, and the pre-release suffix is
        // dropped: CFBundleVersion takes integers only.
        let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap();
        assert_eq!(plist_string(&plist, "CFBundleVersion"), Some("0.4.0"));
        assert_eq!(
            plist_string(&plist, "CFBundleShortVersionString"),
            Some("0.4.0")
        );
        assert_eq!(plist_string(&plist, "CFBundleExecutable"), Some(exec));
        assert_eq!(bundle_exec_name(&bundle), exec);
        // No `.plist.new` left behind by the atomic write.
        assert!(!bundle.join("Contents/Info.plist.new").exists());

        // A plain (non-bundle) install still copies name over name.
        let plain = tmp.join("bin");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("hydra-gui"), b"old").unwrap();
        apply(&src, &plain).unwrap();
        assert_eq!(
            std::fs::read_to_string(plain.join("hydra-gui")).unwrap(),
            "new gui"
        );
        assert!(!plain.join("hydra").exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_copied_cli_alias_is_refreshed_but_a_linked_one_is_left_alone() {
        // Windows may only be able to give `hya` the CLI's bits rather than
        // its name, and those bits go stale the moment `hydra` is replaced.
        let tmp = std::env::temp_dir().join(format!("hydra-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let src = tmp.join("src");
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let cli = cli_file_name();
        std::fs::write(src.join(cli), b"new cli").unwrap();
        std::fs::write(bin.join(cli), b"old cli").unwrap();
        std::fs::write(bin.join(CLI_ALIAS), b"old cli").unwrap();

        let report = apply(&src, &bin).unwrap();
        assert_eq!(
            std::fs::read_to_string(bin.join(CLI_ALIAS)).unwrap(),
            "new cli"
        );
        assert!(report.notes.iter().any(|n| n.contains(CLI_ALIAS)));

        // A symlinked alias resolves through the name, so it is already
        // current — and rewriting it would turn it into a second copy.
        #[cfg(unix)]
        {
            std::fs::write(src.join(cli), b"newer cli").unwrap();
            std::fs::remove_file(bin.join(CLI_ALIAS)).unwrap();
            std::os::unix::fs::symlink(cli, bin.join(CLI_ALIAS)).unwrap();
            apply(&src, &bin).unwrap();
            assert!(std::fs::symlink_metadata(bin.join(CLI_ALIAS))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                std::fs::read_to_string(bin.join(CLI_ALIAS)).unwrap(),
                "newer cli"
            );
        }

        // An install that never had the short name does not gain one.
        std::fs::remove_file(bin.join(CLI_ALIAS)).unwrap();
        apply(&src, &bin).unwrap();
        assert!(!bin.join(CLI_ALIAS).exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_directory_we_own_updates_itself() {
        // Ownership is what decides, and a temp dir is ours: no prompt, no
        // installer detour.
        let dir = std::env::temp_dir().join(format!("hydra-method-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(update_method(&dir), UpdateMethod::InPlace);
        assert!(update_method(&dir).is_self_update());
        assert!(update_target_is_writable(&dir));
        // A path that does not exist is writable by nobody; without an
        // elevation helper that is the installer's job.
        let missing = dir.join("nope");
        assert!(!update_target_is_writable(&missing));
        assert_eq!(
            update_method(&missing).is_self_update(),
            elevation().is_some()
        );
        assert!(!UpdateMethod::Package.is_self_update());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writability_is_probed_not_guessed() {
        let dir = std::env::temp_dir().join(format!("hydra-wtest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(dir_is_writable(&dir));
        assert!(!dir_is_writable(&dir.join("no-such-subdir")));
        // The probe leaves nothing behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn elevated_commands_survive_quoting() {
        // Install paths hold spaces ("Hydra Download Manager.app"), and on
        // macOS the whole command crosses two levels of quoting: an
        // AppleScript string literal holding a shell command line. One
        // missed quote there runs the wrong argv as root.
        let os = std::ffi::OsStr::new;
        let argv = [
            os("/tmp/hydra-updater"),
            os("--install-dir"),
            os("/Applications/Hydra Download Manager.app/Contents/MacOS"),
        ];
        let cmd = shell_command(&argv);
        assert_eq!(
            cmd,
            "'/tmp/hydra-updater' '--install-dir' \
             '/Applications/Hydra Download Manager.app/Contents/MacOS'"
                .replace("\n             ", " ")
        );
        assert_eq!(applescript_string(&cmd), format!("\"{cmd}\""));

        // A quote inside a path closes and reopens the shell quoting instead
        // of ending the argument; backslashes and quotes are then escaped
        // again on their way into AppleScript.
        let odd = [os(r#"/tmp/it's a "dir"\x"#)];
        assert_eq!(shell_command(&odd), r#"'/tmp/it'\''s a "dir"\x'"#);
        assert_eq!(
            applescript_string(&shell_command(&odd)),
            r#""'/tmp/it'\\''s a \"dir\"\\x'""#
        );
    }

    #[test]
    fn sums_parsing() {
        let sums = "abc\n\
            0123456789012345678901234567890123456789012345678901234567890123  hydra-0.2.4-linux-amd64.tar.gz\n\
            ABCDEF6789012345678901234567890123456789012345678901234567890123 *hydra-0.2.4-windows-amd64.zip\n";
        assert_eq!(
            sum_for(sums, "hydra-0.2.4-linux-amd64.tar.gz").as_deref(),
            Some("0123456789012345678901234567890123456789012345678901234567890123")
        );
        // Binary-mode `*` prefix and uppercase hex both normalise.
        assert_eq!(
            sum_for(sums, "hydra-0.2.4-windows-amd64.zip").as_deref(),
            Some("abcdef6789012345678901234567890123456789012345678901234567890123")
        );
        assert_eq!(sum_for(sums, "missing.tar.gz"), None);
    }

    #[test]
    fn release_json_parses() {
        let json = "{
            \"tag_name\": \"v0.2.4\",
            \"name\": \"hydra 0.2.4\",
            \"body\": \"## What's Changed\\n- faster\",
            \"html_url\": \"https://github.com/ja7ad/hydra/releases/tag/v0.2.4\",
            \"published_at\": \"2026-08-01T00:00:00Z\",
            \"assets\": [
                {\"name\": \"hydra-0.2.4-macos-arm64.tar.gz\",
                 \"browser_download_url\": \"https://example.com/a.tar.gz\",
                 \"size\": 123}
            ]
        }";
        let r: Release = serde_json::from_str(json).unwrap();
        assert_eq!(r.version(), "0.2.4");
        assert_eq!(r.assets.len(), 1);
        assert!(r.asset("hydra-0.2.4-macos-arm64.tar.gz").is_some());
    }

    #[test]
    fn an_insecure_api_override_is_refused_off_loopback() {
        // The mock flow: plain http on this machine is fine.
        assert_eq!(
            accepted_api_base("http://127.0.0.1:8642/"),
            Some("http://127.0.0.1:8642".into())
        );
        assert_eq!(
            accepted_api_base(" http://localhost:8642 "),
            Some("http://localhost:8642".into())
        );
        assert_eq!(
            accepted_api_base("https://mirror.example/api"),
            Some("https://mirror.example/api".into())
        );
        // A cleartext base anywhere else is a downgrade nobody asked for.
        assert_eq!(accepted_api_base("http://mirror.example/api"), None);
        assert_eq!(accepted_api_base("http://10.0.0.7:8642"), None);
        assert_eq!(accepted_api_base(""), None);
        assert_eq!(accepted_api_base("ftp://x"), None);
    }

    #[test]
    fn a_release_is_prerelease_by_flag_or_by_tag() {
        assert!(rel("v0.3.0-rc1", false).is_prerelease());
        assert!(rel("v0.3.0", true).is_prerelease());
        assert!(rel("v0.3.0-beta.2", false).is_prerelease());
        assert!(!rel("v0.3.0", false).is_prerelease());
        assert!(!rel("v1.0.0+build7", false).is_prerelease());
        // The stable channel's fallback: the newest release that is stable
        // both ways, whatever order GitHub lists them in.
        let list = vec![
            rel("v1.0.0-rc", false), // an rc published without the flag
            rel("v0.9.0", false),
            rel("v0.9.1", false),
            rel("v0.9.2", true),
        ];
        assert_eq!(
            newest_stable(list).map(|r| r.tag_name),
            Some("v0.9.1".into())
        );
        assert!(newest_stable(vec![rel("v1.0.0-rc", true)]).is_none());
    }

    #[test]
    fn homebrew_installs_belong_to_brew() {
        // A keg, a cask payload, and linuxbrew's prefix are brew's to
        // rewrite; a dragged app and a hand-unpacked tarball are not.
        for (dir, want) in [
            ("/opt/homebrew/Cellar/hydra/1.0.0/bin", Some(Brew::Formula)),
            ("/usr/local/Cellar/hydra/1.0.0/bin", Some(Brew::Formula)),
            (
                "/opt/homebrew/Caskroom/hydra/1.0.0/Hydra Download Manager.app/Contents/MacOS",
                Some(Brew::Cask),
            ),
            ("/usr/local/Caskroom/hydra/1.0.0", Some(Brew::Cask)),
            (
                "/home/linuxbrew/.linuxbrew/Cellar/hydra/1.0.0/bin",
                Some(Brew::Formula),
            ),
            ("/home/linuxbrew/.linuxbrew/bin", Some(Brew::Formula)),
            ("/usr/local/bin", None),
            (
                "/Applications/Hydra Download Manager.app/Contents/MacOS",
                None,
            ),
            ("/home/j/.local/bin", None),
        ] {
            assert_eq!(brew_owned(Path::new(dir)), want, "{dir}");
        }
        assert_eq!(
            package_manager_hint(Path::new("/opt/homebrew/Cellar/hydra/1.0.0/bin")),
            Some("brew upgrade ja7ad/tap/hydra")
        );
        assert_eq!(
            package_manager_hint(Path::new("/usr/local/Caskroom/hydra/1.0.0")),
            Some("brew upgrade --cask ja7ad/tap/hydra")
        );
        assert_eq!(package_manager_hint(Path::new("/home/j/.local/bin")), None);
        // Whatever the ownership probe says about the directory, brew's own
        // tree is a package install and never updated in place.
        assert!(package_managed(Path::new(
            "/usr/local/Cellar/hydra/1.0.0/bin"
        )));
        assert!(!package_managed(Path::new("/home/j/.local/bin")) || is_flatpak());
    }

    #[test]
    fn a_sums_file_without_the_asset_refuses_the_archive() {
        let dir = std::env::temp_dir().join(format!("hydra-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("hydra-1.0.0-linux-amd64.tar.gz");
        let write = || std::fs::write(&archive, b"archive bytes").unwrap();
        write();
        let digest = file_sha256(&archive).unwrap();
        let listed = format!("{digest}  hydra-1.0.0-linux-amd64.tar.gz\n");
        assert_eq!(
            verify_archive(&archive, "hydra-1.0.0-linux-amd64.tar.gz", Some(&listed)).unwrap(),
            Verification::Verified
        );
        assert!(archive.exists(), "a verified archive stays");

        // Published sums that do not mention this file are not a pass.
        let other = format!("{digest}  hydra-1.0.0-macos-arm64.tar.gz\n");
        let err =
            verify_archive(&archive, "hydra-1.0.0-linux-amd64.tar.gz", Some(&other)).unwrap_err();
        assert!(err.to_string().contains("no entry"), "{err}");
        assert!(!archive.exists(), "a refused archive is removed");

        write();
        let wrong = format!("{}  hydra-1.0.0-linux-amd64.tar.gz\n", "0".repeat(64));
        let err =
            verify_archive(&archive, "hydra-1.0.0-linux-amd64.tar.gz", Some(&wrong)).unwrap_err();
        assert!(err.to_string().contains("mismatch"), "{err}");
        assert!(!archive.exists());

        // No sums published at all: accepted, and said so.
        write();
        assert_eq!(
            verify_archive(&archive, "hydra-1.0.0-linux-amd64.tar.gz", None).unwrap(),
            Verification::Unpublished
        );
        assert!(archive.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_is_private_to_the_user() {
        let base = std::env::temp_dir().join(format!("hydra-staging-{}", std::process::id()));
        let runtime = base.join("run");
        let cache = base.join("cache");
        std::fs::create_dir_all(&runtime).unwrap();
        if cfg!(windows) {
            assert!(staging_dir_under(Some(runtime), Some(cache)).ends_with("hydra-update"));
            return;
        }
        // The runtime directory wins while it is there; a runtime directory
        // that is not (the variable outliving the session) is ignored.
        assert_eq!(
            staging_dir_under(Some(runtime.clone()), Some(cache.clone())),
            runtime.join("hydra-update")
        );
        assert_eq!(
            staging_dir_under(Some(base.join("gone")), Some(cache.clone())),
            cache.join("hydra").join("update")
        );
        assert_eq!(
            staging_dir_under(None, Some(cache.clone())),
            cache.join("hydra").join("update")
        );
        // Never the bare shared temp directory.
        let fallback = staging_dir_under(None, None);
        assert_ne!(fallback, std::env::temp_dir().join("hydra-update"));
        assert!(fallback.starts_with(std::env::temp_dir()));

        // The real one is created with mode 0700.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir = staging_dir();
            assert!(dir.is_dir());
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_swap_puts_every_file_back() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = std::env::temp_dir().join(format!("hydra-rollback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let src = tmp.join("src");
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(src.join("hydra-gui"), b"new gui").unwrap();
        std::fs::write(src.join("hydra"), b"new cli").unwrap();
        std::fs::write(src.join("hydra-host"), b"new host").unwrap();
        // The middle file cannot be read, so its copy fails after another
        // file has already been swapped.
        std::fs::set_permissions(src.join("hydra"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        if std::fs::read(src.join("hydra")).is_ok() {
            // Running as root: the mode does not bite, and neither does the scenario.
            let _ = std::fs::remove_dir_all(&tmp);
            return;
        }
        for name in ["hydra-gui", "hydra", "hydra-host"] {
            std::fs::write(bin.join(name), format!("old {name}")).unwrap();
            std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }

        let err = apply(&src, &bin).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        for name in ["hydra-gui", "hydra", "hydra-host"] {
            assert_eq!(
                std::fs::read_to_string(bin.join(name)).unwrap(),
                format!("old {name}"),
                "{name} is as it was"
            );
            assert!(
                !bin.join(format!("{name}.old")).exists(),
                "no {name}.old left behind"
            );
        }
        assert_eq!(
            std::fs::read_dir(&bin).unwrap().count(),
            3,
            "nothing extra in the install"
        );

        // Once every source is readable the same swap goes through whole,
        // keeps the executable bit, and leaves no `.old` behind.
        std::fs::set_permissions(src.join("hydra"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let report = apply(&src, &bin).unwrap();
        assert_eq!(report.replaced.len(), 3);
        assert_eq!(
            std::fs::read_to_string(bin.join("hydra")).unwrap(),
            "new cli"
        );
        assert_eq!(
            std::fs::metadata(bin.join("hydra"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0o111
        );
        assert_eq!(std::fs::read_dir(&bin).unwrap().count(), 3);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resigned_bundles_of_different_versions_share_one_designated_requirement() {
        let tmp = std::env::temp_dir().join(format!("hydra-designated-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let requirement = |version: &str| {
            let dir = tmp.join(version);
            let bundle = fake_bundle(&dir, "Hydra Download Manager", version);
            std::fs::copy(
                "/usr/bin/true",
                bundle_bin_dir(&bundle).join("Hydra Download Manager"),
            )
            .unwrap();
            assert!(resign_bundle(&bundle).unwrap());
            let out = std::process::Command::new("/usr/bin/codesign")
                .args(["-d", "-r-"])
                .arg(&bundle)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("designated => ").map(str::to_owned))
                .expect("a designated requirement")
        };
        let old = requirement("0.9.0");
        assert_eq!(old, requirement("1.0.0"));
        assert_eq!(old, "identifier \"io.github.ja7ad.hydra\"");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_swapped_bundle_loses_its_quarantine_flag() {
        let tmp = std::env::temp_dir().join(format!("hydra-quarantine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let bundle = fake_bundle(&tmp, "Hydra Download Manager", "0.9.0");
        let exe = bundle_bin_dir(&bundle).join("Hydra Download Manager");
        let flag = |path: &Path| {
            std::process::Command::new("/usr/bin/xattr")
                .args(["-p", "com.apple.quarantine"])
                .arg(path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        let ok = std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "com.apple.quarantine", "0083;00000000;Safari;"])
            .arg(&exe)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok && flag(&exe), "the fixture carries the flag");
        assert!(strip_quarantine(&bundle).unwrap());
        assert!(!flag(&exe), "the flag is gone from the executable");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_is_flatpak() {
        assert!(
            !is_flatpak()
                || std::env::var_os("FLATPAK_ID").is_some()
                || std::path::Path::new("/.flatpak-info").exists()
        );
        std::env::set_var("FLATPAK_ID", "io.github.ja7ad.hydra");
        assert!(is_flatpak());
        assert!(package_managed(std::path::Path::new("/app/bin")));
        std::env::remove_var("FLATPAK_ID");
    }
}
