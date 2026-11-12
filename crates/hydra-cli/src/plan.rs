//! Execute accepted plugin tracks through the existing transfer engine.
use hya_plugin_api::{Assemble, Plan, Preferences, TrackKind};
use std::path::PathBuf;

pub async fn run(
    mut plan: Plan,
    template: crate::download::Job,
    prefs: Preferences,
    plugin: &str,
) -> Result<crate::download::Outcome, String> {
    if template.to_stdout {
        return Err("plugin plans require an output file; --stdout is unsupported".into());
    }
    if let Some(transfer) = plan.transfer.take() {
        return run_transfer(
            *transfer,
            template,
            prefs,
            plugin,
            plan.title.as_deref().unwrap_or(&plan.id),
        )
        .await;
    }
    let selected = hya_plugin_api::select(&plan, &prefs);
    if selected.tracks.is_empty() {
        return Err("no tracks match the selection".into());
    }
    let now = hya_net::cookies::now_secs();
    if plan.expires_at.is_some_and(|at| at <= now)
        || selected
            .tracks
            .iter()
            .any(|&i| plan.tracks[i].expires_at.is_some_and(|at| at <= now))
    {
        let ids = selected
            .tracks
            .iter()
            .map(|&i| plan.tracks[i].id.clone())
            .collect();
        plan = crate::plugins::refresh_plan(&template, plugin, plan, ids).await?;
    }
    let extract = prefs.audio_only && prefs.audio_format.is_some();
    if prefs.audio_only
        && !selected
            .tracks
            .iter()
            .any(|&i| plan.tracks[i].kind == TrackKind::Audio)
    {
        return Err("no audio track matches the selection".into());
    }
    let mux = plan.assemble == Assemble::Mux
        && selected
            .tracks
            .iter()
            .any(|&i| plan.tracks[i].kind == TrackKind::Audio)
        && selected
            .tracks
            .iter()
            .any(|&i| plan.tracks[i].kind == TrackKind::Video);
    let started = std::time::Instant::now();
    let mut output = template.output.clone().unwrap_or_else(|| {
        let title = plan.title.as_deref().unwrap_or(&plan.id);
        let safe = hya_net::filename::portable(title).unwrap_or_else(|| "download".into());
        PathBuf::from(format!(
            "{}.{}",
            safe,
            if extract {
                prefs.audio_format.as_deref().unwrap()
            } else if mux {
                prefs.container.as_deref().unwrap_or("mkv")
            } else {
                plan.tracks[selected.tracks[0]]
                    .container
                    .as_deref()
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()))
                    .unwrap_or("bin")
            }
        ))
    });
    if output.is_relative() {
        if let Some(dir) = &template.output_dir {
            output = dir.join(output);
        }
    }
    if output.exists() && !template.force {
        return Err(format!(
            "{} already exists; use --force to replace it",
            output.display()
        ));
    }

    if (mux || extract) && !hya_stream::hls::ffmpeg_available() {
        return Err("combining tracks or extracting audio requires ffmpeg on PATH".into());
    }
    let primary = selected
        .tracks
        .iter()
        .find(|&&i| plan.tracks[i].kind != TrackKind::Subtitle)
        .or_else(|| selected.tracks.first())
        .copied();
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| e.to_string())?;
    let scratch = tempfile::Builder::new()
        .prefix(".hydra-plugin-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let mut media = Vec::new();
    for (ordinal, index) in selected.tracks.iter().enumerate() {
        let track = plan.tracks[*index].clone();
        let destination = if !mux && Some(*index) == primary {
            output.clone()
        } else {
            PathBuf::from(format!(
                "{}.track-{ordinal}.{}",
                output.display(),
                track
                    .container
                    .as_deref()
                    .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()))
                    .unwrap_or("bin")
            ))
        };
        let staged = scratch.path().join(format!("track-{ordinal}"));
        let mut job = template.clone();
        job.force = true;
        // Signed sources may change identity between resolves; never reuse old spans.
        job.resume = false;
        job.plugin_options = None;
        job.urls = track.sources.iter().map(|s| s.url.clone()).collect();
        job.output = Some(staged.clone());
        job.output_dir = None;
        job.cookies = None;
        job.headers.retain(|line| {
            line.split_once(':').is_none_or(|(name, _)| {
                !["authorization", "cookie", "proxy-authorization"]
                    .iter()
                    .any(|x| name.trim().eq_ignore_ascii_case(x))
            })
        });
        job.headers
            .extend(track.headers.iter().map(|(k, v)| format!("{k}: {v}")));
        job.checksum = track.digest.clone();
        job.attested = track.size.map(|size| crate::metalink::Attested {
            size,
            digest: track.digest.clone(),
            pieces: None,
            origin: format!("plugin {plugin}"),
        });
        job.conns = Some(
            track
                .sources
                .iter()
                .filter_map(|s| s.max_connections)
                .min()
                .unwrap_or(4) as usize,
        );
        job.chunk_size = track.chunk_hint;
        if track.ranges == hya_plugin_api::Ranges::Deny {
            job.conns = Some(1);
            job.force_stream = true;
            job.resume = false;
        }
        let mut result = crate::download::run_file(job.clone()).await;
        if !result.ok
            && result
                .note
                .as_deref()
                .is_some_and(|s| s.contains("403") || s.contains("410"))
        {
            plan = crate::plugins::refresh_plan(&template, plugin, plan, vec![track.id.clone()])
                .await?;
            let refreshed = &plan.tracks[*index];
            job.urls = refreshed.sources.iter().map(|s| s.url.clone()).collect();
            job.headers = refreshed
                .headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect();
            job.checksum = refreshed.digest.clone();
            job.attested = refreshed.size.map(|size| crate::metalink::Attested {
                size,
                digest: refreshed.digest.clone(),
                pieces: None,
                origin: format!("plugin {plugin}"),
            });
            result = crate::download::run_file(job).await;
        }
        if !result.ok {
            return Err(format!(
                "track {}: {}",
                track.id,
                result.note.unwrap_or_else(|| "download failed".into())
            ));
        }
        media.push((track.kind, staged, destination));
    }
    if mux {
        let video = media
            .iter()
            .find(|(k, _, _)| *k == TrackKind::Video)
            .unwrap()
            .1
            .clone();
        let audio = media
            .iter()
            .find(|(k, _, _)| *k == TrackKind::Audio)
            .unwrap()
            .1
            .clone();
        let staged = scratch.path().join(format!(
            "assembled.{}",
            prefs.container.as_deref().unwrap_or("mkv")
        ));
        let target = staged.clone();
        tokio::task::spawn_blocking(move || {
            hya_stream::hls::mux(&video, &audio, &target, hya_stream::hls::Segments::Fmp4)
        })
        .await
        .map_err(|e| e.to_string())??;
        publish(&staged, &output, template.force)?;
    }
    if extract {
        let audio = media
            .iter()
            .find(|(kind, _, _)| *kind == TrackKind::Audio)
            .ok_or("no audio track is available for extraction")?;
        let format = prefs.audio_format.as_deref().unwrap();
        let converted = scratch.path().join(format!("audio.{format}"));
        let ffmpeg = hya_stream::hls::ffmpeg().ok_or("audio extraction requires ffmpeg")?;
        hya_plugin::media::extract_audio(
            &ffmpeg,
            &audio.1,
            &converted,
            format,
            template.cancel.clone(),
        )
        .await?;
        publish(&converted, &output, template.force)?;
    }
    for (kind, staged, destination) in media {
        if (!mux && !extract) || matches!(kind, TrackKind::Subtitle | TrackKind::File) {
            publish(&staged, &destination, template.force)?;
        }
    }
    crate::plugins::finish_job(template.urls[0].clone(), output.clone()).await?;
    let size = tokio::fs::metadata(&output).await.map_or(0, |m| m.len());
    Ok(crate::download::Outcome {
        url: template.urls.first().cloned().unwrap_or_default(),
        output: output.to_string_lossy().into_owned(),
        size,
        elapsed_s: started.elapsed().as_secs_f64(),
        ok: true,
        ..Default::default()
    })
}

pub async fn run_playlist(
    plan: Plan,
    template: crate::download::Job,
    prefs: Preferences,
    plugin: &str,
) -> Result<crate::download::Outcome, String> {
    if template.output.is_some() || template.to_stdout {
        return Err(
            "playlists require --dir or the current directory; a single output file is unsupported"
                .into(),
        );
    }
    let entries: Vec<_> = plan
        .entries
        .into_iter()
        .filter(|entry| {
            prefs
                .playlist_ids
                .as_ref()
                .is_none_or(|ids| ids.contains(&entry.id))
        })
        .collect();
    if entries.is_empty() {
        return Err("no playlist items selected".into());
    }
    let title = hya_net::filename::portable(plan.title.as_deref().unwrap_or(&plan.id))
        .unwrap_or_else(|| "playlist".into());
    let directory = template
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(title);
    let start = std::time::Instant::now();
    let size = run_entries(&entries, template.cancel.as_deref(), |index, entry| {
        let mut job = template.clone();
        job.urls = vec![entry.url.clone()];
        job.output_dir = Some(directory.join(format!("{:03}-{}", index + 1, entry.id)));
        if let Some(options) = &mut job.plugin_options {
            options.only = Some(plugin.into());
        }
        run_playlist_item(job, &prefs, plugin)
    })
    .await?;
    Ok(crate::download::Outcome {
        url: template.urls[0].clone(),
        output: directory.to_string_lossy().into_owned(),
        size,
        elapsed_s: start.elapsed().as_secs_f64(),
        ok: true,
        ..Default::default()
    })
}

/// Downloads every entry in order and returns the bytes saved; an entry that
/// fails is reported and skipped, and only cancellation stops the rest.
async fn run_entries<F, Fut>(
    entries: &[hya_plugin_api::PlaylistEntry],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    mut download: F,
) -> Result<u64, String>
where
    F: FnMut(usize, &hya_plugin_api::PlaylistEntry) -> Fut,
    Fut: std::future::Future<Output = Result<crate::download::Outcome, String>>,
{
    let mut size = 0u64;
    let mut failures = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
            return Err("playlist cancelled".into());
        }
        eprintln!(
            "Playlist {}/{} — {}",
            index + 1,
            entries.len(),
            crate::plugin_ui::clean(entry.title.as_deref().unwrap_or(&entry.id))
        );
        match download(index, entry).await {
            Ok(outcome) => size = size.saturating_add(outcome.size),
            Err(error) => {
                eprintln!("hydra: playlist item {}: {error}", entry.id);
                failures.push(format!("{}: {error}", entry.id));
            }
        }
    }
    playlist_verdict(entries.len(), &failures)?;
    Ok(size)
}

async fn run_playlist_item(
    job: crate::download::Job,
    prefs: &Preferences,
    plugin: &str,
) -> Result<crate::download::Outcome, String> {
    let (id, item) = crate::plugins::resolve_job(&job)
        .await?
        .ok_or_else(|| format!("plugin {plugin} did not resolve it"))?;
    if !item.entries.is_empty() {
        return Err("nested playlists are unsupported".into());
    }
    run(item, job, prefs.clone(), &id).await
}

/// One unavailable video must not cost the rest of the playlist, but the run
/// still fails so scripts notice the gap.
fn playlist_verdict(total: usize, failures: &[String]) -> Result<(), String> {
    match failures {
        [] => Ok(()),
        [only] if total == 1 => Err(format!("playlist item {only}")),
        _ => Err(format!(
            "{} of {total} playlist items failed: {}",
            failures.len(),
            failures.join("; ")
        )),
    }
}

fn publish(
    source: &std::path::Path,
    destination: &std::path::Path,
    replace: bool,
) -> Result<(), String> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    std::fs::copy(source, file.path()).map_err(|e| e.to_string())?;
    if replace {
        file.persist(destination)
    } else {
        file.persist_noclobber(destination)
    }
    .map_err(|e| e.to_string())?;
    Ok(())
}

async fn run_transfer(
    transfer: hya_plugin_api::Transfer,
    template: crate::download::Job,
    prefs: Preferences,
    plugin: &str,
    title: &str,
) -> Result<crate::download::Outcome, String> {
    hya_plugin::transfer::validate(&transfer).map_err(|error| error.to_string())?;
    let started = std::time::Instant::now();
    let authorization_plugin = plugin.to_owned();
    let program = transfer.engine.clone();
    let executable = tokio::task::spawn_blocking(move || {
        hya_plugin::transfer::authorize(
            hya_plugin::hydra_dir().join("plugins"),
            &authorization_plugin,
            &program,
        )
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    let destination = template.output.clone().unwrap_or_else(|| {
        template
            .output_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(hya_net::filename::portable(title).unwrap_or_else(|| "download".into()))
    });
    let selected_size: u64 = transfer
        .files
        .iter()
        .filter(|file| {
            prefs
                .transfer_files
                .as_ref()
                .is_none_or(|indices| indices.contains(&file.index))
        })
        .map(|file| file.size)
        .sum();
    if template
        .max_filesize
        .is_some_and(|maximum| selected_size > maximum)
    {
        return Err("selected torrent files exceed --max-filesize".into());
    }
    if template.no_clobber && destination.exists() {
        return Err(format!(
            "{} already exists; --no-clobber prevents resuming it",
            destination.display()
        ));
    }
    let mut resume_name = destination.as_os_str().to_owned();
    resume_name.push(".hydra-transfer-resume");
    let schema = transfer.details.clone();
    let mut display = crate::download::progress_for(&template, title, Some(selected_size))?;
    let resuming = destination.exists();
    let mut baseline_set = !resuming;
    let request = hya_plugin::transfer::Request {
        transfer,
        destination: destination.clone(),
        files: prefs.transfer_files,
        download_limit: template.limit_rate,
        control: None,
        resume_path: resume_name.into(),
        proxy: (!template.no_proxy)
            .then_some(template.proxy.as_deref())
            .flatten()
            .map(hya_net::Proxy::parse)
            .transpose()?
            .as_ref()
            .map(hya_plugin::transfer::proxy_config),
    };
    let cancel = template
        .cancel
        .clone()
        .unwrap_or_else(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
    let filter = std::env::var("HYDRA_LOG").unwrap_or_else(|_| "info".into());
    let mut last_done = 0;
    let result = hya_plugin::transfer::download(&executable, request, cancel, |progress| {
        for record in &progress.logs {
            if crate::plugins::log_enabled(&record.level, &filter) {
                display.event(
                    0,
                    &format!("[{plugin}] {}: {}", record.level, record.message),
                );
            }
        }
        if !baseline_set && progress.state != "checking" {
            display.set_baseline(progress.done);
            baseline_set = true;
        }
        last_done = progress.done;
        display.set_reported_rate(progress.download_rate);
        display.set_plugin_details(
            format!(
                "{} · {} peers · upload {}/s",
                progress.state,
                progress.peers,
                hya_core::fmt::bytes(progress.upload_rate)
            ),
            schema.as_ref(),
            &progress.details,
        );
        display.draw(progress.done, &[], Default::default());
    })
    .await;
    match &result {
        Ok(progress) => display.finish(
            progress.done,
            progress.state == "complete",
            Default::default(),
            None,
        ),
        Err(error) => {
            display.event(0, &format!("[{plugin}] error: {error}"));
            display.finish(last_done, false, Default::default(), None);
        }
    }
    let progress = result?;
    Ok(crate::download::Outcome {
        url: template.urls.first().cloned().unwrap_or_default(),
        output: destination.to_string_lossy().into_owned(),
        size: progress.done,
        elapsed_s: started.elapsed().as_secs_f64(),
        transfer_s: started.elapsed().as_secs_f64(),
        throughput_bps: progress.done as f64 / started.elapsed().as_secs_f64().max(0.001),
        ok: progress.state == "complete",
        note: (progress.state != "complete").then(|| "transfer stopped; resume state saved".into()),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(ids: &[&str]) -> Vec<hya_plugin_api::PlaylistEntry> {
        ids.iter()
            .map(|id| hya_plugin_api::PlaylistEntry {
                id: (*id).into(),
                url: format!("https://example.com/{id}"),
                title: None,
            })
            .collect()
    }

    fn saved(size: u64) -> Result<crate::download::Outcome, String> {
        Ok(crate::download::Outcome {
            size,
            ok: true,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn an_item_that_fails_does_not_stop_the_items_after_it() {
        let playlist = entries(&["a", "b", "c"]);
        let mut attempted = Vec::new();
        let result = run_entries(&playlist, None, |index, entry| {
            attempted.push((index, entry.id.clone()));
            let outcome = if entry.id == "b" {
                Err("track 18: server answered 403 Forbidden".to_owned())
            } else {
                saved(10)
            };
            async move { outcome }
        })
        .await;
        assert_eq!(
            attempted,
            [
                (0, "a".to_owned()),
                (1, "b".to_owned()),
                (2, "c".to_owned())
            ]
        );
        assert_eq!(
            result.unwrap_err(),
            "1 of 3 playlist items failed: b: track 18: server answered 403 Forbidden"
        );
    }

    #[tokio::test]
    async fn a_clean_playlist_reports_the_bytes_of_every_item() {
        let playlist = entries(&["a", "b"]);
        let size = run_entries(&playlist, None, |index, _| async move {
            saved(100 + index as u64)
        })
        .await;
        assert_eq!(size, Ok(201));
    }

    #[tokio::test]
    async fn cancellation_stops_before_the_next_item_starts() {
        let playlist = entries(&["a", "b", "c"]);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut started = 0;
        let result = run_entries(&playlist, Some(&cancel), |_, _| {
            started += 1;
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            async { saved(1) }
        })
        .await;
        assert_eq!(result.unwrap_err(), "playlist cancelled");
        assert_eq!(started, 1);
    }

    #[test]
    fn a_playlist_succeeds_only_when_every_item_did() {
        assert_eq!(playlist_verdict(3, &[]), Ok(()));
    }

    #[test]
    fn failed_items_are_counted_and_named_after_the_rest_finished() {
        let failures = [
            "b: track 18: 403 Forbidden".to_owned(),
            "c: no tracks".to_owned(),
        ];
        assert_eq!(
            playlist_verdict(5, &failures).unwrap_err(),
            "2 of 5 playlist items failed: b: track 18: 403 Forbidden; c: no tracks"
        );
        assert_eq!(
            playlist_verdict(2, &failures[..1]).unwrap_err(),
            "1 of 2 playlist items failed: b: track 18: 403 Forbidden"
        );
    }

    #[test]
    fn a_single_item_playlist_reports_its_failure_plainly() {
        assert_eq!(
            playlist_verdict(1, &["a: no tracks match the selection".to_owned()]).unwrap_err(),
            "playlist item a: no tracks match the selection"
        );
    }
}
