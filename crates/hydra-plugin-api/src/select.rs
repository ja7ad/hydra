//! Track selection: a pure function over a plan and the user's preferences,
//! shared by every front end so flags and pick lists cannot disagree.

use serde::{Deserialize, Serialize};

use crate::plan::{Plan, TrackKind};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AudioPref {
    #[default]
    Best,
    None,
    Id(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// Omit video tracks for audio extraction.
    pub audio_only: bool,
    /// Optional output audio format: mp3, m4a, opus, flac or wav.
    pub audio_format: Option<String>,
    /// Playlist item IDs to include; None selects every item.
    pub playlist_ids: Option<Vec<String>>,
    /// Highest video height wanted; the best at or below it is picked.
    pub max_height: Option<u32>,
    pub audio: AudioPref,
    /// Subtitle languages wanted; empty means none.
    pub subtitle_languages: Vec<String>,
    /// `mp4`, `mkv` or `webm`.
    pub container: Option<String>,
    /// Explicit track ids override every other rule for their kind.
    pub track_ids: Vec<String>,
    pub include_files: bool,
    /// Transfer file indices; None selects every non-padding file.
    pub transfer_files: Option<Vec<u32>>,
}

/// Indices into `Plan::tracks`, in plan order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Selection {
    pub tracks: Vec<usize>,
}

fn container_holds(container: &str, codec: Option<&str>) -> bool {
    let Some(codec) = codec else { return true };
    let c = codec.to_ascii_lowercase();
    match container {
        "mp4" => {
            c.starts_with("avc")
                || c.starts_with("hvc")
                || c.starts_with("hev")
                || c.starts_with("av01")
                || c.starts_with("mp4a")
                || c.starts_with("ac-3")
                || c.starts_with("ec-3")
        }
        "webm" => {
            c.starts_with("vp8")
                || c.starts_with("vp9")
                || c.starts_with("vp09")
                || c.starts_with("av01")
                || c.starts_with("opus")
                || c.starts_with("vorbis")
        }
        _ => true,
    }
}

/// Picks tracks per the shared rules: at most one video, at most one audio,
/// any number of subtitles and files.
pub fn select(plan: &Plan, prefs: &Preferences) -> Selection {
    let container = if prefs.audio_only {
        None
    } else {
        prefs.container.as_deref()
    };
    let usable = |i: usize| {
        let t = &plan.tracks[i];
        container.is_none_or(|c| container_holds(c, t.codec.as_deref()))
    };
    let of_kind = |kind: TrackKind| -> Vec<usize> {
        (0..plan.tracks.len())
            .filter(|&i| plan.tracks[i].kind == kind)
            .collect()
    };
    let explicit = |kind: TrackKind| -> Option<usize> {
        of_kind(kind)
            .into_iter()
            .find(|&i| prefs.track_ids.contains(&plan.tracks[i].id))
    };

    let mut chosen = Vec::new();

    let video = explicit(TrackKind::Video).or_else(|| {
        let candidates: Vec<usize> = of_kind(TrackKind::Video)
            .into_iter()
            .filter(|&i| usable(i))
            .collect();
        let flagged = candidates.iter().copied().find(|&i| plan.tracks[i].default);
        let height_of = |i: usize| plan.tracks[i].height.unwrap_or(0);
        match prefs.max_height {
            Some(max) => {
                let within = candidates
                    .iter()
                    .copied()
                    .filter(|&i| height_of(i) <= max)
                    .max_by_key(|&i| (height_of(i), plan.tracks[i].bitrate.unwrap_or(0)));
                within
                    .or(flagged)
                    .or_else(|| candidates.iter().copied().min_by_key(|&i| height_of(i)))
            }
            None => flagged.or_else(|| {
                candidates
                    .iter()
                    .copied()
                    .max_by_key(|&i| (height_of(i), plan.tracks[i].bitrate.unwrap_or(0)))
            }),
        }
    });
    if !prefs.audio_only {
        chosen.extend(video);
    }

    let audio = match &prefs.audio {
        AudioPref::None => None,
        AudioPref::Id(id) => of_kind(TrackKind::Audio)
            .into_iter()
            .find(|&i| &plan.tracks[i].id == id),
        AudioPref::Best => explicit(TrackKind::Audio).or_else(|| {
            let candidates: Vec<usize> = of_kind(TrackKind::Audio)
                .into_iter()
                .filter(|&i| usable(i))
                .collect();
            // A plan may flag several tracks, e.g. every codec of the original language.
            candidates
                .iter()
                .copied()
                .max_by_key(|&i| (plan.tracks[i].default, plan.tracks[i].bitrate.unwrap_or(0)))
        }),
    };
    chosen.extend(audio);

    for i in of_kind(TrackKind::Subtitle) {
        let t = &plan.tracks[i];
        let wanted = prefs.track_ids.contains(&t.id)
            || t.language
                .as_deref()
                .is_some_and(|l| prefs.subtitle_languages.iter().any(|w| w == l));
        if wanted {
            chosen.push(i);
        }
    }

    for i in of_kind(TrackKind::File) {
        if prefs.include_files || prefs.track_ids.contains(&plan.tracks[i].id) || {
            let only_files = plan.tracks.iter().all(|t| t.kind == TrackKind::File);
            only_files && plan.tracks[i].default
        } {
            chosen.push(i);
        }
    }

    chosen.sort_unstable();
    chosen.dedup();
    Selection { tracks: chosen }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Assemble, Track};

    fn track(id: &str, kind: TrackKind) -> Track {
        Track {
            kind,
            default: false,
            ..Track::file(id, "https://a/x")
        }
    }

    fn video(id: &str, height: u32, codec: &str) -> Track {
        Track {
            height: Some(height),
            codec: Some(codec.into()),
            ..track(id, TrackKind::Video)
        }
    }

    fn audio(id: &str, bitrate: u64, codec: &str) -> Track {
        Track {
            bitrate: Some(bitrate),
            codec: Some(codec.into()),
            ..track(id, TrackKind::Audio)
        }
    }

    fn plan(tracks: Vec<Track>) -> Plan {
        Plan {
            transfer: None,
            id: "p".into(),
            title: None,
            expires_at: None,
            assemble: Assemble::Mux,
            tracks,
            entries: Vec::new(),
        }
    }

    fn ids(p: &Plan, s: &Selection) -> Vec<String> {
        s.tracks.iter().map(|&i| p.tracks[i].id.clone()).collect()
    }

    #[test]
    fn audio_extraction_omits_video_and_ignores_video_container_filter() {
        let p = plan(vec![video("v", 1080, "avc1"), audio("a", 128000, "opus")]);
        let prefs = Preferences {
            audio_only: true,
            container: Some("mp4".into()),
            audio_format: Some("mp3".into()),
            ..Default::default()
        };
        assert_eq!(ids(&p, &select(&p, &prefs)), ["a"]);
        let legacy: Preferences = serde_json::from_str(r#"{"max_height":720,"audio":"Best","subtitle_languages":[],"container":null,"track_ids":[],"include_files":true}"#).unwrap();
        assert!(!legacy.audio_only);
        assert!(legacy.playlist_ids.is_none());
    }

    #[test]
    fn picks_highest_video_and_best_audio_by_default() {
        let p = plan(vec![
            video("v720", 720, "avc1"),
            video("v1080", 1080, "avc1"),
            audio("a64", 64_000, "mp4a.40.2"),
            audio("a128", 128_000, "mp4a.40.2"),
        ]);
        let s = select(&p, &Preferences::default());
        assert_eq!(ids(&p, &s), ["v1080", "a128"]);
    }

    #[test]
    fn flagged_default_beats_height() {
        let mut low = video("v480", 480, "avc1");
        low.default = true;
        let p = plan(vec![low, video("v1080", 1080, "avc1")]);
        let s = select(&p, &Preferences::default());
        assert_eq!(ids(&p, &s), ["v480"]);
    }

    #[test]
    fn max_height_picks_best_at_or_below() {
        let p = plan(vec![
            video("v480", 480, "avc1"),
            video("v720", 720, "avc1"),
            video("v1080", 1080, "avc1"),
        ]);
        let prefs = Preferences {
            max_height: Some(900),
            audio: AudioPref::None,
            ..Preferences::default()
        };
        assert_eq!(ids(&p, &select(&p, &prefs)), ["v720"]);
    }

    #[test]
    fn max_height_below_everything_falls_back_to_the_lowest() {
        let p = plan(vec![
            video("v720", 720, "avc1"),
            video("v1080", 1080, "avc1"),
        ]);
        let prefs = Preferences {
            max_height: Some(240),
            audio: AudioPref::None,
            ..Preferences::default()
        };
        assert_eq!(ids(&p, &select(&p, &prefs)), ["v720"]);
    }

    #[test]
    fn container_filter_drops_incompatible_codecs() {
        let p = plan(vec![
            video("vp9", 2160, "vp09.00.50.08"),
            video("avc", 1080, "avc1.640028"),
            audio("opus", 160_000, "opus"),
            audio("aac", 128_000, "mp4a.40.2"),
        ]);
        let mp4 = Preferences {
            container: Some("mp4".into()),
            ..Preferences::default()
        };
        assert_eq!(ids(&p, &select(&p, &mp4)), ["avc", "aac"]);
        let webm = Preferences {
            container: Some("webm".into()),
            ..Preferences::default()
        };
        assert_eq!(ids(&p, &select(&p, &webm)), ["vp9", "opus"]);
    }

    #[test]
    fn several_flagged_audio_tracks_compete_on_bitrate_within_the_container() {
        let flagged = |id: &str, bitrate: u64, codec: &str| Track {
            default: true,
            ..audio(id, bitrate, codec)
        };
        let p = plan(vec![
            video("v", 1080, "avc1"),
            flagged("original-aac", 128_000, "mp4a.40.2"),
            flagged("original-opus", 160_000, "opus"),
            audio("dubbed-opus", 160_000, "opus"),
            audio("dubbed-aac", 192_000, "mp4a.40.2"),
        ]);
        let any = select(&p, &Preferences::default());
        assert_eq!(ids(&p, &any), ["v", "original-opus"]);
        let mp4 = Preferences {
            container: Some("mp4".into()),
            ..Preferences::default()
        };
        assert_eq!(ids(&p, &select(&p, &mp4)), ["v", "original-aac"]);
    }

    #[test]
    fn audio_none_and_explicit_id() {
        let p = plan(vec![
            video("v", 720, "avc1"),
            audio("a1", 64_000, "mp4a"),
            audio("a2", 128_000, "mp4a"),
        ]);
        let none = Preferences {
            audio: AudioPref::None,
            ..Preferences::default()
        };
        assert_eq!(ids(&p, &select(&p, &none)), ["v"]);
        let by_id = Preferences {
            audio: AudioPref::Id("a1".into()),
            ..Preferences::default()
        };
        assert_eq!(ids(&p, &select(&p, &by_id)), ["v", "a1"]);
        let missing = Preferences {
            audio: AudioPref::Id("zz".into()),
            ..Preferences::default()
        };
        assert_eq!(ids(&p, &select(&p, &missing)), ["v"]);
    }

    #[test]
    fn subtitles_selected_by_language_only() {
        let mut en = track("s-en", TrackKind::Subtitle);
        en.language = Some("en".into());
        let mut de = track("s-de", TrackKind::Subtitle);
        de.language = Some("de".into());
        let p = plan(vec![video("v", 720, "avc1"), en, de]);
        let none = select(&p, &Preferences::default());
        assert_eq!(ids(&p, &none), ["v"]);
        let prefs = Preferences {
            subtitle_languages: vec!["de".into()],
            ..Preferences::default()
        };
        assert_eq!(ids(&p, &select(&p, &prefs)), ["v", "s-de"]);
    }

    #[test]
    fn explicit_track_ids_override_defaults() {
        let p = plan(vec![
            video("v480", 480, "avc1"),
            video("v1080", 1080, "avc1"),
        ]);
        let prefs = Preferences {
            track_ids: vec!["v480".into()],
            ..Preferences::default()
        };
        assert_eq!(ids(&p, &select(&p, &prefs)), ["v480"]);
    }

    #[test]
    fn file_only_plan_selects_its_default_file() {
        let p = plan(vec![Track::file("f", "https://a/f")]);
        assert_eq!(ids(&p, &select(&p, &Preferences::default())), ["f"]);
    }

    #[test]
    fn empty_plan_selects_nothing() {
        let p = plan(vec![]);
        assert!(select(&p, &Preferences::default()).tracks.is_empty());
    }
}
