//! Bounded HTTPS package downloads, the default catalog, and user-added mirrors.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hya_plugin_api::{ErrorCode, PluginError};
use serde::{Deserialize, Serialize};

use crate::{http, matcher::HostList, package};

/// The permanent catalog shared by Hydra's website, desktop app and CLI.
pub const DEFAULT_INDEX_URL: &str = "https://hydra.javad.dev/plugins.json";

#[cfg(debug_assertions)]
static DEBUG_CATALOG: std::sync::OnceLock<url::Url> = std::sync::OnceLock::new();

#[cfg(debug_assertions)]
fn debug_catalog_url(address: &str) -> Result<url::Url, PluginError> {
    let url = url::Url::parse(address).map_err(error)?;
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(_)) => false,
        None => false,
    };
    if url.scheme() != "http"
        || !loopback
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(error(
            "debug plugin catalogs require an HTTP localhost or IPv4 loopback URL without credentials",
        ));
    }
    Ok(url)
}

/// Overrides the default catalog for a debug process and permits its local HTTP origin.
///
/// # Errors
/// Returns an error for unsupported URLs or a second configuration attempt.
#[cfg(debug_assertions)]
pub fn configure_debug_catalog(address: &str) -> Result<(), PluginError> {
    let url = debug_catalog_url(address)?;
    DEBUG_CATALOG
        .set(url)
        .map_err(|_| error("debug plugin catalog is already configured"))
}

fn default_catalog_url() -> &'static str {
    #[cfg(debug_assertions)]
    if let Some(url) = DEBUG_CATALOG.get() {
        return url.as_str();
    }
    DEFAULT_INDEX_URL
}

fn debug_distribution_url(url: &url::Url) -> bool {
    #[cfg(debug_assertions)]
    return DEBUG_CATALOG
        .get()
        .is_some_and(|catalog| catalog.origin() == url.origin());
    #[cfg(not(debug_assertions))]
    {
        let _ = url;
        false
    }
}

fn distribution_url(url: &url::Url) -> bool {
    (url.scheme() == "https" || debug_distribution_url(url))
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

fn error(e: impl std::fmt::Display) -> PluginError {
    PluginError::new(ErrorCode::InvalidInput, e.to_string())
}

/// An inspected installation source kept alive until consent is applied.
#[derive(Clone, Debug)]
pub struct Prepared {
    /// Local source passed to the installer after consent.
    pub path: PathBuf,
    temporary: Option<Arc<tempfile::TempPath>>,
}
impl Prepared {
    /// Fetches HTTPS sources or inspects local packages without installing them.
    pub fn new(source: &str, sha256: Option<&str>) -> Result<Self, PluginError> {
        let prepared = if url::Url::parse(source)
            .is_ok_and(|url| url.scheme() == "https" || debug_distribution_url(&url))
        {
            let bytes = fetch(source)?;
            package::open(&bytes, sha256).map_err(error)?;
            let mut file = tempfile::NamedTempFile::new().map_err(error)?;
            std::io::Write::write_all(&mut file, &bytes).map_err(error)?;
            let path = Arc::new(file.into_temp_path());
            Self {
                path: path.to_path_buf(),
                temporary: Some(path),
            }
        } else {
            if source.contains("://") {
                return Err(error("plugin downloads require HTTPS"));
            }
            let path = PathBuf::from(source);
            if let Some(hash) = sha256 {
                if path.is_dir() {
                    return Err(error("a directory has no archive checksum"));
                }
                package::open(&std::fs::read(&path).map_err(error)?, Some(hash)).map_err(error)?;
            }
            Self {
                path,
                temporary: None,
            }
        };
        crate::manager::Manager::inspect(&prepared.path)?;
        Ok(prepared)
    }
    /// Whether this source was fetched over HTTPS.
    pub fn remote(&self) -> bool {
        self.temporary.is_some()
    }
}

/// Fetches a public HTTPS distribution document through Hydra's transport.
pub fn fetch(address: &str) -> Result<Vec<u8>, PluginError> {
    let url = url::Url::parse(address).map_err(error)?;
    if !distribution_url(&url) {
        return Err(error(
            "distribution URLs must use HTTPS without credentials",
        ));
    }
    let connector = hya_net::tls::TlsCapableConnector::new().map_err(error)?;
    let mut policy = http::HttpPolicy {
        proxy: None,
        http: HostList::parse(&[if debug_distribution_url(&url) {
            format!(
                "{}:{}",
                url.host_str().unwrap_or_default(),
                url.port_or_known_default().unwrap_or(80)
            )
        } else {
            "*".into()
        }])
        .map_err(error)?,
        cookies: HostList::parse(&[]).map_err(error)?,
        user_cookies: hya_net::CookieJar::new(),
        session: hya_net::CookieJar::new(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(error)?;
    let response = runtime.block_on(http::perform(
        &connector,
        &mut policy,
        &http::HttpRequest {
            method: "GET".into(),
            url: address.into(),
            headers: Default::default(),
            body_b64: None,
        },
    ))?;
    if response.status != 200
        || !url::Url::parse(&response.url).is_ok_and(|url| distribution_url(&url))
    {
        return Err(error(format!(
            "distribution download returned {}",
            response.status
        )));
    }
    hya_net::base64::decode(&response.body_b64).ok_or_else(|| error("invalid response body"))
}

pub(crate) fn compatible(api: i32, minimum: Option<&str>) -> Result<(), PluginError> {
    if api != hya_plugin_api::abi::API_MAJOR {
        return Err(error("unsupported plugin API"));
    }
    if let Some(minimum) = minimum {
        let required = semver::Version::parse(minimum).map_err(error)?;
        let current = semver::Version::parse(env!("HYDRA_PRODUCT_VERSION")).map_err(error)?;
        if required > current {
            return Err(error(format!(
                "plugin requires Hydra {required}; installed {current}"
            )));
        }
    }
    Ok(())
}

/// A user-pinned index location and optional signature verification key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexSource {
    pub url: String,
    pub key: Option<String>,
}
/// A publisher's distribution catalog in JSON or TOML format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "Catalog")]
pub struct Index {
    pub schema: u32,
    pub name: String,
    pub plugins: Vec<Entry>,
}
/// A package advertised by an index, with optional archive and publisher pins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub package: String,
    pub sha256: Option<String>,
    pub publisher_key: Option<String>,
    pub api: i32,
    pub min_hydra: Option<String>,
    #[serde(alias = "description")]
    pub summary: Option<String>,
    /// Whether the catalog lists this as a Hydra-maintained plugin.
    #[serde(default)]
    pub is_official: bool,
}
#[derive(Deserialize)]
struct Catalog {
    schema: u32,
    name: String,
    plugins: Vec<CatalogEntry>,
}

#[derive(Deserialize)]
struct CatalogEntry {
    #[serde(flatten)]
    entry: Entry,
    download: Option<Downloads>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Downloads {
    Link(String),
    Choices(Vec<Download>),
}

#[derive(Deserialize)]
struct Download {
    caption: String,
    link: String,
    platform: Option<String>,
    sha256: Option<String>,
}

impl Catalog {
    fn resolve(self, platform: &str) -> Result<Index, PluginError> {
        let mut plugins = Vec::with_capacity(self.plugins.len());
        for CatalogEntry {
            mut entry,
            download,
        } in self.plugins
        {
            match download {
                Some(Downloads::Link(link)) => entry.package = link,
                Some(Downloads::Choices(choices)) => {
                    if choices.is_empty()
                        || choices.iter().any(|choice| {
                            choice.caption.trim().is_empty()
                                || choice
                                    .platform
                                    .as_ref()
                                    .is_some_and(|value| value.trim().is_empty())
                                || !url::Url::parse(&choice.link).is_ok_and(|url| {
                                    distribution_url(&url)
                                        && url.fragment().is_none()
                                        && url.path().to_ascii_lowercase().ends_with(".hyaplugin")
                                })
                                || choice.sha256.as_ref().is_some_and(|hash| {
                                    hash.len() != 64
                                        || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                                })
                        })
                    {
                        return Err(error("invalid plugin downloads"));
                    }
                    let selected = choices
                        .iter()
                        .find(|choice| choice.platform.as_deref() == Some(platform))
                        .or_else(|| choices.iter().find(|choice| choice.platform.is_none()));
                    let Some(selected) = selected else { continue };
                    entry.package.clone_from(&selected.link);
                    if selected.sha256.is_some() {
                        entry.sha256.clone_from(&selected.sha256);
                    }
                }
                None => {}
            }
            plugins.push(entry);
        }
        Ok(Index {
            schema: self.schema,
            name: self.name,
            plugins,
        })
    }
}

impl TryFrom<Catalog> for Index {
    type Error = PluginError;

    fn try_from(catalog: Catalog) -> Result<Self, Self::Error> {
        catalog.resolve(&crate::native::platform())
    }
}

impl IndexSource {
    /// Whether this source is the permanent default, including equivalent URL spellings.
    pub fn is_default(&self) -> bool {
        is_default_url(&self.url)
    }
    /// Downloads an index, verifying its signature against the user's pinned key.
    pub fn load(&self) -> Result<Index, PluginError> {
        let bytes = fetch(&self.url)?;
        let signature = if self.key.is_some() {
            Some(fetch(&format!("{}.minisig", self.url))?)
        } else {
            None
        };
        parse_index(&bytes, self.key.as_deref(), signature.as_deref())
    }
}
/// Parses a bounded index and verifies a detached signature when a key is pinned.
pub fn parse_index(
    bytes: &[u8],
    key: Option<&str>,
    signature: Option<&[u8]>,
) -> Result<Index, PluginError> {
    if bytes.len() > 1024 * 1024 {
        return Err(error("index exceeds 1 MiB"));
    }
    if let Some(key) = key {
        let signature = signature.ok_or_else(|| error("signed index has no signature"))?;
        let signature =
            minisign_verify::Signature::decode(std::str::from_utf8(signature).map_err(error)?)
                .map_err(error)?;
        let key = minisign_verify::PublicKey::from_base64(key).map_err(error)?;
        key.verify(bytes, &signature, false).map_err(error)?;
    }
    let text = std::str::from_utf8(bytes).map_err(error)?;
    let index: Index = if text.trim_start().starts_with('{') {
        serde_json::from_str(text).map_err(error)?
    } else {
        toml::from_str(text).map_err(error)?
    };
    if index.schema != 1 || index.name.trim().is_empty() {
        return Err(error("unsupported or unnamed index"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for entry in &index.plugins {
        if !ids.insert(&entry.id)
            || entry.sha256.as_ref().is_some_and(|hash| {
                hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || !url::Url::parse(&entry.package).is_ok_and(|url| distribution_url(&url))
        {
            return Err(error("invalid or duplicate index entry"));
        }
        semver::Version::parse(&entry.version).map_err(error)?;
        if let Some(minimum) = &entry.min_hydra {
            semver::Version::parse(minimum).map_err(error)?;
        }
    }
    Ok(index)
}
fn is_default_url(address: &str) -> bool {
    url::Url::parse(address)
        .is_ok_and(|url| url.as_str() == DEFAULT_INDEX_URL || url.as_str() == default_catalog_url())
}

fn mirrors(root: &Path) -> Result<Vec<IndexSource>, PluginError> {
    match std::fs::read(root.join("indexes.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(error),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(error(e)),
    }
}

/// Returns the permanent default catalog followed by the user's mirrors.
pub fn sources(root: &Path) -> Result<Vec<IndexSource>, PluginError> {
    let mut list = vec![IndexSource {
        url: default_catalog_url().into(),
        key: None,
    }];
    list.extend(
        mirrors(root)?
            .into_iter()
            .filter(|source| !source.is_default()),
    );
    Ok(list)
}
/// Adds a verified index, or replaces its explicitly supplied verification key.
pub fn add(root: &Path, source: IndexSource) -> Result<Index, PluginError> {
    if source.is_default() {
        return Err(error("the default plugin catalog is read-only"));
    }
    let index = source.load()?;
    let mut list = mirrors(root)?;
    list.retain(|s| !s.is_default() && s.url != source.url);
    list.push(source);
    crate::manager::write_atomic(
        &root.join("indexes.json"),
        &serde_json::to_vec(&list).map_err(error)?,
    )?;
    Ok(index)
}
/// Removes an index source without changing any installed plugin.
pub fn remove(root: &Path, url: &str) -> Result<(), PluginError> {
    if is_default_url(url) {
        return Err(error("the default plugin catalog cannot be removed"));
    }
    let mut list = mirrors(root)?;
    list.retain(|s| !s.is_default() && s.url != url);
    crate::manager::write_atomic(
        &root.join("indexes.json"),
        &serde_json::to_vec(&list).map_err(error)?,
    )
}

impl Entry {
    /// Fetches exactly the advertised package, including its publisher and version.
    pub fn prepare(&self) -> Result<Prepared, PluginError> {
        compatible(self.api, self.min_hydra.as_deref())?;
        let prepared = Prepared::new(&self.package, self.sha256.as_deref())?;
        let package = crate::manager::Manager::inspect(&prepared.path)?;
        if package.manifest.id != self.id
            || package.manifest.version != self.version
            || package.manifest.api != self.api
            || self
                .publisher_key
                .as_ref()
                .is_some_and(|key| package.manifest.publisher_key.as_ref() != Some(key))
        {
            return Err(error(
                "package identity or publisher differs from its index entry",
            ));
        }
        Ok(prepared)
    }
}
/// Checks configured catalogs for newer compatible versions, without installing them.
pub fn updates(
    root: &Path,
    installed: &[crate::manager::Installed],
    id: Option<&str>,
) -> Result<Vec<Entry>, PluginError> {
    if installed.is_empty() {
        return Ok(Vec::new());
    }
    let mut catalogs = Vec::new();
    for source in sources(root)? {
        catalogs.push((source.is_default(), source.load()?));
    }
    select_updates(catalogs, installed, id)
}

fn select_updates(
    catalogs: Vec<(bool, Index)>,
    installed: &[crate::manager::Installed],
    id: Option<&str>,
) -> Result<Vec<Entry>, PluginError> {
    let mut selected: std::collections::BTreeMap<String, (bool, Entry)> = Default::default();
    for (default, catalog) in catalogs {
        for entry in catalog.plugins {
            if !default && compatible(entry.api, entry.min_hydra.as_deref()).is_err() {
                continue;
            }
            let replace = match selected.get(&entry.id) {
                None => true,
                Some((previous_default, previous)) => {
                    default
                        || (!previous_default
                            && semver::Version::parse(&entry.version).map_err(error)?
                                > semver::Version::parse(&previous.version).map_err(error)?)
                }
            };
            if replace {
                selected.insert(entry.id.clone(), (default, entry));
            }
        }
    }
    let mut updates = Vec::new();
    for (_, entry) in selected.into_values() {
        if compatible(entry.api, entry.min_hydra.as_deref()).is_err()
            || id.is_some_and(|id| id != entry.id)
        {
            continue;
        }
        if let Some(previous) = installed.iter().find(|p| p.manifest.id == entry.id) {
            if semver::Version::parse(&entry.version).map_err(error)?
                > semver::Version::parse(&previous.manifest.version).map_err(error)?
            {
                updates.push(entry);
            }
        }
    }
    Ok(updates)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_refuses_missing_pinned_signature_bad_hash_and_duplicate_ids() {
        let text = format!("schema=1\nname='Test'\n[[plugins]]\nid='a.b'\nname='Example'\nversion='1.2.0'\npackage='https://example.com/plugin.hyaplugin'\nsha256='{}'\napi=1\n", "a".repeat(64));
        assert!(parse_index(text.as_bytes(), None, None).is_ok());
        assert!(parse_index(text.as_bytes(), Some("pinned"), None).is_err());
        assert!(parse_index(text.replace(&"a".repeat(64), "bad").as_bytes(), None, None).is_err());
        let duplicate = format!("{text}{}", &text[text.find("[[plugins]]").unwrap()..]);
        assert!(parse_index(duplicate.as_bytes(), None, None).is_err());
        assert!(Prepared::new("http://example.com/plugin.hyaplugin", None).is_err());
    }
    #[test]
    fn signed_index_authenticates_content_key_and_trusted_comment() {
        let data = include_bytes!("../tests/fixtures/index.toml");
        let key = include_str!("../tests/fixtures/index.pub").trim();
        let signature = include_bytes!("../tests/fixtures/index.minisig");
        let index = parse_index(data, Some(key), Some(signature)).unwrap();
        assert_eq!(index.plugins[0].id, "example.direct");
        let mut changed = data.to_vec();
        changed.extend_from_slice(b"\n#tampered");
        assert!(parse_index(&changed, Some(key), Some(signature)).is_err());
        let comment = String::from_utf8_lossy(signature).replace("Hydra test fixture", "different");
        assert!(parse_index(data, Some(key), Some(comment.as_bytes())).is_err());
        assert!(parse_index(data, Some("invalid"), Some(signature)).is_err());
        assert!(parse_index(data, Some(key), Some(b"invalid")).is_err());
    }
    #[test]
    fn catalogs_bound_input_and_enforce_host_compatibility() {
        assert!(parse_index(&vec![b'x'; 1024 * 1024 + 1], None, None).is_err());
        assert!(parse_index(b"schema=2\nname='Future'", None, None).is_err());
        assert!(parse_index(b"schema=1\nname=' '", None, None).is_err());
        assert!(parse_index(b"invalid", None, None).is_err());
        assert!(compatible(1, Some(env!("HYDRA_PRODUCT_VERSION"))).is_ok());
        assert!(compatible(1, None).is_ok());
        assert!(compatible(2, None).is_err());
        assert!(compatible(1, Some("999.0.0")).is_err());
        assert!(compatible(1, Some("broken")).is_err());
        for url in [
            "http://example.com/plugin",
            "https://user:pass@example.com/plugin",
            "file:///tmp/plugin",
            "not a URL",
            "https://127.0.0.1/plugin",
        ] {
            assert!(fetch(url).is_err());
        }
    }
    #[test]
    fn index_source_removal_preserves_other_sources_and_rejects_corrupt_state() {
        let root = tempfile::tempdir().unwrap();
        let defaults = sources(root.path()).unwrap();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].url, DEFAULT_INDEX_URL);
        assert!(defaults[0].is_default());
        let list = vec![
            IndexSource {
                url: "https://example.com/a".into(),
                key: Some("pin".into()),
            },
            IndexSource {
                url: "https://example.com/b".into(),
                key: None,
            },
        ];
        std::fs::write(
            root.path().join("indexes.json"),
            serde_json::to_vec(&list).unwrap(),
        )
        .unwrap();
        remove(root.path(), "https://example.com/a").unwrap();
        let left = sources(root.path()).unwrap();
        assert_eq!(left.len(), 2);
        assert!(left[0].is_default());
        assert_eq!(left[1].url, list[1].url);
        std::fs::write(root.path().join("indexes.json"), b"bad").unwrap();
        assert!(sources(root.path()).is_err());
    }
    #[test]
    #[cfg(debug_assertions)]
    fn debug_catalogs_are_limited_to_explicit_http_loopback_origins() {
        for url in [
            "http://localhost:8000/plugins.json",
            "http://127.0.0.1:8000/plugins.json",
        ] {
            assert!(debug_catalog_url(url).is_ok(), "{url}");
        }
        for url in [
            "bad",
            "http://example.com/plugins.json",
            "http://192.168.1.2/plugins.json",
            "http://[::1]:8000/plugins.json",
            "https://localhost/plugins.json",
            "file:///tmp/plugins.json",
            "http://user:pass@localhost/plugins.json",
            "http://localhost/plugins.json#fragment",
        ] {
            assert!(debug_catalog_url(url).is_err(), "{url}");
        }
        assert!(!debug_distribution_url(
            &url::Url::parse("http://127.0.0.1/plugin.hyaplugin").unwrap()
        ));
    }

    #[test]
    fn platform_downloads_select_matching_package_and_checksum() {
        for platform in [
            "linux-aarch64",
            "linux-x86_64",
            "macos-aarch64",
            "macos-x86_64",
            "windows-aarch64",
            "windows-x86_64",
        ] {
            let catalog: Catalog =
                serde_json::from_slice(include_bytes!("../../../docs/plugins.json")).unwrap();
            let index = catalog.resolve(platform).unwrap();
            let torrent = index
                .plugins
                .iter()
                .find(|entry| entry.id == "hydra.torrent")
                .unwrap();
            let versioned = format!(
                "torrent-download_{}_{}.hyaplugin",
                torrent.version,
                platform.replace('-', "_")
            );
            let unversioned = format!("torrent-download-{platform}.hyaplugin");
            assert!(
                torrent.package.ends_with(&versioned) || torrent.package.ends_with(&unversioned),
                "{}",
                torrent.package
            );
            let source: serde_json::Value =
                serde_json::from_slice(include_bytes!("../../../docs/plugins.json")).unwrap();
            let choice = source["plugins"][1]["download"]
                .as_array()
                .unwrap()
                .iter()
                .find(|choice| choice["platform"] == platform)
                .unwrap();
            assert_eq!(torrent.sha256.as_deref(), choice["sha256"].as_str());
        }
    }

    #[test]
    fn unsupported_platform_skips_native_plugin_and_keeps_universal_download() {
        let catalog: Catalog =
            serde_json::from_slice(include_bytes!("../../../docs/plugins.json")).unwrap();
        let index = catalog.resolve("freebsd-x86_64").unwrap();
        assert_eq!(index.plugins.len(), 1);
        assert_eq!(index.plugins[0].id, "hydra.youtube");
    }

    #[test]
    fn malformed_platform_downloads_are_rejected_even_on_another_platform() {
        for download in serde_json::json!([
            [], [{"caption": "", "link": "https://example.com/p.hyaplugin"}],
            [{"caption": "Linux", "link": "http://example.com/p.hyaplugin"}],
            [{"caption": "Linux", "link": "https://example.com/p.exe"}],
            [{"caption": "Linux", "link": "https://example.com/p.hyaplugin#fragment"}],
            [{"caption": "Linux", "link": "https://example.com/p.hyaplugin", "sha256": "bad"}],
            [{"caption": "Linux", "link": "https://example.com/p.hyaplugin", "platform": ""}]
        ])
        .as_array()
        .unwrap()
        {
            let mut source: serde_json::Value =
                serde_json::from_slice(include_bytes!("../../../docs/plugins.json")).unwrap();
            source["plugins"][1]["download"] = download.clone();
            let catalog: Catalog = serde_json::from_value(source).unwrap();
            assert!(catalog.resolve("freebsd-x86_64").is_err());
        }
    }

    fn catalog(id: &str, version: &str) -> Index {
        parse_index(
            serde_json::to_vec(&serde_json::json!({
                "schema": 1, "name": "Test", "plugins": [{
                    "id": id, "name": id, "version": version,
                    "download": "https://example.com/plugin.hyaplugin", "api": 1
                }]
            }))
            .unwrap()
            .as_slice(),
            None,
            None,
        )
        .unwrap()
    }

    fn installed(id: &str, version: &str) -> crate::manager::Installed {
        serde_json::from_value(serde_json::json!({
            "manifest": {"id": id, "name": id, "version": version, "api": 1, "module": "plugin.wasm"},
            "directory": "plugins/test", "dev": false, "enabled": true, "grants": {}, "pins": {}
        })).unwrap()
    }

    #[test]
    fn json_indexes_accept_optional_pins_and_description_alias() {
        let mut index = catalog("community.video", "1.0.0");
        assert!(index.plugins[0].sha256.is_none());
        assert!(index.plugins[0].publisher_key.is_none());
        assert!(!index.plugins[0].is_official);
        index.plugins[0].sha256 = Some("a".repeat(64));
        index.plugins[0].publisher_key = Some("publisher".into());
        let bytes = serde_json::to_vec(&index).unwrap();
        let roundtrip = parse_index(&bytes, None, None).unwrap();
        assert_eq!(roundtrip.plugins[0].sha256, index.plugins[0].sha256);
        assert!(parse_index(b"{broken", None, None).is_err());
        index.plugins[0].sha256 = Some("bad".into());
        assert!(parse_index(&serde_json::to_vec(&index).unwrap(), None, None).is_err());
    }

    #[test]
    fn default_catalog_is_read_only_and_cannot_be_shadowed_in_saved_sources() {
        let root = tempfile::tempdir().unwrap();
        let default = IndexSource {
            url: DEFAULT_INDEX_URL.into(),
            key: Some("other".into()),
        };
        assert!(add(root.path(), default.clone()).is_err());
        assert!(!root.path().join("indexes.json").exists());
        for url in [
            DEFAULT_INDEX_URL,
            "https://HYDRA.JAVAD.DEV:443/plugins.json",
        ] {
            assert!(remove(root.path(), url).is_err());
        }
        std::fs::write(
            root.path().join("indexes.json"),
            serde_json::to_vec(&[default]).unwrap(),
        )
        .unwrap();
        let sources = sources(root.path()).unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].key.is_none());
        assert!(updates(root.path(), &[], None).unwrap().is_empty());
    }

    #[test]
    fn default_catalog_wins_duplicates_even_when_a_mirror_advertises_a_newer_version() {
        let installed = [installed("hydra.youtube", "1.0.0")];
        for default_first in [true, false] {
            let default = (true, catalog("hydra.youtube", "1.1.0"));
            let mirror = (false, catalog("hydra.youtube", "9.0.0"));
            let catalogs = if default_first {
                vec![default, mirror]
            } else {
                vec![mirror, default]
            };
            let updates = select_updates(catalogs, &installed, None).unwrap();
            assert_eq!(updates.len(), 1);
            assert_eq!(updates[0].version, "1.1.0");
        }
        let catalogs = vec![
            (true, catalog("hydra.youtube", "1.0.0")),
            (false, catalog("hydra.youtube", "9.0.0")),
        ];
        assert!(select_updates(catalogs, &installed, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn mirrors_supply_one_newest_compatible_update_for_plugins_missing_from_default() {
        let previous = [
            installed("community.video", "1.0.0"),
            installed("community.audio", "1.0.0"),
        ];
        let catalogs = vec![
            (true, catalog("hydra.youtube", "1.0.0")),
            (false, catalog("community.video", "2.0.0")),
            (false, catalog("community.video", "1.1.0")),
            (false, catalog("community.video", "3.0.0")),
            (false, catalog("community.audio", "1.2.0")),
        ];
        let updates = select_updates(catalogs.clone(), &previous, Some("community.video")).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].version, "3.0.0");
        assert_eq!(select_updates(catalogs, &previous, None).unwrap().len(), 2);
        let mut future = catalog("community.video", "9.0.0");
        future.plugins[0].min_hydra = Some("999.0.0".into());
        let updates = select_updates(
            vec![
                (false, future.clone()),
                (false, catalog("community.video", "1.1.0")),
            ],
            &previous,
            None,
        )
        .unwrap();
        assert_eq!(updates[0].version, "1.1.0");
        assert!(select_updates(
            vec![(true, future), (false, catalog("community.video", "1.1.0"))],
            &previous,
            None
        )
        .unwrap()
        .is_empty());
        assert!(select_updates(
            vec![(false, catalog("community.video", "1.0.0"))],
            &previous,
            None
        )
        .unwrap()
        .is_empty());
        assert!(select_updates(
            vec![(true, catalog("hydra.youtube", "2.0.0"))],
            &previous,
            None
        )
        .unwrap()
        .is_empty());
    }
}
