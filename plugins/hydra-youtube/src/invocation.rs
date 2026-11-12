//! Builds the yt-dlp command line from the plugin settings.
//!
//! Every shape produced here must be declared as an `exec` template in
//! `hydra-plugin.toml`; the manifest test at the bottom enforces that.
use std::collections::BTreeMap;

use hya_plugin_sdk::{ErrorCode, PluginError, Result, Value};

pub const PLAYLIST_LIMIT: &str = "512";
const RUNTIMES: [&str; 4] = ["deno", "node", "quickjs", "bun"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Video,
    Playlist,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Fixed(&'static str),
    /// User-controlled; the manifest template admits it as `*`.
    Value(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Options {
    /// `None` leaves the choice to yt-dlp among Deno and Node.
    pub runtime: Option<String>,
    pub extractor_args: Option<String>,
    pub cookies: bool,
}

fn text<'a>(settings: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a str> {
    settings
        .get(key)
        .and_then(Value::as_text)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::new(ErrorCode::InvalidInput, message)
}

impl Options {
    /// # Errors
    /// `invalid_input` when a setting cannot be turned into a safe argument.
    pub fn from_settings(settings: &BTreeMap<String, Value>) -> Result<Self> {
        let choice = text(settings, "javascript_runtime").filter(|c| *c != "default");
        let path = text(settings, "javascript_runtime_path");
        let runtime = match (choice, path) {
            (Some(name), _) if !RUNTIMES.contains(&name) => {
                return Err(invalid(format!("unknown JavaScript runtime `{name}`")));
            }
            (Some(name), Some(path)) => Some(format!("{name}:{path}")),
            (Some(name), None) => Some(name.to_owned()),
            (None, Some(_)) => {
                return Err(invalid(
                    "Pick the runtime under 'JavaScript runtime for yt-dlp' that the runtime folder belongs to; 'default' cannot use a custom folder.",
                ));
            }
            (None, None) => None,
        };
        let extractor_args = text(settings, "extractor_args")
            .map(|raw| raw.strip_prefix("youtube:").unwrap_or(raw))
            .map(|args| {
                if args.chars().any(char::is_control) {
                    return Err(invalid("YouTube extractor arguments must be one line"));
                }
                Ok(format!("youtube:{args}"))
            })
            .transpose()?;
        Ok(Self {
            runtime,
            extractor_args,
            cookies: settings
                .get("use_cookies")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

pub fn version() -> Vec<Arg> {
    ["--ignore-config", "--no-plugin-dirs", "--version"]
        .map(Arg::Fixed)
        .to_vec()
}

pub fn extraction(target: Target, options: &Options, url: &str) -> Vec<Arg> {
    let mut args = vec![
        Arg::Fixed("--ignore-config"),
        Arg::Fixed("--no-plugin-dirs"),
    ];
    match target {
        Target::Video => args.extend(["--no-playlist", "-J", "--skip-download"].map(Arg::Fixed)),
        Target::Playlist => args.extend(
            [
                "--yes-playlist",
                "-J",
                "--skip-download",
                "--flat-playlist",
                "--playlist-end",
                PLAYLIST_LIMIT,
            ]
            .map(Arg::Fixed),
        ),
    }
    match &options.runtime {
        // yt-dlp enables only Deno by default and `--js-runtimes` adds to that
        // set, so this keeps Deno's priority and lets Node stand in for it.
        None => args.extend([Arg::Fixed("--js-runtimes"), Arg::Value("node".into())]),
        // An enabled Deno outranks every other runtime; clearing the set is the
        // only way an explicit choice is honoured.
        Some(runtime) => args.extend([
            Arg::Fixed("--no-js-runtimes"),
            Arg::Fixed("--js-runtimes"),
            Arg::Value(runtime.clone()),
        ]),
    }
    if let Some(extractor_args) = &options.extractor_args {
        args.extend([
            Arg::Fixed("--extractor-args"),
            Arg::Value(extractor_args.clone()),
        ]);
    }
    if options.cookies {
        args.extend([Arg::Fixed("--cookies"), Arg::Fixed("{cookie_file}")]);
    }
    args.extend([Arg::Fixed("--"), Arg::Value(url.to_owned())]);
    args
}

pub fn command_line(args: &[Arg]) -> Vec<String> {
    args.iter()
        .map(|arg| match arg {
            Arg::Fixed(literal) => (*literal).to_owned(),
            Arg::Value(value) => value.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn text_value(s: &str) -> Value {
        Value::Text(s.into())
    }

    fn line(target: Target, options: &Options) -> String {
        command_line(&extraction(target, options, "https://youtu.be/x")).join(" ")
    }

    #[test]
    fn default_settings_keep_deno_first_and_add_node() {
        let options = Options::from_settings(&settings(&[])).unwrap();
        assert_eq!(options, Options::default());
        assert_eq!(
            line(Target::Video, &options),
            "--ignore-config --no-plugin-dirs --no-playlist -J --skip-download --js-runtimes node -- https://youtu.be/x"
        );
        let explicit_default =
            Options::from_settings(&settings(&[("javascript_runtime", text_value("default"))]))
                .unwrap();
        assert_eq!(explicit_default, options);
    }

    #[test]
    fn an_explicit_runtime_clears_yt_dlp_defaults_so_deno_cannot_outrank_it() {
        let options =
            Options::from_settings(&settings(&[("javascript_runtime", text_value("node"))]))
                .unwrap();
        let line = line(Target::Video, &options);
        assert!(line.contains("--no-js-runtimes --js-runtimes node --"));
    }

    #[test]
    fn a_runtime_folder_is_attached_to_the_chosen_runtime_only() {
        let with_path = Options::from_settings(&settings(&[
            ("javascript_runtime", text_value("deno")),
            (
                "javascript_runtime_path",
                text_value(" /Users/me/.deno/bin "),
            ),
        ]))
        .unwrap();
        assert_eq!(
            with_path.runtime.as_deref(),
            Some("deno:/Users/me/.deno/bin")
        );
        let orphan = Options::from_settings(&settings(&[(
            "javascript_runtime_path",
            text_value("/opt/node/bin"),
        )]))
        .unwrap_err();
        assert_eq!(orphan.code, ErrorCode::InvalidInput);
        assert!(orphan.message.contains("Pick the runtime"));
        let unknown =
            Options::from_settings(&settings(&[("javascript_runtime", text_value("python"))]))
                .unwrap_err();
        assert_eq!(unknown.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn extractor_arguments_are_scoped_to_youtube_and_blank_means_none() {
        for (raw, expected) in [
            (
                "player_client=web_safari",
                "youtube:player_client=web_safari",
            ),
            (
                "youtube:po_token=web.gvs+abc",
                "youtube:po_token=web.gvs+abc",
            ),
            ("-x", "youtube:-x"),
        ] {
            let options =
                Options::from_settings(&settings(&[("extractor_args", text_value(raw))])).unwrap();
            assert_eq!(options.extractor_args.as_deref(), Some(expected));
        }
        let blank =
            Options::from_settings(&settings(&[("extractor_args", text_value("  "))])).unwrap();
        assert_eq!(blank.extractor_args, None);
        let multiline =
            Options::from_settings(&settings(&[("extractor_args", text_value("a\n--exec x"))]))
                .unwrap_err();
        assert_eq!(multiline.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn playlists_are_listed_flat_and_capped_and_cookies_come_last() {
        let options = Options {
            runtime: Some("bun".into()),
            extractor_args: Some("youtube:player_client=tv".into()),
            cookies: true,
        };
        assert_eq!(
            line(Target::Playlist, &options),
            "--ignore-config --no-plugin-dirs --yes-playlist -J --skip-download --flat-playlist --playlist-end 512 --no-js-runtimes --js-runtimes bun --extractor-args youtube:player_client=tv --cookies {cookie_file} -- https://youtu.be/x"
        );
    }

    fn template(args: &[Arg]) -> Vec<String> {
        args.iter()
            .map(|arg| match arg {
                Arg::Fixed(literal) => (*literal).to_owned(),
                Arg::Value(_) => "*".to_owned(),
            })
            .collect()
    }

    fn declared_templates() -> Vec<Vec<String>> {
        let manifest: toml::Value = include_str!("../hydra-plugin.toml").parse().unwrap();
        manifest["permissions"]["exec"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                assert_eq!(entry["program"].as_str(), Some("yt-dlp"));
                entry["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap().to_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_manifest_declares_exactly_the_command_lines_this_module_builds() {
        let mut built = vec![template(&version())];
        for target in [Target::Video, Target::Playlist] {
            for runtime in [None, Some("node".to_owned())] {
                for extractor_args in [None, Some("youtube:x".to_owned())] {
                    for cookies in [false, true] {
                        let options = Options {
                            runtime: runtime.clone(),
                            extractor_args: extractor_args.clone(),
                            cookies,
                        };
                        built.push(template(&extraction(target, &options, "u")));
                    }
                }
            }
        }
        let mut declared = declared_templates();
        built.sort();
        declared.sort();
        assert_eq!(declared, built);
    }

    #[test]
    fn no_built_value_can_be_read_as_an_option() {
        let options = Options::from_settings(&settings(&[
            ("javascript_runtime", text_value("node")),
            ("javascript_runtime_path", text_value("-rf")),
            ("extractor_args", text_value("--exec")),
        ]))
        .unwrap();
        let args = extraction(Target::Video, &options, "https://youtu.be/x");
        for arg in &args[..args.len() - 1] {
            if let Arg::Value(value) = arg {
                assert!(!value.starts_with('-'), "{value}");
            }
        }
    }
}
