//! Official resolver: yt-dlp extracts metadata; Hydra transfers the media.
mod backend;
mod diagnose;
mod env;
mod invocation;
mod plan;

use base64::Engine;
use hya_plugin_sdk::{ErrorCode, Host, Plan, Plugin, PluginError, Resolve, ResolveRequest};

use env::Env;
use invocation::{Options, Target};
pub use plan::plan;

#[derive(Default)]
pub struct Youtube;

impl Plugin for Youtube {
    fn check(&mut self, host: &Host) -> hya_plugin_sdk::Result<()> {
        backend::check(host)
    }

    fn resolve(&mut self, host: &Host, req: ResolveRequest) -> hya_plugin_sdk::Result<Resolve> {
        resolve(host, &req.url)
    }
}

/// `None` for an address that is not YouTube's.
fn target(address: &str) -> hya_plugin_sdk::Result<Option<Target>> {
    let url = url::Url::parse(address)
        .map_err(|e| PluginError::new(ErrorCode::InvalidInput, e.to_string()))?;
    let host = url.host_str().unwrap_or("");
    let youtube = host == "youtu.be" || host == "youtube.com" || host.ends_with(".youtube.com");
    if !matches!(url.scheme(), "http" | "https") || !youtube {
        return Ok(None);
    }
    Ok(Some(if url.path() == "/playlist" {
        Target::Playlist
    } else {
        Target::Video
    }))
}

fn extract(env: &impl Env, target: Target, url: &str) -> hya_plugin_sdk::Result<Plan> {
    let options = Options::from_settings(&env.settings()?)?;
    env.log(
        "debug",
        match target {
            Target::Playlist => "Reading playlist entries",
            Target::Video => "Reading available media formats",
        },
    );
    let output = backend::run(env, &invocation::extraction(target, &options, url))
        .and_then(|output| match output.status {
            0 => Ok(output),
            _ => Err(PluginError::new(ErrorCode::ToolFailed, output.stderr)),
        })
        .map_err(|error| diagnose::explain(env, error, target, options.cookies))?;
    diagnose::log_warnings(env, &output.stderr);
    let invalid =
        |e: &dyn std::fmt::Display| PluginError::new(ErrorCode::InvalidReply, e.to_string());
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(output.stdout_b64)
        .map_err(|e| invalid(&e))?;
    let info = serde_json::from_slice(&bytes).map_err(|e| invalid(&e))?;
    let plan = plan::plan(&info)?;
    env.log(
        "debug",
        &format!(
            "Resolved {} media tracks and {} playlist entries",
            plan.tracks.len(),
            plan.entries.len()
        ),
    );
    Ok(plan)
}

fn resolve(env: &impl Env, address: &str) -> hya_plugin_sdk::Result<Resolve> {
    match target(address)? {
        Some(target) => extract(env, target, address).map(Resolve::Plan),
        None => Ok(Resolve::NotClaimed),
    }
}

#[cfg(target_arch = "wasm32")]
hya_plugin_sdk::export!(Youtube);

#[cfg(test)]
mod tests {
    use hya_plugin_sdk::Value;

    use super::*;
    use crate::env::fake::{failure, output, FakeEnv};

    const VIDEO: &str = r#"{"id":"abc","title":"Clip","formats":[
        {"format_id":"137","url":"https://r.googlevideo.com/v","vcodec":"avc1","acodec":"none","height":1080},
        {"format_id":"140","url":"https://r.googlevideo.com/a","vcodec":"none","acodec":"mp4a","abr":128}]}"#;

    #[test]
    fn addresses_outside_youtube_are_declined_without_running_anything() {
        let env = FakeEnv::default();
        for address in [
            "https://example.com/watch?v=abc",
            "https://notyoutube.com/watch?v=abc",
            "ftp://www.youtube.com/watch?v=abc",
        ] {
            assert_eq!(resolve(&env, address).unwrap(), Resolve::NotClaimed);
        }
        assert_eq!(
            resolve(&env, "not a url").unwrap_err().code,
            ErrorCode::InvalidInput
        );
        assert!(env.executed.borrow().is_empty());
    }

    #[test]
    fn only_a_playlist_page_is_listed_as_a_playlist() {
        for (address, expected) in [
            (
                "https://www.youtube.com/playlist?list=PL1",
                Target::Playlist,
            ),
            (
                "https://www.youtube.com/watch?v=abc&list=PL1",
                Target::Video,
            ),
            ("https://youtu.be/abc", Target::Video),
            ("https://music.youtube.com/watch?v=abc", Target::Video),
            ("https://www.youtube.com/shorts/abc", Target::Video),
            ("https://www.youtube.com/live/abc", Target::Video),
        ] {
            assert_eq!(target(address).unwrap(), Some(expected), "{address}");
        }
    }

    #[test]
    fn a_video_resolves_in_a_single_backend_run() {
        let env = FakeEnv::with_exec([output(
            VIDEO,
            "WARNING: [youtube] abc: Some web client https formats have been skipped",
        )]);
        let Resolve::Plan(plan) = resolve(&env, "https://www.youtube.com/watch?v=abc").unwrap()
        else {
            panic!("expected a plan");
        };
        assert_eq!(plan.id, "abc");
        assert_eq!(plan.tracks.len(), 2);
        let executed = env.executed.borrow();
        assert_eq!(executed.len(), 1);
        assert!(executed[0].contains(&"--no-playlist".to_owned()));
        assert_eq!(
            executed[0].last().unwrap(),
            "https://www.youtube.com/watch?v=abc"
        );
        assert!(env.logged("warn", "formats have been skipped"));
        assert!(env.logged("debug", "Resolved 2 media tracks"));
    }

    #[test]
    fn settings_shape_the_playlist_command_line() {
        let env = FakeEnv::with_exec([output(
            r#"{"id":"PL1","title":"Mix","entries":[{"id":"one","title":"One"}]}"#,
            "",
        )])
        .set("use_cookies", Value::Bool(true))
        .set("javascript_runtime", Value::Text("node".into()))
        .set("extractor_args", Value::Text("player_client=tv".into()));
        let Resolve::Plan(plan) =
            resolve(&env, "https://www.youtube.com/playlist?list=PL1").unwrap()
        else {
            panic!("expected a plan");
        };
        assert_eq!(plan.entries.len(), 1);
        let line = env.executed.borrow()[0].join(" ");
        assert!(
            line.contains("--yes-playlist -J --skip-download --flat-playlist --playlist-end 512")
        );
        assert!(line.contains("--no-js-runtimes --js-runtimes node"));
        assert!(
            line.contains("--extractor-args youtube:player_client=tv --cookies {cookie_file} --")
        );
    }

    #[test]
    fn a_rejected_setting_stops_before_the_backend_runs() {
        let env =
            FakeEnv::default().set("javascript_runtime_path", Value::Text("/opt/node".into()));
        let error = resolve(&env, "https://youtu.be/abc").unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
        assert!(env.executed.borrow().is_empty());
    }

    #[test]
    fn a_failed_run_is_explained_whichever_way_the_host_reports_it() {
        let message = "ERROR: [youtube] abc: Sign in to confirm you’re not a bot.";
        let nonzero = Ok(hya_plugin_sdk::ExecOutput {
            status: 1,
            stdout_b64: String::new(),
            stderr: message.into(),
        });
        for reply in [nonzero, failure(ErrorCode::ToolFailed, message)] {
            let env = FakeEnv::with_exec([reply]);
            let error = resolve(&env, "https://youtu.be/abc").unwrap_err();
            assert_eq!(error.code, ErrorCode::Network);
            assert!(error.message.contains("bot verification"));
        }
    }

    #[test]
    fn output_that_is_not_metadata_is_an_invalid_reply() {
        for stdout in ["not json", "null"] {
            let env = FakeEnv::with_exec([output(stdout, "")]);
            let error = resolve(&env, "https://youtu.be/abc").unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidReply, "{stdout}");
        }
        let env = FakeEnv::with_exec([Ok(hya_plugin_sdk::ExecOutput {
            status: 0,
            stdout_b64: "%%%".into(),
            stderr: String::new(),
        })]);
        assert_eq!(
            resolve(&env, "https://youtu.be/abc").unwrap_err().code,
            ErrorCode::InvalidReply
        );
    }
}
