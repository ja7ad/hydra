//! Maps yt-dlp's `-J` metadata onto a Hydra download plan.
use std::collections::{BTreeMap, HashSet};

use hya_plugin_sdk::limits::MAX_TRACKS;
use hya_plugin_sdk::{
    Assemble, ErrorCode, Plan, PlaylistEntry, PluginError, Result, Track, TrackKind,
};
use serde_json::Value;

/// googlevideo throttles large single requests; yt-dlp chunks at the same size.
const CHUNK: u64 = 10 * 1024 * 1024;
const CONNECTIONS: u32 = 4;
const UNAVAILABLE_TITLES: [&str; 2] = ["[Private video]", "[Deleted video]"];

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn unsupported(message: &str) -> PluginError {
    PluginError::new(ErrorCode::Unsupported, message)
}

fn expiry(url: &str) -> Option<u64> {
    url::Url::parse(url)
        .ok()?
        .query_pairs()
        .find_map(|(k, v)| (k == "expire").then(|| v.parse().ok()).flatten())
}

fn headers(info: &Value, format: &Value) -> BTreeMap<String, String> {
    [info.get("http_headers"), format.get("http_headers")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .flatten()
        .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_owned())))
        .collect()
}

fn is_video_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn playlist(info: &Value, entries: &[Value]) -> Result<Plan> {
    let mut seen = HashSet::new();
    let entries: Vec<PlaylistEntry> = entries
        .iter()
        .filter_map(|entry| {
            let id = text(entry, "id").filter(|id| is_video_id(id))?;
            let title = text(entry, "title");
            if title.is_some_and(|t| UNAVAILABLE_TITLES.contains(&t)) || !seen.insert(id) {
                return None;
            }
            Some(PlaylistEntry {
                id: id.into(),
                url: format!("https://www.youtube.com/watch?v={id}"),
                title: title.map(str::to_owned),
            })
        })
        .take(MAX_TRACKS)
        .collect();
    if entries.is_empty() {
        return Err(unsupported("playlist has no available videos"));
    }
    Ok(Plan {
        id: text(info, "id").unwrap_or("playlist").into(),
        title: text(info, "title").map(str::to_owned),
        expires_at: None,
        assemble: Assemble::None,
        tracks: Vec::new(),
        transfer: None,
        entries,
    })
}

struct Media {
    track: Track,
    /// yt-dlp's ranking of the audio language: the original outranks dubs.
    language_rank: i64,
    compressed_range: bool,
}

fn media_track(info: &Value, format: &Value) -> Option<Media> {
    let (id, url) = (text(format, "format_id")?, text(format, "url")?);
    if format.get("has_drm").and_then(Value::as_bool) == Some(true)
        || !matches!(text(format, "protocol"), Some("https" | "http") | None)
    {
        return None;
    }
    let video = text(format, "vcodec").is_some_and(|v| v != "none");
    let audio = text(format, "acodec").is_some_and(|v| v != "none");
    if !video && !audio {
        return None;
    }
    let number = |key: &str| format.get(key).and_then(Value::as_f64);
    let mut track = Track::file(id, url);
    track.kind = if video {
        TrackKind::Video
    } else {
        TrackKind::Audio
    };
    track.default = false;
    track.container = text(format, "ext").map(str::to_owned);
    track.codec = text(format, if video { "vcodec" } else { "acodec" }).map(str::to_owned);
    track.height = format
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|n| n.try_into().ok());
    track.fps = number("fps").map(|n| n as f32);
    track.bitrate = number(if video { "vbr" } else { "abr" })
        .or_else(|| number("tbr"))
        .map(|kbps| (kbps * 1000.0) as u64);
    // `filesize_approx` is an estimate, and Hydra verifies a declared size.
    track.size = format.get("filesize").and_then(Value::as_u64);
    track.language = text(format, "language").map(str::to_owned);
    track.headers = headers(info, format);
    track.expires_at = expiry(url);
    track.chunk_hint = Some(CHUNK);
    track.sources[0].max_connections = Some(CONNECTIONS);
    Some(Media {
        track,
        language_rank: format
            .get("language_preference")
            .and_then(Value::as_i64)
            .unwrap_or(-1),
        compressed_range: id.ends_with("-drc")
            || text(format, "format_note").is_some_and(|note| note.contains("DRC")),
    })
}

/// Flags the audio a viewer hears by default, so "best audio" never lands on
/// a dubbed or dynamic-range-compressed variant of the same bitrate.
fn mark_original_audio(media: &mut [Media]) {
    let preferred = |m: &Media| m.track.kind == TrackKind::Audio && !m.compressed_range;
    let Some(best) = media
        .iter()
        .filter(|m| preferred(m))
        .map(|m| m.language_rank)
        .max()
    else {
        return;
    };
    for m in media.iter_mut() {
        m.track.default = preferred(m) && m.language_rank == best;
    }
}

fn subtitle_tracks(info: &Value, room: usize) -> Vec<Track> {
    let mut tracks: Vec<Track> = Vec::new();
    for (key, auto) in [("subtitles", false), ("automatic_captions", true)] {
        let Some(languages) = info.get(key).and_then(Value::as_object) else {
            continue;
        };
        for (language, formats) in languages {
            if auto
                && tracks
                    .iter()
                    .any(|t| t.language.as_deref() == Some(language))
            {
                continue;
            }
            let Some(format) = formats.as_array().and_then(|formats| {
                formats
                    .iter()
                    .filter(|f| text(f, "url").is_some())
                    .min_by_key(|f| match text(f, "ext") {
                        Some("vtt") => 0,
                        Some("srt") => 1,
                        _ => 2,
                    })
            }) else {
                continue;
            };
            let url = text(format, "url").unwrap_or_default();
            let mut track = Track::file(format!("{key}:{language}"), url);
            track.kind = TrackKind::Subtitle;
            track.language = Some(language.clone());
            track.auto_generated = auto;
            track.default = false;
            track.container = text(format, "ext").map(str::to_owned);
            track.headers = headers(info, format);
            track.expires_at = expiry(url);
            tracks.push(track);
        }
    }
    tracks.truncate(room);
    tracks
}

fn video(info: &Value) -> Result<Plan> {
    match text(info, "live_status") {
        Some("is_upcoming") => {
            return Err(unsupported("this premiere or live stream has not started"));
        }
        Some("is_live") => return Err(unsupported("live streams are not supported")),
        _ => {}
    }
    if info.get("is_live").and_then(Value::as_bool) == Some(true) {
        return Err(unsupported("live streams are not supported"));
    }
    let id = text(info, "id")
        .ok_or_else(|| PluginError::new(ErrorCode::InvalidReply, "metadata has no id"))?;
    let formats = info
        .get("formats")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut media: Vec<Media> = formats
        .iter()
        .filter_map(|format| media_track(info, format))
        .take(MAX_TRACKS)
        .collect();
    if media.is_empty() {
        let streaming_only = formats
            .iter()
            .any(|f| text(f, "protocol").is_some_and(|p| p.starts_with("m3u8")));
        return Err(unsupported(if streaming_only {
            "YouTube offered this session only segmented streams, which this plugin cannot download. Enable 'Use granted browser cookies', or choose other player clients in 'YouTube extractor arguments'."
        } else {
            "no direct HTTP media formats available"
        }));
    }
    mark_original_audio(&mut media);
    let mut tracks: Vec<Track> = media.into_iter().map(|m| m.track).collect();
    let room = MAX_TRACKS - tracks.len();
    tracks.extend(subtitle_tracks(info, room));
    Ok(Plan {
        id: id.into(),
        title: text(info, "title").map(str::to_owned),
        expires_at: tracks.iter().filter_map(|t| t.expires_at).min(),
        assemble: Assemble::Mux,
        transfer: None,
        entries: Vec::new(),
        tracks,
    })
}

/// Converts yt-dlp metadata into direct, independently selectable media tracks.
///
/// # Errors
/// `unsupported` when nothing downloadable is described, `invalid_reply` for
/// metadata without an id.
pub fn plan(info: &Value) -> Result<Plan> {
    match info.get("entries").and_then(Value::as_array) {
        Some(entries) => playlist(info, entries),
        None => video(info),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn track<'a>(plan: &'a Plan, id: &str) -> &'a Track {
        plan.tracks.iter().find(|t| t.id == id).expect(id)
    }

    #[test]
    fn playlist_keeps_order_and_drops_unavailable_duplicate_and_unsafe_entries() {
        let p = plan(&json!({"id":"PLtest","title":"Playlist","entries":[
            {"id":"video-one","title":"One"}, null, {"title":"No id"},
            {"id":"gone","title":"[Deleted video]"}, {"id":"hidden","title":"[Private video]"},
            {"id":"video-two","title":"Two"}, {"id":"video-one"}, {"id":"bad&url=evil"}
        ]}))
        .unwrap();
        let ids: Vec<_> = p.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["video-one", "video-two"]);
        assert!(p.tracks.is_empty());
        assert_eq!(p.assemble, Assemble::None);
        assert_eq!(
            p.entries[0].url,
            "https://www.youtube.com/watch?v=video-one"
        );
        assert_eq!(p.entries[1].title.as_deref(), Some("Two"));
    }

    #[test]
    fn a_playlist_with_nothing_playable_is_refused() {
        for entries in [json!([]), json!([{"id":"x","title":"[Private video]"}])] {
            let error = plan(&json!({"id":"PL","entries":entries})).unwrap_err();
            assert_eq!(error.code, ErrorCode::Unsupported);
        }
    }

    #[test]
    fn adaptive_formats_become_video_and_audio_tracks_with_transfer_hints() {
        let p = plan(&json!({"id":"v","title":"title",
            "http_headers":{"User-Agent":"UA","Accept":"*/*"},
            "formats":[
                {"format_id":"sb0","url":"https://i.ytimg.com/sb","vcodec":"none","acodec":"none","protocol":"mhtml"},
                {"format_id":"137","url":"https://r.googlevideo.com/a?expire=123","vcodec":"avc1.640028","acodec":"none","height":1080,"fps":30,"vbr":4400.5,"filesize":99,"ext":"mp4","protocol":"https","http_headers":{"User-Agent":"format UA"}},
                {"format_id":"140","url":"https://r.googlevideo.com/b?expire=456","vcodec":"none","acodec":"mp4a.40.2","abr":128,"ext":"m4a","filesize_approx":5000},
                {"format_id":"18","url":"https://r.googlevideo.com/c","vcodec":"avc1.42001E","acodec":"mp4a.40.2","height":360,"tbr":359.6,"ext":"mp4","protocol":"https"}
            ]}))
        .unwrap();
        assert_eq!(p.assemble, Assemble::Mux);
        assert_eq!(p.expires_at, Some(123));
        assert_eq!(p.tracks.len(), 3);
        let video = track(&p, "137");
        assert_eq!(video.kind, TrackKind::Video);
        assert_eq!((video.height, video.size), (Some(1080), Some(99)));
        assert_eq!(video.bitrate, Some(4_400_500));
        assert_eq!(video.headers["User-Agent"], "format UA");
        assert_eq!(video.headers["Accept"], "*/*");
        assert_eq!(video.chunk_hint, Some(CHUNK));
        assert_eq!(video.sources[0].max_connections, Some(CONNECTIONS));
        let audio = track(&p, "140");
        assert_eq!(audio.kind, TrackKind::Audio);
        assert_eq!(audio.bitrate, Some(128_000));
        assert_eq!(audio.size, None, "an estimated size must not be attested");
        let muxed = track(&p, "18");
        assert_eq!(muxed.kind, TrackKind::Video);
        assert_eq!(muxed.bitrate, Some(359_600));
    }

    #[test]
    fn segmented_encrypted_and_urlless_formats_are_never_offered() {
        let p = plan(&json!({"id":"v","formats":[
            {"format_id":"ok","url":"https://r.googlevideo.com/a","vcodec":"avc1","protocol":"https"},
            {"format_id":"hls","url":"https://manifest.googlevideo.com/x.m3u8","vcodec":"avc1","protocol":"m3u8_native"},
            {"format_id":"drm","url":"https://r.googlevideo.com/d","vcodec":"avc1","has_drm":true},
            {"format_id":"sabr","vcodec":"vp9","acodec":"none","protocol":"https"}
        ]}))
        .unwrap();
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].id, "ok");
    }

    #[test]
    fn best_audio_defaults_to_the_original_language_and_uncompressed_range() {
        let audio = |id: &str, language: &str, rank: i64| json!({"format_id":id,"url":"https://r.googlevideo.com/a","vcodec":"none","acodec":"opus","abr":160,"language":language,"language_preference":rank});
        let mut drc = audio("251-drc", "en", 10);
        drc["format_note"] = json!("medium, DRC");
        let p = plan(&json!({"id":"v","formats":[
            {"format_id":"248","url":"https://r.googlevideo.com/v","vcodec":"vp9","acodec":"none","height":1080},
            audio("140-0", "en", 10), audio("251-0", "en", 10), drc,
            audio("251-1", "fr", -1), audio("251-2", "de", -1)
        ]}))
        .unwrap();
        let defaults: Vec<_> = p
            .tracks
            .iter()
            .filter(|t| t.default)
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(defaults, ["140-0", "251-0"]);
        assert_eq!(track(&p, "251-1").language.as_deref(), Some("fr"));
    }

    #[test]
    fn a_single_language_video_flags_every_regular_audio_track() {
        let p = plan(&json!({"id":"v","formats":[
            {"format_id":"140","url":"https://r.googlevideo.com/a","vcodec":"none","acodec":"mp4a","abr":128},
            {"format_id":"251","url":"https://r.googlevideo.com/b","vcodec":"none","acodec":"opus","abr":160}
        ]}))
        .unwrap();
        assert!(p.tracks.iter().all(|t| t.default));
    }

    #[test]
    fn subtitles_prefer_vtt_and_translations_never_shadow_authored_ones() {
        let p = plan(&json!({"id":"v",
            "formats":[{"format_id":"18","url":"https://r.googlevideo.com/c?expire=9","vcodec":"avc1","acodec":"mp4a"}],
            "subtitles":{
                "en":[{"url":"https://www.youtube.com/api/timedtext?fmt=json3","ext":"json3"},{"url":"https://www.youtube.com/api/timedtext?fmt=vtt&expire=7","ext":"vtt"}],
                "de":[{"url":"https://www.youtube.com/api/timedtext?fmt=srt","ext":"srt"},{"ext":"vtt"}],
                "live_chat":[{"ext":"json"}]
            },
            "automatic_captions":{
                "en":[{"url":"https://www.youtube.com/api/timedtext?kind=asr","ext":"vtt"}],
                "fa":[{"url":"https://www.youtube.com/api/timedtext?tlang=fa","ext":"vtt"}]
            }}))
        .unwrap();
        let subtitles: Vec<_> = p
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Subtitle)
            .map(|t| (t.id.as_str(), t.container.as_deref(), t.auto_generated))
            .collect();
        assert_eq!(
            subtitles,
            [
                ("subtitles:de", Some("srt"), false),
                ("subtitles:en", Some("vtt"), false),
                ("automatic_captions:fa", Some("vtt"), true),
            ]
        );
        assert_eq!(p.expires_at, Some(7));
        assert!(p
            .tracks
            .iter()
            .all(|t| t.kind != TrackKind::Subtitle || !t.default));
    }

    #[test]
    fn a_long_caption_list_is_cut_to_the_host_track_limit() {
        let captions: serde_json::Map<String, Value> = (0..MAX_TRACKS + 40)
            .map(|n| {
                (
                    format!("l{n:04}"),
                    json!([{"url":"https://www.youtube.com/api/timedtext","ext":"vtt"}]),
                )
            })
            .collect();
        let p = plan(&json!({"id":"v",
            "formats":[{"format_id":"18","url":"https://r.googlevideo.com/c","vcodec":"avc1","acodec":"mp4a"}],
            "automatic_captions":captions}))
        .unwrap();
        assert_eq!(p.tracks.len(), MAX_TRACKS);
        assert_eq!(p.tracks[0].id, "18");
    }

    #[test]
    fn live_upcoming_and_empty_metadata_are_refused_with_the_reason() {
        for (info, reason) in [
            (
                json!({"id":"x","is_live":true,"formats":[]}),
                "live streams",
            ),
            (json!({"id":"x","live_status":"is_live"}), "live streams"),
            (
                json!({"id":"x","live_status":"is_upcoming"}),
                "has not started",
            ),
            (json!({"id":"x","formats":[]}), "no direct HTTP media"),
            (
                json!({"id":"x","formats":[{"format_id":"93","url":"https://manifest.googlevideo.com/p","vcodec":"avc1","protocol":"m3u8_native"}],
                       "subtitles":{"en":[{"url":"https://www.youtube.com/api/timedtext","ext":"vtt"}]}}),
                "only segmented streams",
            ),
        ] {
            let error = plan(&info).unwrap_err();
            assert_eq!(error.code, ErrorCode::Unsupported);
            assert!(error.message.contains(reason), "{}", error.message);
        }
        let error = plan(&json!({"formats":[]})).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidReply);
    }

    #[test]
    fn a_finished_live_stream_is_an_ordinary_video() {
        let p = plan(&json!({"id":"v","live_status":"was_live","is_live":false,
            "formats":[{"format_id":"18","url":"https://r.googlevideo.com/c","vcodec":"avc1","acodec":"mp4a"}]}))
        .unwrap();
        assert_eq!(p.tracks.len(), 1);
    }
}
