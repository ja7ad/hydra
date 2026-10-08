// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//! Accepted resolver plans use the desktop engine for each selected track.
use super::Pace;
use super::{Event, StartSpec};
use hya_plugin_api::{Assemble, Preferences, ResolveRequest, TrackKind};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Default)]
struct NativeRates {
    cap: u64,
    active: usize,
    transfers:
        std::collections::BTreeMap<crate::model::DlId, (u64, Arc<std::sync::atomic::AtomicU64>)>,
}

fn rates() -> &'static Mutex<NativeRates> {
    static RATES: std::sync::OnceLock<Mutex<NativeRates>> = std::sync::OnceLock::new();
    RATES.get_or_init(|| Mutex::new(NativeRates::default()))
}

fn distribute_rates(state: &NativeRates) {
    super::global_limiter().set_rate(distribute_native_rates(state));
}

fn distribute_native_rates(state: &NativeRates) -> u64 {
    let count = state.transfers.len();
    let http_active = state.active > count;
    let lanes = count + usize::from(http_active);
    let reserved = if http_active && state.cap > 0 {
        (state.cap / lanes.max(1) as u64).max(1)
    } else {
        0
    };
    let native_budget = state.cap.saturating_sub(reserved);
    let mut allocated = 0;
    for (ordinal, (requested, control)) in state.transfers.values().enumerate() {
        let share = if count == 0 {
            0
        } else {
            native_budget / count as u64
                + u64::from((ordinal as u64) < native_budget % count as u64)
        };
        let cap = if state.cap == 0 {
            *requested
        } else if share == 0 {
            u64::MAX - 1
        } else if *requested == 0 {
            share
        } else {
            share.min(*requested)
        };
        if cap != u64::MAX - 1 {
            allocated += cap;
        }
        control.store(cap, Ordering::Relaxed);
    }
    if state.cap == 0 {
        0
    } else {
        state.cap.saturating_sub(allocated).max(1)
    }
}

pub(super) fn set_global_limit(cap: u64) {
    let mut state = rates().lock().unwrap_or_else(|error| error.into_inner());
    state.cap = cap;
    distribute_rates(&state);
}

pub(super) fn set_active_count(count: usize) {
    let mut state = rates().lock().unwrap_or_else(|error| error.into_inner());
    if state.active != count {
        state.active = count;
        distribute_rates(&state);
    }
}

pub(super) fn set_native_limit(id: crate::model::DlId, cap: u64) {
    let mut state = rates().lock().unwrap_or_else(|error| error.into_inner());
    if let Some((requested, _)) = state.transfers.get_mut(&id) {
        *requested = cap;
        distribute_rates(&state);
    }
}

struct NativeRateGuard(crate::model::DlId);
impl NativeRateGuard {
    fn register(id: crate::model::DlId, cap: u64) -> (Self, Arc<std::sync::atomic::AtomicU64>) {
        let control = Arc::new(std::sync::atomic::AtomicU64::new(cap));
        let mut state = rates().lock().unwrap_or_else(|error| error.into_inner());
        state.transfers.insert(id, (cap, control.clone()));
        distribute_rates(&state);
        (Self(id), control)
    }
}
impl Drop for NativeRateGuard {
    fn drop(&mut self) {
        let mut state = rates().lock().unwrap_or_else(|error| error.into_inner());
        state.transfers.remove(&self.0);
        distribute_rates(&state);
    }
}

pub(super) async fn run(
    spec: &StartSpec,
    cancel: &Arc<AtomicBool>,
    pace: &Pace,
    final_path: &Arc<Mutex<String>>,
    tx: &UnboundedSender<Event>,
) -> bool {
    run_with_root(
        spec,
        cancel,
        pace,
        final_path,
        tx,
        crate::model::app_dir().join("plugins"),
    )
    .await
}

async fn run_with_root(
    spec: &StartSpec,
    cancel: &Arc<AtomicBool>,
    pace: &Pace,
    final_path: &Arc<Mutex<String>>,
    tx: &UnboundedSender<Event>,
    root: std::path::PathBuf,
) -> bool {
    if let Some(info) = &spec.plugin_plan {
        if let Some(transfer) = &info.plan.transfer {
            run_transfer(
                spec,
                &info.plugin,
                (**transfer).clone(),
                info.preferences.transfer_files.clone(),
                cancel,
                final_path,
                tx,
                root,
            )
            .await;
            return true;
        }
    }
    let request = ResolveRequest {
        url: spec.url.clone(),
        headers: spec
            .referer
            .as_ref()
            .map(|r| [("Referer".into(), r.clone())].into_iter().collect())
            .unwrap_or_default(),
    };
    let route = match crate::proxy::for_choice(&spec.proxy) {
        Ok(route) => route,
        Err(error) => {
            let _ = tx.send(Event::Failed {
                id: spec.id,
                error,
                done: 0,
                held: vec![],
                permission_denied: false,
            });
            return true;
        }
    };
    let mut context = hya_plugin::manager::ResolveContext {
        input_file: url::Url::parse(&spec.url)
            .ok()
            .and_then(|url| url.to_file_path().ok()),
        proxy: route.plugin_proxy(),
        ..Default::default()
    };
    if let (Some(cookies), Ok(url)) = (&spec.cookies, super::parse_url(&spec.url)) {
        context.cookies.add_pairs(cookies, &url.host);
    }
    context.only = spec.plugin_plan.as_ref().map(|p| p.plugin.clone());
    let ctl = context.ctl.clone();
    let mut worker = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let mut manager =
            hya_plugin::manager::Manager::open_with_official(root).map_err(|e| e.to_string())?;
        let connector =
            hya_plugin::http::connector(context.proxy.as_ref()).map_err(|e| e.to_string())?;
        manager
            .resolve_with_context(
                || connector.clone(),
                request,
                |id| Box::new(crate::plugins::Frontend { plugin: id.into() }),
                context,
            )
            .map_err(|e| e.to_string())
    });
    let resolved = loop {
        tokio::select! {
            result = &mut worker => break result,
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => { if cancel.load(Ordering::Relaxed) { ctl.cancel(); } }
        }
    };
    let fail = |error: String| {
        let _ = tx.send(Event::Failed {
            id: spec.id,
            error,
            done: 0,
            held: vec![],
            permission_denied: false,
        });
    };
    let (plugin, mut plan) = match resolved {
        Ok(Ok(None)) => return false,
        Ok(Ok(Some(plan))) => plan,
        Ok(Err(error)) => {
            fail(error);
            return true;
        }
        Err(error) => {
            fail(format!("plugin worker: {error}"));
            return true;
        }
    };
    if cancel.load(Ordering::Relaxed) {
        let _ = tx.send(Event::Stopped {
            id: spec.id,
            done: 0,
            held: vec![],
        });
        return true;
    }
    let prefs = spec
        .plugin_plan
        .as_ref()
        .map(|p| p.preferences.clone())
        .unwrap_or_else(|| Preferences {
            include_files: true,
            container: Some("mkv".into()),
            ..Default::default()
        });
    if let Some(transfer) = plan.transfer.take() {
        run_transfer(
            spec,
            &plugin,
            *transfer,
            prefs.transfer_files,
            cancel,
            final_path,
            tx,
            crate::model::app_dir().join("plugins"),
        )
        .await;
        return true;
    }
    let selected = hya_plugin_api::select(&plan, &prefs);
    if selected.tracks.is_empty() {
        fail(format!("plugin {plugin}: no tracks selected"));
        return true;
    }
    let extract = prefs.audio_only && prefs.audio_format.is_some();
    if prefs.audio_only
        && !selected
            .tracks
            .iter()
            .any(|&i| plan.tracks[i].kind == TrackKind::Audio)
    {
        fail(format!(
            "plugin {plugin}: no audio track matches the selection"
        ));
        return true;
    }
    let mux = plan.assemble == Assemble::Mux
        && selected
            .tracks
            .iter()
            .any(|&i| plan.tracks[i].kind == TrackKind::Video)
        && selected
            .tracks
            .iter()
            .any(|&i| plan.tracks[i].kind == TrackKind::Audio);
    if (mux || extract) && !hya_stream::hls::ffmpeg_available() {
        fail(format!(
            "plugin {plugin}: combining tracks or extracting audio requires ffmpeg"
        ));
        return true;
    }
    let size = selected.tracks.iter().try_fold(0u64, |n, &i| {
        plan.tracks[i].size.and_then(|s| n.checked_add(s))
    });
    let title = plan.title.as_deref().unwrap_or(&plan.id);
    let name = hya_net::filename::portable(title).unwrap_or_else(|| "download".into());
    let ext = if extract {
        prefs.audio_format.as_deref().unwrap()
    } else if mux {
        prefs.container.as_deref().unwrap_or("mkv")
    } else {
        plan.tracks[selected.tracks[0]]
            .container
            .as_deref()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()))
            .unwrap_or("bin")
    };
    let pending_audio = prefs.audio_only
        && prefs.audio_format.is_none()
        && spec.plugin_plan.as_ref().is_some_and(|info| {
            info.plan.tracks.iter().any(|track| {
                track.kind == TrackKind::File
                    && track.sources.iter().any(|source| source.url == spec.url)
            })
        });
    let file_name = if pending_audio {
        let mut path = std::path::PathBuf::from(&spec.final_path);
        path.set_extension(ext);
        if path.to_string_lossy() != spec.final_path {
            if path.exists() {
                fail(format!(
                    "plugin {plugin}: {} already exists",
                    path.display()
                ));
                return true;
            }
            *final_path.lock().unwrap_or_else(|error| error.into_inner()) =
                path.to_string_lossy().into_owned();
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        } else {
            None
        }
    } else {
        spec.plugin_plan
            .is_none()
            .then(|| format!("{}.{}", name.trim_matches('.'), ext))
    };
    let _ = tx.send(Event::Probed {
        id: spec.id,
        size,
        ranges: false,
        file_name,
    });
    let start = std::time::Instant::now();
    let mut completed = 0;
    let scratch_parent = std::path::Path::new(&spec.temp_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    if let Err(e) = std::fs::create_dir_all(scratch_parent) {
        fail(format!("plugin {plugin}: {e}"));
        return true;
    }
    let scratch = match tempfile::tempdir_in(scratch_parent) {
        Ok(dir) => dir,
        Err(e) => {
            fail(format!("plugin {plugin}: {e}"));
            return true;
        }
    };
    let mut outputs = Vec::new();
    for (ordinal, &i) in selected.tracks.iter().enumerate() {
        let mut attempt = 0;
        loop {
            let track = plan.tracks[i].clone();
            if attempt == 0
                && (plan
                    .expires_at
                    .is_some_and(|at| at <= hya_net::cookies::now_secs())
                    || track
                        .expires_at
                        .is_some_and(|at| at <= hya_net::cookies::now_secs()))
            {
                match refresh(spec, &plugin, plan.clone(), vec![track.id.clone()], cancel).await {
                    Ok(next) => {
                        plan = next;
                        attempt = 1;
                        continue;
                    }
                    Err(error) => {
                        fail(format!("plugin {plugin}: {error}"));
                        return true;
                    }
                }
            }
            let mut child = spec.clone();
            child.url = track.sources[0].url.clone();
            child.auth = None;
            child.cookies = None;
            child.referer = None;
            child.plugin_headers = track
                .headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect();
            child.mirrors = track
                .sources
                .iter()
                .skip(1)
                .map(|source| crate::model::MirrorRef {
                    url: source.url.clone(),
                    priority: source.priority.unwrap_or(1),
                    max_connections: source.max_connections.map(|n| n as usize),
                })
                .collect();
            child.held.clear();
            child.expected_size = None;
            child.attested_size = track.size;
            child.attested_digest = track.digest.clone();
            child.pieces = None;
            child.conns = track
                .sources
                .iter()
                .filter_map(|s| s.max_connections)
                .min()
                .unwrap_or(4) as usize;
            if track.ranges == hya_plugin_api::Ranges::Deny {
                child.conns = 1;
                child.force_stream = true;
            }
            child.temp_path = scratch
                .path()
                .join(format!("track-{ordinal}.part"))
                .to_string_lossy()
                .into_owned();
            child.final_path = scratch
                .path()
                .join(format!("track-{ordinal}"))
                .to_string_lossy()
                .into_owned();
            let destination = child.final_path.clone();
            let (events, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let task = super::run_file_download(
                child,
                cancel.clone(),
                pace.clone(),
                Arc::new(Mutex::new(destination.clone())),
                events,
            );
            tokio::pin!(task);
            let mut finished = None;
            let mut failure = None;
            loop {
                tokio::select! {
                    () = &mut task => { while let Ok(event) = rx.try_recv() { if let Event::Finished {size,..} = event { finished = Some(size); } else if let Event::Failed {error,..} = event {failure = Some(error);} } break; }
                    event = rx.recv() => match event {
                        Some(Event::Finished {size,..}) => finished = Some(size),
                        Some(Event::Progress {done,rate,eta,conns,..}) => { let _ = tx.send(Event::Progress { id:spec.id,done:completed+done,rate,eta,conns,held:vec![],recorded:None }); }
                        Some(Event::Failed {error,..}) => {failure = Some(error);break;}
                        Some(Event::Stopped {done,..}) => { let _ = tx.send(Event::Stopped { id:spec.id,done:completed+done,held:vec![] }); return true; }
                        Some(Event::Status {line,..}) => { let _ = tx.send(Event::Status {id:spec.id,line:format!("{plugin}: {line}")}); }
                        _ => {}
                    }
                }
            }
            if let Some(error) = failure {
                if attempt == 0 && (error.contains("403") || error.contains("410")) {
                    match refresh(spec, &plugin, plan.clone(), vec![track.id.clone()], cancel).await
                    {
                        Ok(next) => {
                            plan = next;
                            attempt = 1;
                            continue;
                        }
                        Err(error) => {
                            fail(format!("plugin {plugin}: {error}"));
                            return true;
                        }
                    }
                }
                fail(format!("plugin {plugin}, track {}: {error}", track.id));
                return true;
            }
            let Some(size) = finished else {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(Event::Stopped {
                        id: spec.id,
                        done: completed,
                        held: vec![],
                    });
                } else {
                    fail(format!("plugin {plugin}: track did not finish"));
                }
                return true;
            };
            completed += size;
            outputs.push((i, destination));
            break;
        }
    }
    let mut finish_spec = spec.clone();
    if mux {
        let video = outputs
            .iter()
            .find(|(index, _)| plan.tracks[*index].kind == TrackKind::Video)
            .unwrap()
            .1
            .clone();
        let audio = outputs
            .iter()
            .find(|(index, _)| plan.tracks[*index].kind == TrackKind::Audio)
            .unwrap()
            .1
            .clone();
        let staged = scratch
            .path()
            .join(format!(
                "mux.{}",
                prefs.container.as_deref().unwrap_or("mkv")
            ))
            .to_string_lossy()
            .into_owned();
        let output = staged.clone();
        match tokio::task::spawn_blocking(move || {
            hya_stream::hls::mux(
                std::path::Path::new(&video),
                std::path::Path::new(&audio),
                std::path::Path::new(&output),
                hya_stream::hls::Segments::Fmp4,
            )
        })
        .await
        {
            Ok(Ok(())) => finish_spec.temp_path = staged,
            Ok(Err(e)) => {
                fail(format!("plugin {plugin}: {e}"));
                return true;
            }
            Err(e) => {
                fail(format!("plugin {plugin}: {e}"));
                return true;
            }
        }
    } else {
        let primary = outputs
            .iter()
            .find(|(index, _)| plan.tracks[*index].kind != TrackKind::Subtitle)
            .unwrap_or(&outputs[0]);
        finish_spec.temp_path = primary.1.clone();
    }
    if extract {
        let Some(audio) = outputs
            .iter()
            .find(|(index, _)| plan.tracks[*index].kind == TrackKind::Audio)
        else {
            fail(format!("plugin {plugin}: no audio track available"));
            return true;
        };
        let format = prefs.audio_format.as_deref().unwrap();
        let converted = scratch.path().join(format!("audio.{format}"));
        match hya_plugin::media::extract_audio(
            std::path::Path::new(&audio.1),
            &converted,
            format,
            Some(cancel.clone()),
        )
        .await
        {
            Ok(()) => finish_spec.temp_path = converted.to_string_lossy().into_owned(),
            Err(error) => {
                fail(format!("plugin {plugin}: {error}"));
                return true;
            }
        }
    }
    let destination = final_path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for (index, path) in &outputs {
        let track = &plan.tracks[*index];
        if path == &finish_spec.temp_path
            || ((mux || extract) && matches!(track.kind, TrackKind::Video | TrackKind::Audio))
        {
            continue;
        }
        let extension = track
            .container
            .as_deref()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()))
            .unwrap_or("bin");
        let label = hya_net::filename::portable(track.language.as_deref().unwrap_or(&track.id))
            .unwrap_or_else(|| index.to_string());
        let sidecar = format!("{destination}.{index}-{label}.{extension}");
        if let Err(e) = write_sidecar(std::path::Path::new(path), std::path::Path::new(&sidecar)) {
            fail(format!("plugin {plugin}: {e}"));
            return true;
        }
    }
    let size = tokio::fs::metadata(&finish_spec.temp_path)
        .await
        .map_or(completed, |m| m.len());
    super::finish_file(
        &finish_spec,
        final_path,
        tx,
        size,
        start.elapsed().as_secs_f64(),
        None,
        None,
    )
    .await;
    true
}

async fn run_transfer(
    spec: &StartSpec,
    plugin: &str,
    transfer: hya_plugin_api::Transfer,
    files: Option<Vec<u32>>,
    cancel: &Arc<AtomicBool>,
    final_path: &Arc<Mutex<String>>,
    tx: &UnboundedSender<Event>,
    root: std::path::PathBuf,
) {
    let started = std::time::Instant::now();
    let (_rate_guard, control) = NativeRateGuard::register(spec.id, spec.limit.unwrap_or(0));
    let authorization_plugin = plugin.to_owned();
    let program = transfer.engine.clone();
    let mut done = 0;
    let mut probed = false;
    let result = async {
        let proxy = crate::proxy::for_choice(&spec.proxy)?
            .plugin_proxy()
            .as_ref()
            .map(hya_plugin::transfer::proxy_config);
        let executable = tokio::task::spawn_blocking(move || {
            hya_plugin::transfer::authorize(root, &authorization_plugin, &program)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        let destination = final_path.lock().map_err(|e| e.to_string())?.clone().into();
        let request = hya_plugin::transfer::Request {
            transfer,
            control: Some(control),
            destination,
            files,
            download_limit: spec.limit.unwrap_or(0),
            resume_path: format!("{}.transfer-resume", spec.temp_path).into(),
            proxy,
        };
        hya_plugin::transfer::download(&executable, request, cancel.clone(), |progress| {
            for record in &progress.logs {
                hya_plugin::host::Frontend::log(
                    &mut crate::plugins::Frontend {
                        plugin: plugin.into(),
                    },
                    &record.level,
                    &record.message,
                );
            }
            let _ = tx.send(Event::PluginDetails {
                id: spec.id,
                rows: progress.details.clone(),
            });
            done = progress.done;
            if !probed {
                let _ = tx.send(Event::Probed {
                    id: spec.id,
                    size: Some(progress.total),
                    ranges: false,
                    file_name: None,
                });
                probed = true;
            }
            let _ = tx.send(Event::Status {
                id: spec.id,
                line: format!(
                    "{} · {} peers · upload {}/s",
                    progress.state,
                    progress.peers,
                    crate::fmt::size2(progress.upload_rate)
                ),
            });
            let _ = tx.send(Event::Progress {
                id: spec.id,
                done: progress.done,
                rate: progress.download_rate as f64,
                eta: (progress.download_rate > 0)
                    .then(|| progress.total.saturating_sub(progress.done) / progress.download_rate),
                conns: vec![],
                held: vec![],
                recorded: None,
            });
        })
        .await
    }
    .await;
    match result {
        Ok(progress) if progress.state == "complete" => {
            let _ = tx.send(Event::Finished {
                id: spec.id,
                elapsed: started.elapsed().as_secs_f64(),
                size: progress.done,
            });
        }
        Ok(progress) => {
            let _ = tx.send(Event::Stopped {
                id: spec.id,
                done: progress.done,
                held: vec![],
            });
        }
        Err(error) => {
            crate::log::error(&format!("plugin {plugin}: transfer failed: {error}"));
            let _ = tx.send(Event::Failed {
                id: spec.id,
                error,
                done,
                held: vec![],
                permission_denied: false,
            });
        }
    }
}

async fn refresh(
    spec: &StartSpec,
    plugin: &str,
    plan: hya_plugin_api::Plan,
    ids: Vec<String>,
    cancel: &Arc<AtomicBool>,
) -> Result<hya_plugin_api::Plan, String> {
    let root = crate::model::app_dir().join("plugins");
    let plugin = plugin.to_string();
    let request = ResolveRequest {
        url: spec.url.clone(),
        headers: spec
            .referer
            .as_ref()
            .map(|r| [("Referer".into(), r.clone())].into_iter().collect())
            .unwrap_or_default(),
    };
    let mut context = hya_plugin::manager::ResolveContext {
        input_file: url::Url::parse(&spec.url)
            .ok()
            .and_then(|url| url.to_file_path().ok()),
        proxy: crate::proxy::for_choice(&spec.proxy)?.plugin_proxy(),
        ..Default::default()
    };
    if let (Some(cookies), Ok(url)) = (&spec.cookies, super::parse_url(&spec.url)) {
        context.cookies.add_pairs(cookies, &url.host);
    }
    let ctl = context.ctl.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        let mut manager =
            hya_plugin::manager::Manager::open_with_official(root).map_err(|e| e.to_string())?;
        let connector =
            hya_plugin::http::connector(context.proxy.as_ref()).map_err(|e| e.to_string())?;
        manager
            .refresh_plan(
                &plugin,
                request,
                plan,
                ids,
                || connector.clone(),
                |id| Box::new(crate::plugins::Frontend { plugin: id.into() }),
                context,
            )
            .map_err(|e| e.to_string())
    });
    loop {
        tokio::select! {
            result = &mut worker => return result.map_err(|e|e.to_string())?,
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => if cancel.load(Ordering::Relaxed) {ctl.cancel();}
        }
    }
}

fn write_sidecar(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
    let parent = destination
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let staged = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::copy(source, staged.path())?;
    staged.persist_noclobber(destination).map_err(|e| e.error)?;
    Ok(())
}

pub(super) async fn with_hooks(
    spec: &StartSpec,
    cancel: &Arc<AtomicBool>,
    final_path: &Arc<Mutex<String>>,
    tx: &UnboundedSender<Event>,
    task: impl std::future::Future<Output = ()>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Event>,
) {
    tokio::pin!(task);
    let mut finished = None;
    let mut accept = |event| {
        if let Event::Finished { elapsed, size, .. } = event {
            finished = Some((elapsed, size));
        } else {
            let _ = tx.send(event);
        }
    };
    loop {
        tokio::select! {
            () = &mut task => {while let Ok(event) = rx.try_recv() {accept(event);} break;},
            event = rx.recv() => {if let Some(event) = event {accept(event);}}
        }
    }
    let Some((elapsed, size)) = finished else {
        return;
    };
    let url = spec.url.clone();
    let path = final_path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let output = path.clone();
    let root = crate::model::app_dir().join("plugins");
    let context = hya_plugin::manager::ResolveContext {
        proxy: crate::proxy::active().plugin_proxy(),
        ..Default::default()
    };
    let ctl = context.ctl.clone();
    let mut worker = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let manager =
            hya_plugin::manager::Manager::open_with_official(root).map_err(|e| e.to_string())?;
        let connector =
            hya_plugin::http::connector(context.proxy.as_ref()).map_err(|e| e.to_string())?;
        let path = std::path::Path::new(&path);
        let request = hya_plugin_api::CompleteRequest {
            url,
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            size,
        };
        manager
            .finish(
                request,
                path,
                || connector.clone(),
                |id| Box::new(crate::plugins::Frontend { plugin: id.into() }),
                context,
            )
            .map_err(|e| e.to_string())
    });
    let result = loop {
        tokio::select! {
            result = &mut worker => break result.unwrap_or_else(|e|Err(e.to_string())),
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => if cancel.load(Ordering::Relaxed) {ctl.cancel();}
        }
    };
    let event = match result {
        Ok(()) => Event::Finished {
            id: spec.id,
            elapsed,
            size: completed_size(std::path::Path::new(&output), size),
        },
        Err(error) => Event::Failed {
            id: spec.id,
            error,
            done: size,
            held: vec![],
            permission_denied: false,
        },
    };
    let _ = tx.send(event);
}

fn completed_size(path: &std::path::Path, verified: u64) -> u64 {
    std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .map_or(verified, |metadata| metadata.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn native_transfer_probes_once_and_reports_each_progress_frame() {
        let root = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let library = package.path().join("fixture.so");
        let status = std::process::Command::new("cc")
            .args(["-shared", "-fPIC"])
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../hydra-plugin/tests/fixtures/native.c"),
            )
            .arg("-o")
            .arg(&library)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(
            package.path().join("plugin.wasm"),
            wat::parse_str(
                r#"(module
            (memory (export "memory") 2)
            (func (export "hydra_api") (result i32) i32.const 1)
            (func (export "hydra_alloc") (param i32) (result i32) i32.const 4096)
            (func (export "hydra_call") (param i32 i32 i32 i32) (result i64) i64.const 0))"#,
            )
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            package.path().join("hydra-plugin.toml"),
            format!(
                r#"
            id="example.native"
            name="Native fixture"
            version="1.0.0"
            api=1
            module="plugin.wasm"
            [[native_modules]]
            id="fixture-engine"
            platform="{}"
            module="fixture.so"
            [permissions]
            native=["fixture-engine"]
        "#,
                hya_plugin::native::platform()
            ),
        )
        .unwrap();
        let grants = hya_plugin::package::load_dir(package.path())
            .unwrap()
            .manifest
            .permissions;
        let mut manager = hya_plugin::manager::Manager::open(root.path().into()).unwrap();
        manager.install(package.path(), grants).unwrap();
        drop(manager);
        let mut info = crate::plugins::tests::transfer_plan();
        let transfer = info.plan.transfer.as_mut().unwrap();
        transfer.engine = "fixture-engine".into();
        transfer.files.truncate(1);
        let spec = StartSpec {
            id: 711,
            temp_path: output.path().join("resume").to_string_lossy().into_owned(),
            ..StartSpec::plain()
        };
        let final_path = Arc::new(Mutex::new(
            output.path().join("output").to_string_lossy().into_owned(),
        ));
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        run_transfer(
            &spec,
            "example.native",
            (**transfer).clone(),
            None,
            &Arc::new(AtomicBool::new(false)),
            &final_path,
            &sender,
            root.path().into(),
        )
        .await;
        drop(sender);
        let mut probes = 0;
        let mut bytes = Vec::new();
        let mut complete = false;
        while let Some(event) = receiver.recv().await {
            match event {
                Event::Probed { size, .. } => {
                    probes += 1;
                    assert_eq!(size, Some(10));
                }
                Event::Progress { done, .. } => bytes.push(done),
                Event::Finished { size, .. } => {
                    assert_eq!(size, 10);
                    complete = true;
                }
                Event::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
        assert_eq!(probes, 1);
        assert_eq!(bytes, [5, 10]);
        assert!(complete);
    }

    #[test]
    fn native_caps_share_the_budget_and_reserve_bandwidth_for_http() {
        let first = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let second = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut state = NativeRates {
            cap: 100,
            active: 3,
            transfers: [(1, (0, first.clone())), (2, (25, second.clone()))].into(),
        };
        assert_eq!(distribute_native_rates(&state), 41);
        assert_eq!(first.load(Ordering::Relaxed), 34);
        assert_eq!(second.load(Ordering::Relaxed), 25);
        state.cap = 1;
        state.active = 2;
        assert_eq!(distribute_native_rates(&state), 1);
        assert_eq!(first.load(Ordering::Relaxed), 1);
        assert_eq!(second.load(Ordering::Relaxed), u64::MAX - 1);
        state.cap = 0;
        assert_eq!(distribute_native_rates(&state), 0);
        assert_eq!(first.load(Ordering::Relaxed), 0);
        assert_eq!(second.load(Ordering::Relaxed), 25);
    }

    #[test]
    fn completed_directory_retains_verified_payload_size() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("payload");
        std::fs::write(&file, b"payload").unwrap();
        assert_eq!(completed_size(directory.path(), 128_128), 128_128);
        assert_eq!(completed_size(&file, 0), 7);
        assert_eq!(completed_size(&directory.path().join("missing"), 11), 11);
    }

    #[test]
    fn subtitle_sidecars_never_replace_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("sub.vtt");
        std::fs::write(&source, b"WEBVTT").unwrap();
        write_sidecar(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"WEBVTT");
        std::fs::write(&source, b"changed").unwrap();
        assert!(write_sidecar(&source, &destination).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"WEBVTT");
    }
    #[tokio::test]
    async fn gui_engine_executes_installed_plan_saves_sidecar_and_cleans_failed_tracks() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let root = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    loop {
                        let mut request = Vec::new();
                        let mut byte = [0];
                        while !request.ends_with(b"\r\n\r\n") {
                            if socket.read(&mut byte).await.unwrap_or(0) == 0 {
                                break;
                            }
                            request.push(byte[0]);
                        }
                        if request.is_empty() {
                            break;
                        }
                        let request = String::from_utf8_lossy(&request);
                        assert!(!request.to_ascii_lowercase().contains("\r\nrange:"));
                        let body = if request.contains("/sub ") {
                            b"WEBVTT\n".as_slice()
                        } else {
                            b"GUI plugin bytes".as_slice()
                        };
                        let head =
                            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                        if socket.write_all(head.as_bytes()).await.is_err() {
                            break;
                        }
                        if !request.starts_with("HEAD ") && socket.write_all(body).await.is_err() {
                            break;
                        }
                        if request.to_ascii_lowercase().contains("connection: close") {
                            break;
                        }
                    }
                });
            }
        });
        let plan: hya_plugin_api::Plan=serde_json::from_value(serde_json::json!({"id":"gui","title":"GUI file","tracks":[
            {"id":"file","kind":"audio","container":"bin","ranges":"deny","sources":[{"url":format!("http://{address}/file")}]},
            {"id":"sub","kind":"subtitle","language":"en","container":"vtt","ranges":"deny","sources":[{"url":format!("http://{address}/sub")}]}
        ]})).unwrap();
        let reply = serde_json::to_vec(&hya_plugin_api::Resolve::Plan(plan.clone())).unwrap();
        let encoded = reply
            .iter()
            .map(|b| format!("\\{b:02x}"))
            .collect::<String>();
        let module=wat::parse_str(format!(r#"(module
          (memory (export "memory") 4) (global $heap (mut i32) (i32.const 10000))
          (data (i32.const 128) "{encoded}")
          (func (export "hydra_api") (result i32) i32.const 1)
          (func (export "hydra_alloc") (param $n i32) (result i32) (local $p i32) global.get $heap local.set $p global.get $heap local.get $n i32.add global.set $heap local.get $p)
          (func (export "hydra_call") (param i32 i32 i32 i32) (result i64) i64.const {}))"#,hya_plugin_api::abi::pack(128,reply.len() as u32))).unwrap();
        std::fs::write(package.path().join("plugin.wasm"), module).unwrap();
        std::fs::write(package.path().join("hydra-plugin.toml"),"id='test.gui'\nname='GUI fixture'\nversion='1.0.0'\napi=1\nmodule='plugin.wasm'\nhooks=['resolve']\nclaims=['https://example.com/*']\n[permissions]\nsources=['127.0.0.1']\n").unwrap();
        let mut manager = hya_plugin::manager::Manager::open(root.path().into()).unwrap();
        let manifest = hya_plugin::manager::Manager::inspect(package.path())
            .unwrap()
            .manifest;
        manager
            .install(package.path(), manifest.permissions)
            .unwrap();
        drop(manager);
        let mut spec = StartSpec::plain();
        spec.id = 99;
        spec.url = "https://example.com/file".into();
        spec.proxy = crate::model::ProxyChoice::Direct;
        spec.temp_path = output
            .path()
            .join("transfer.part")
            .to_string_lossy()
            .into_owned();
        spec.final_path = output
            .path()
            .join("result.bin")
            .to_string_lossy()
            .into_owned();
        spec.plugin_plan = Some(crate::plugins::PlanInfo {
            plugin: "test.gui".into(),
            plan,
            preferences: Preferences {
                include_files: true,
                subtitle_languages: vec!["en".into()],
                ..Default::default()
            },
        });
        let final_path = Arc::new(Mutex::new(spec.final_path.clone()));
        let pace = Pace::pair(
            Arc::new(super::super::RateLimiter::unlimited()),
            Arc::new(super::super::RateLimiter::unlimited()),
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        assert!(run_with_root(&spec, &cancel, &pace, &final_path, &tx, root.path().into()).await);
        let mut finished = false;
        while let Ok(event) = events.try_recv() {
            match event {
                Event::Finished { size, .. } => {
                    assert_eq!(size, 16);
                    finished = true;
                }
                Event::Probed { file_name, .. } => assert!(
                    file_name.is_none(),
                    "a refreshed title must preserve the queued output path"
                ),
                Event::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
        assert!(finished);
        assert_eq!(
            std::fs::read(&spec.final_path).unwrap(),
            b"GUI plugin bytes"
        );
        assert_eq!(
            std::fs::read(format!("{}.1-en.vtt", spec.final_path)).unwrap(),
            b"WEBVTT\n"
        );
        let mut pending = spec.clone();
        pending.final_path = output
            .path()
            .join("Playlist item-id.mkv")
            .to_string_lossy()
            .into_owned();
        pending.plugin_plan = Some(crate::plugins::PlanInfo {
            plugin: "test.gui".into(),
            plan: hya_plugin_api::Plan::single(
                "Playlist item",
                hya_plugin_api::Track::file("id", &spec.url),
            ),
            preferences: Preferences {
                audio_only: true,
                ..Default::default()
            },
        });
        let pending_path = Arc::new(Mutex::new(pending.final_path.clone()));
        assert!(
            run_with_root(
                &pending,
                &cancel,
                &pace,
                &pending_path,
                &tx,
                root.path().into()
            )
            .await
        );
        let actual = output.path().join("Playlist item-id.bin");
        assert_eq!(std::fs::read(&actual).unwrap(), b"GUI plugin bytes");
        assert_eq!(*pending_path.lock().unwrap(), actual.to_string_lossy());
        assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(event, Event::Probed { file_name: Some(name), .. } if name == "Playlist item-id.bin")));
        assert!(
            run_with_root(
                &pending,
                &cancel,
                &pace,
                &pending_path,
                &tx,
                root.path().into()
            )
            .await
        );
        assert!(std::iter::from_fn(|| events.try_recv().ok()).any(
            |event| matches!(event, Event::Failed { error, .. } if error.contains("already exists"))
        ));
        std::fs::remove_file(actual).unwrap();
        std::fs::remove_file(&spec.final_path).unwrap();
        assert!(run_with_root(&spec, &cancel, &pace, &final_path, &tx, root.path().into()).await);
        assert!(!std::path::Path::new(&spec.final_path).exists());
        assert!(std::iter::from_fn(|| events.try_recv().ok())
            .any(|e| matches!(e, Event::Failed { .. })));
        assert_eq!(
            std::fs::read_dir(output.path()).unwrap().count(),
            1,
            "failed transfer must clean staged tracks"
        );
        cancel.store(true, Ordering::Relaxed);
        assert!(run_with_root(&spec, &cancel, &pace, &final_path, &tx, root.path().into()).await);
        server.abort();
    }
}
