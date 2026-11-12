//! Turns yt-dlp's stderr into an error the user can act on.
use hya_plugin_sdk::{ErrorCode, PluginError};

use crate::env::Env;
use crate::invocation::Target;

struct Rule {
    /// Every needle must appear, compared case-insensitively.
    needles: &'static [&'static str],
    code: ErrorCode,
    advice: &'static str,
}

const BOT_CHECK_WITH_COOKIES: &str = "YouTube refused this connection with bot verification even with your browser cookies. Sign in to youtube.com in that browser and add its cookies again, or use another network. Sessions that still fail need a PO token passed through 'YouTube extractor arguments'.";

/// Ordered: a missing runtime also produces later, misleading errors.
const RULES: &[Rule] = &[
    Rule {
        needles: &["challenge solving failed"],
        code: ErrorCode::ToolFailed,
        advice: "yt-dlp could not solve YouTube's player challenge because it found no usable JavaScript runtime. Install Deno or Node.js (22 or newer), then pick it under 'JavaScript runtime for yt-dlp' in this plugin's settings; if it lives outside the usual folders, also set 'JavaScript runtime folder'. A pip-installed yt-dlp additionally needs `pip install -U \"yt-dlp[default]\"`.",
    },
    Rule {
        needles: &["no supported javascript runtime"],
        code: ErrorCode::ToolFailed,
        advice: "yt-dlp found no supported JavaScript runtime. Install Deno or Node.js (22 or newer), or point 'JavaScript runtime folder' in this plugin's settings at the one you have.",
    },
    Rule {
        needles: &["sign in to confirm", "not a bot"],
        code: ErrorCode::Network,
        advice: "YouTube refused this connection with bot verification. Enable 'Use granted browser cookies' in this plugin's settings and add cookies from a browser that can play the video, or use another network. Sessions that still fail need a PO token passed through 'YouTube extractor arguments'.",
    },
    Rule {
        needles: &["sign in to confirm your age"],
        code: ErrorCode::PermissionDenied,
        advice: "This video is age-restricted. Enable 'Use granted browser cookies' and add cookies from a browser signed in to an account that may watch it.",
    },
    Rule {
        needles: &["private video"],
        code: ErrorCode::PermissionDenied,
        advice: "This video is private. Enable 'Use granted browser cookies' and add cookies from an account that has access to it.",
    },
    Rule {
        needles: &["members-only"],
        code: ErrorCode::PermissionDenied,
        advice: "This video is for channel members. Enable 'Use granted browser cookies' and add cookies from a member account.",
    },
    Rule {
        needles: &["available in your country"],
        code: ErrorCode::Network,
        advice: "YouTube does not offer this video in your region.",
    },
    Rule {
        needles: &["http error 429"],
        code: ErrorCode::Network,
        advice: "YouTube is rate-limiting this connection. Wait a while before trying again.",
    },
    Rule {
        needles: &["premieres in"],
        code: ErrorCode::Unsupported,
        advice: "This premiere has not started yet.",
    },
    Rule {
        needles: &["live event will begin"],
        code: ErrorCode::Unsupported,
        advice: "This live stream has not started yet.",
    },
    Rule {
        needles: &["video unavailable"],
        code: ErrorCode::Unsupported,
        advice: "YouTube reports this video as unavailable; it may have been removed.",
    },
    Rule {
        needles: &["unable to extract"],
        code: ErrorCode::ToolFailed,
        advice: "yt-dlp could not read YouTube's response, which usually means it is out of date. Update yt-dlp, then approve the new program in this plugin's permissions.",
    },
];

/// Rewrites a failed extraction, keeping yt-dlp's own text in the plugin log.
pub fn explain(
    env: &impl Env,
    error: PluginError,
    target: Target,
    cookies_sent: bool,
) -> PluginError {
    if error.code != ErrorCode::ToolFailed {
        return error;
    }
    // YouTube words this with a typographic apostrophe.
    let haystack = error.message.to_lowercase().replace('’', "'");
    let Some(rule) = RULES
        .iter()
        .find(|rule| rule.needles.iter().all(|needle| haystack.contains(needle)))
    else {
        return error;
    };
    env.log("warn", &error.message);
    let advice = if cookies_sent && rule.needles.contains(&"not a bot") {
        BOT_CHECK_WITH_COOKIES
    } else {
        rule.advice
    };
    let subject = match target {
        Target::Video => "Video",
        Target::Playlist => "Playlist",
    };
    PluginError::new(
        rule.code,
        format!("{subject}: {advice} See the plugin log for yt-dlp's own message."),
    )
}

/// Records what yt-dlp warned about during an extraction that succeeded.
pub fn log_warnings(env: &impl Env, stderr: &str) {
    for line in stderr.lines().filter(|line| line.starts_with("WARNING:")) {
        env.log("warn", line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::fake::FakeEnv;

    fn explained(message: &str, target: Target) -> (PluginError, FakeEnv) {
        let env = FakeEnv::default();
        let error = explain(
            &env,
            PluginError::new(ErrorCode::ToolFailed, message),
            target,
            false,
        );
        (error, env)
    }

    #[test]
    fn bot_verification_after_a_metadata_warning_points_at_cookies() {
        let message = "exited with 1: WARNING: [youtube] No title found in player responses; falling back to title from initial data.\nERROR: [youtube] 9KkbfDVQYO0: Sign in to confirm you’re not a bot. This helps protect our community.";
        let (video, env) = explained(message, Target::Video);
        assert_eq!(video.code, ErrorCode::Network);
        assert!(video.message.starts_with("Video: "));
        assert!(video.message.contains("Use granted browser cookies"));
        assert!(env.logged("warn", "9KkbfDVQYO0"));
        let (playlist, _) = explained(message, Target::Playlist);
        assert!(playlist.message.starts_with("Playlist: "));
    }

    #[test]
    fn bot_verification_despite_cookies_points_at_signing_in() {
        let env = FakeEnv::default();
        let error = PluginError::new(
            ErrorCode::ToolFailed,
            "ERROR: [youtube] x: Sign in to confirm you're not a bot.",
        );
        let shown = explain(&env, error, Target::Video, true);
        assert_eq!(shown.code, ErrorCode::Network);
        assert!(shown.message.contains("even with your browser cookies"));
        assert!(shown.message.contains("Sign in to youtube.com"));
        assert!(!shown
            .message
            .contains("Enable 'Use granted browser cookies'"));
    }

    #[test]
    fn a_missing_runtime_is_named_instead_of_the_page_reload_error_it_causes() {
        let (shown, _) = explained(
            "WARNING: [youtube] RJ0VQMoZc8k: n challenge solving failed: Some formats may be missing. Ensure you have a supported JavaScript runtime\nERROR: [youtube] RJ0VQMoZc8k: The page needs to be reloaded.",
            Target::Video,
        );
        assert_eq!(shown.code, ErrorCode::ToolFailed);
        assert!(shown.message.contains("Deno or Node.js"));
        assert!(shown.message.contains("JavaScript runtime folder"));
        assert!(!shown.message.contains("page needs to be reloaded"));
    }

    #[test]
    fn each_known_refusal_gets_its_own_code_and_advice() {
        for (message, code, advice) in [
            (
                "ERROR: Sign in to confirm your age. This video may be inappropriate",
                ErrorCode::PermissionDenied,
                "age-restricted",
            ),
            (
                "ERROR: [youtube] x: Private video. Sign in if you've been granted access",
                ErrorCode::PermissionDenied,
                "private",
            ),
            (
                "ERROR: Join this channel to get access to members-only content",
                ErrorCode::PermissionDenied,
                "channel members",
            ),
            (
                "ERROR: The uploader has not made this video available in your country",
                ErrorCode::Network,
                "region",
            ),
            (
                "ERROR: Unable to download API page: HTTP Error 429: Too Many Requests",
                ErrorCode::Network,
                "rate-limiting",
            ),
            (
                "ERROR: [youtube] x: Premieres in 3 hours",
                ErrorCode::Unsupported,
                "premiere",
            ),
            (
                "ERROR: [youtube] x: This live event will begin in 2 days.",
                ErrorCode::Unsupported,
                "live stream",
            ),
            (
                "ERROR: [youtube] x: Video unavailable. This video has been removed",
                ErrorCode::Unsupported,
                "unavailable",
            ),
            (
                "ERROR: [youtube] x: Unable to extract initial player response; please report this issue",
                ErrorCode::ToolFailed,
                "out of date",
            ),
            (
                "ERROR: No supported JavaScript runtime could be found",
                ErrorCode::ToolFailed,
                "JavaScript runtime folder",
            ),
        ] {
            let (shown, _) = explained(message, Target::Video);
            assert_eq!(shown.code, code, "{message}");
            assert!(shown.message.contains(advice), "{message}: {}", shown.message);
        }
    }

    #[test]
    fn unrecognized_and_non_tool_errors_are_left_exactly_as_reported() {
        for (code, message) in [
            (
                ErrorCode::ToolFailed,
                "WARNING: No title found in player responses",
            ),
            (ErrorCode::ToolFailed, "ERROR: something new"),
            (
                ErrorCode::PermissionDenied,
                "Sign in to confirm you're not a bot",
            ),
            (ErrorCode::ToolMissing, "yt-dlp is missing"),
            (ErrorCode::Deadline, "the call ran past its deadline"),
        ] {
            let env = FakeEnv::default();
            let error = PluginError::new(code, message);
            assert_eq!(explain(&env, error.clone(), Target::Video, false), error);
            assert!(env.logs.borrow().is_empty());
        }
    }

    #[test]
    fn only_warning_lines_of_a_successful_run_reach_the_log() {
        let env = FakeEnv::default();
        log_warnings(
            &env,
            "[youtube] x: Downloading webpage\nWARNING: [youtube] x: Some web client https formats have been skipped\n[info] done",
        );
        assert_eq!(env.logs.borrow().len(), 1);
        assert!(env.logged("warn", "formats have been skipped"));
    }
}
