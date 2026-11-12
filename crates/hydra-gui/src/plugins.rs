// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later
//! Plugin management in Options and resolver activity in the download list.
use crate::app::{App, El};
use crate::i18n::tr;
use crate::{theme, windows};
use hya_plugin::manager::{Installed, Manager};
use hya_plugin_api::{Answers, ErrorCode, FieldKind, Form, Manifest, PluginError, Value};
use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::Task;
use std::path::PathBuf;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PlanInfo {
    pub plugin: String,
    pub plan: hya_plugin_api::Plan,
    pub preferences: hya_plugin_api::Preferences,
}

pub fn file_name(info: &PlanInfo) -> String {
    let plan = &info.plan;
    if plan.transfer.is_some() {
        return hya_net::filename::portable(plan.title.as_deref().unwrap_or(&plan.id))
            .unwrap_or_else(|| "transfer-download".into());
    }
    let selected = hya_plugin_api::select(plan, &info.preferences);
    let mux = plan.assemble == hya_plugin_api::Assemble::Mux
        && [
            hya_plugin_api::TrackKind::Video,
            hya_plugin_api::TrackKind::Audio,
        ]
        .iter()
        .all(|kind| {
            selected
                .tracks
                .iter()
                .any(|&i| plan.tracks[i].kind == *kind)
        });
    let extension = if info.preferences.audio_only && info.preferences.audio_format.is_some() {
        info.preferences.audio_format.as_deref().unwrap()
    } else if mux {
        info.preferences.container.as_deref().unwrap_or("mkv")
    } else {
        selected
            .tracks
            .first()
            .and_then(|&i| plan.tracks[i].container.as_deref())
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric()))
            .unwrap_or("bin")
    };
    let title = hya_net::filename::portable(plan.title.as_deref().unwrap_or(&plan.id))
        .unwrap_or_else(|| "download".into());
    format!("{}.{}", title.trim_matches('.'), extension)
}

pub async fn resolve(
    url: String,
    referer: Option<String>,
    cookies: Option<String>,
    browser_cookies: Option<hya_net::CookieJar>,
    only: Option<String>,
    ctl: std::sync::Arc<hya_plugin::runtime::CallCtl>,
) -> Result<Option<PlanInfo>, String> {
    let root = crate::model::app_dir().join("plugins");
    tokio::task::spawn_blocking(move || {
        let mut manager = Manager::open_with_official(root).map_err(|e| e.to_string())?;
        let mut context = hya_plugin::manager::ResolveContext {
            proxy: crate::proxy::active().plugin_proxy(),
            only,
            ctl,
            input_file: url::Url::parse(&url)
                .ok()
                .filter(|url| url.scheme() == "file")
                .and_then(|url| url.to_file_path().ok()),
            ..Default::default()
        };
        let connector =
            hya_plugin::http::connector(context.proxy.as_ref()).map_err(|e| e.to_string())?;
        context.cookies = resolver_cookies(&url, cookies.as_deref(), browser_cookies);
        let request = hya_plugin_api::ResolveRequest {
            url,
            headers: referer
                .map(|r| [("Referer".into(), r)].into_iter().collect())
                .unwrap_or_default(),
        };
        manager
            .resolve_with_context(
                || connector.clone(),
                request,
                |id| Box::new(Frontend { plugin: id.into() }),
                context,
            )
            .map(|result| {
                result.map(|(plugin, plan)| PlanInfo {
                    plugin,
                    plan,
                    preferences: hya_plugin_api::Preferences {
                        include_files: true,
                        container: Some("mkv".into()),
                        ..Default::default()
                    },
                })
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn resolver_cookies(
    url: &str,
    cookies: Option<&str>,
    browser_cookies: Option<hya_net::CookieJar>,
) -> hya_net::CookieJar {
    if let Some(jar) = browser_cookies {
        return jar;
    }
    let mut jar = hya_net::CookieJar::new();
    if let (Some(cookies), Ok(parsed)) = (cookies, crate::engine::parse_url(url)) {
        jar.add_pairs(cookies, &parsed.host);
    }
    jar
}

#[derive(Clone, Debug)]
pub struct Prompt {
    pub plugin: String,
    pub form: Form,
    pub reply: std::sync::mpsc::SyncSender<Result<Answers, PluginError>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detail {
    Settings(String),
    Info(String),
    Logs(String),
}

#[derive(Debug, Default)]
pub struct Page {
    pub detail: Option<Detail>,
    pub prompt: Option<Prompt>,
    pub answers: std::collections::BTreeMap<String, String>,
    pub edits: std::collections::BTreeMap<(String, String), String>,
    pub installed: Vec<Installed>,
    pub path: String,
    pub review: Option<Manifest>,
    pub prepared: Option<hya_plugin::distribution::Prepared>,
    pub review_signing: hya_plugin::package::Signing,
    pub busy: bool,
    pub permission_details: bool,
    pub pending_source: Option<String>,
    pub error: Option<String>,
    pub logs: String,
    pub welcome: Option<(String, String)>,
    pub index_url: String,
    pub index_key: String,
    pub indexes: Vec<hya_plugin::distribution::IndexSource>,
    pub updates: Vec<hya_plugin::distribution::Entry>,
}
#[derive(Debug, Clone)]
pub enum Message {
    Loaded(Result<(Vec<Installed>, Vec<hya_plugin::distribution::IndexSource>), String>),
    PermissionDetails(bool),
    IndexUrl(String),
    IndexKey(String),
    IndexAdd,
    IndexRemove(String),
    CheckUpdates,
    CheckUpdate(String),
    UpdateLoaded(String, Result<Vec<hya_plugin::distribution::Entry>, String>),
    UpdatesLoaded(Result<Vec<hya_plugin::distribution::Entry>, String>),
    ReviewUpdate(hya_plugin::distribution::Entry),
    LogsLoaded(String, Result<String, String>),
    Check(String),
    Detail(Option<Detail>),
    Answer(String, String),
    SubmitPrompt(bool),
    Edit(String, String, String),
    SaveSetting(String, String, String),
    Permission(String, String, bool),
    Rollback(String),
    Path(String),
    Review,
    Reviewed(
        Box<
            Result<
                (
                    Manifest,
                    hya_plugin::package::Signing,
                    hya_plugin::distribution::Prepared,
                ),
                String,
            >,
        >,
    ),
    Install,
    Enable(String, bool),
    Remove(String),
    Move(String, bool),
    Finished(Result<(), String>),
    Installed(Result<(), String>),
}
fn wrap(m: Message) -> crate::app::Message {
    crate::app::Message::Plugin(m)
}
fn manager() -> Result<Manager, String> {
    Manager::open_with_official(crate::model::app_dir().join("plugins")).map_err(|e| e.to_string())
}
pub fn load() -> Task<crate::app::Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(|| {
                let manager = manager()?;
                let indexes =
                    hya_plugin::distribution::sources(&crate::model::app_dir().join("plugins"))
                        .map_err(|e| e.to_string())?;
                Ok((manager.list().to_vec(), indexes))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        |r| wrap(Message::Loaded(r)),
    )
}
fn reset_scroll() -> Task<crate::app::Message> {
    iced::widget::operation::scroll_to("plugin-page", scrollable::AbsoluteOffset::<f32>::default())
}

fn resume_source(page: &mut Page) -> Task<crate::app::Message> {
    page.pending_source
        .take()
        .map_or_else(Task::none, |source| {
            Task::done(crate::app::Message::InstallPluginSource(source))
        })
}

fn check_updates(page: &mut Page, id: Option<String>) -> Task<crate::app::Message> {
    if page.busy {
        return Task::none();
    }
    page.busy = true;
    page.error = None;
    let response_id = id.clone();
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                let manager = manager()?;
                hya_plugin::distribution::updates(
                    &crate::model::app_dir().join("plugins"),
                    manager.list(),
                    id.as_deref(),
                )
                .map_err(|error| error.to_string())
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()))
        },
        move |result| {
            wrap(match &response_id {
                Some(id) => Message::UpdateLoaded(id.clone(), result),
                None => Message::UpdatesLoaded(result),
            })
        },
    )
}

pub fn update(app: &mut App, message: Message) -> Task<crate::app::Message> {
    let installed = matches!(&message, Message::Installed(_));
    let page = &mut app.options.plugins;
    match message {
        Message::PermissionDetails(show) => {
            page.permission_details = show;
            Task::none()
        }
        Message::IndexUrl(value) => {
            page.index_url = value;
            Task::none()
        }
        Message::IndexKey(value) => {
            page.index_key = value;
            Task::none()
        }
        Message::UpdatesLoaded(result) => {
            page.busy = false;
            match result {
                Ok(updates) => page.updates = updates,
                Err(error) => page.error = Some(error),
            }
            resume_source(page)
        }
        Message::CheckUpdates => check_updates(page, None),
        Message::CheckUpdate(id) => check_updates(page, Some(id)),
        Message::UpdateLoaded(id, result) => {
            page.busy = false;
            match result {
                Ok(updates) => {
                    page.updates.retain(|entry| entry.id != id);
                    page.updates
                        .extend(updates.into_iter().filter(|entry| entry.id == id));
                }
                Err(error) => page.error = Some(error),
            }
            resume_source(page)
        }
        Message::ReviewUpdate(entry) => {
            if page.busy {
                return Task::none();
            }
            page.path = entry.package.clone();
            page.review = None;
            page.prepared = None;
            page.error = None;
            page.welcome = None;
            page.busy = true;
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let prepared = entry.prepare().map_err(|e| e.to_string())?;
                        let package =
                            Manager::inspect(&prepared.path).map_err(|e| e.to_string())?;
                        Ok((package.manifest, package.signing, prepared))
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                },
                |r| wrap(Message::Reviewed(Box::new(r))),
            )
        }
        Message::Detail(detail) => {
            if let Some(Detail::Logs(id)) = &detail {
                let id = id.clone();
                let response_id = id.clone();
                page.detail = detail;
                page.logs.clear();
                return Task::batch([
                    reset_scroll(),
                    Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                manager()?.logs(&id).map_err(|e| e.to_string())
                            })
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                        },
                        move |r| wrap(Message::LogsLoaded(response_id.clone(), r)),
                    ),
                ]);
            }
            page.detail = detail;
            page.error = None;
            page.edits.clear();
            reset_scroll()
        }
        Message::Edit(id, key, value) => {
            page.edits.insert((id, key), value);
            Task::none()
        }
        Message::Answer(key, value) => {
            page.answers.insert(key, value);
            Task::none()
        }
        Message::SubmitPrompt(accept) => {
            if let Some(prompt) = &page.prompt {
                let result = if accept {
                    form_answers(&prompt.form, &page.answers)
                } else {
                    Err(PluginError::new(ErrorCode::Cancelled, "prompt cancelled"))
                };
                if accept {
                    if let Err(e) = &result {
                        page.error = Some(e.to_string());
                        return Task::none();
                    }
                }
                let _ = prompt.reply.send(result);
            }
            page.prompt = None;
            page.answers.clear();
            Task::none()
        }
        Message::Path(path) => {
            page.path = path;
            page.review = None;
            page.prepared = None;
            Task::none()
        }
        Message::LogsLoaded(id, result) => {
            if page.detail != Some(Detail::Logs(id)) {
                return Task::none();
            }
            match result {
                Ok(logs) => page.logs = logs,
                Err(e) => page.error = Some(e),
            }
            Task::none()
        }
        Message::Loaded(result) => {
            match result {
                Ok((list, indexes)) => {
                    page.updates.retain(|entry| {
                        list.iter()
                            .find(|plugin| plugin.manifest.id == entry.id)
                            .is_some_and(|plugin| {
                                semver::Version::parse(&entry.version).ok()
                                    > semver::Version::parse(&plugin.manifest.version).ok()
                            })
                    });
                    crate::extbus::publish_plugins(&list);
                    page.installed = list;
                    page.indexes = indexes;
                }
                Err(e) => page.error = Some(e),
            };
            Task::none()
        }
        Message::Reviewed(result) => {
            page.busy = false;
            if page.pending_source.is_some() {
                return resume_source(page);
            }
            match *result {
                Ok((manifest, signing, prepared)) => {
                    page.review_signing = signing;
                    page.review = Some(manifest);
                    page.prepared = Some(prepared);
                    return reset_scroll();
                }
                Err(e) => page.error = Some(e),
            };
            Task::none()
        }
        Message::Review => {
            if page.busy {
                return Task::none();
            }
            page.busy = true;
            page.error = None;
            let source = page.path.clone();
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        let prepared = hya_plugin::distribution::Prepared::new(&source, None)
                            .map_err(|e| e.to_string())?;
                        let package =
                            Manager::inspect(&prepared.path).map_err(|e| e.to_string())?;
                        Ok((package.manifest, package.signing, prepared))
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                },
                |r| wrap(Message::Reviewed(Box::new(r))),
            )
        }
        Message::Finished(result) | Message::Installed(result) => {
            page.busy = false;
            match result {
                Ok(()) => {
                    if installed {
                        page.welcome = page.review.as_ref().and_then(|manifest| {
                            manifest
                                .welcome
                                .clone()
                                .map(|text| (manifest.name.clone(), text))
                        });
                        page.review = None;
                    }
                    page.error = None;
                }
                Err(e) => page.error = Some(e),
            };
            Task::batch([load(), resume_source(page)])
        }
        operation => {
            if page.busy {
                return Task::none();
            }
            if let Message::SaveSetting(id, key, _) = &operation {
                page.edits.remove(&(id.clone(), key.clone()));
            }
            let installing = matches!(&operation, Message::Install);
            let index_url = page.index_url.clone();
            let index_key =
                (!page.index_key.trim().is_empty()).then(|| page.index_key.trim().to_string());
            let prepared = page.prepared.clone();
            let path = prepared
                .as_ref()
                .map(|p| p.path.clone())
                .unwrap_or_else(|| PathBuf::from(&page.path));
            let reviewed = page.review.clone();
            page.busy = true;
            page.error = None;
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || -> Result<(), String> {
                        let _prepared = prepared;
                        let mut manager = manager()?;
                        match operation {
                            Message::IndexAdd => {
                                hya_plugin::distribution::add(
                                    &crate::model::app_dir().join("plugins"),
                                    hya_plugin::distribution::IndexSource {
                                        url: index_url,
                                        key: index_key,
                                    },
                                )
                                .map_err(|e| e.to_string())?;
                            }
                            Message::IndexRemove(url) => hya_plugin::distribution::remove(
                                &crate::model::app_dir().join("plugins"),
                                &url,
                            )
                            .map_err(|e| e.to_string())?,
                            Message::Install => {
                                let manifest =
                                    reviewed.ok_or("review permissions before installing")?;
                                let package = Manager::inspect(&path).map_err(|e| e.to_string())?;
                                if package.manifest != manifest {
                                    return Err("package changed; review it again".into());
                                }
                                manager
                                    .install_with_publisher_consent(
                                        &path,
                                        manifest.permissions,
                                        true,
                                    )
                                    .map_err(|e| e.to_string())?;
                            }
                            Message::SaveSetting(id, key, raw) => {
                                let field = manager
                                    .list()
                                    .iter()
                                    .find(|p| p.manifest.id == id)
                                    .and_then(|p| p.manifest.settings.iter().find(|f| f.key == key))
                                    .ok_or("unknown setting")?;
                                let secret = field.kind == FieldKind::Secret;
                                let value = match field.kind {
                                    FieldKind::Bool | FieldKind::Checkbox => {
                                        Value::Bool(raw.parse().map_err(|_| "enter true or false")?)
                                    }
                                    FieldKind::Number => {
                                        Value::Number(raw.parse().map_err(|_| "enter a number")?)
                                    }
                                    _ => Value::Text(raw),
                                };
                                if secret {
                                    manager
                                        .set_secret(
                                            &id,
                                            &key,
                                            value.as_text().unwrap_or_default().into(),
                                        )
                                        .map_err(|e| e.to_string())?;
                                } else {
                                    manager.set(&id, &key, value).map_err(|e| e.to_string())?;
                                }
                                manager
                                    .check(
                                        &id,
                                        hya_net::tls::TlsCapableConnector::new()
                                            .map_err(|e| e.to_string())?,
                                        Box::new(Frontend { plugin: id.clone() }),
                                    )
                                    .map_err(|e| e.to_string())?;
                            }
                            Message::Check(id) => manager
                                .check(
                                    &id,
                                    hya_net::tls::TlsCapableConnector::new()
                                        .map_err(|e| e.to_string())?,
                                    Box::new(Frontend { plugin: id.clone() }),
                                )
                                .map_err(|e| e.to_string())?,
                            Message::Permission(id, capability, grant) => manager
                                .permission(&id, &capability, grant)
                                .map_err(|e| e.to_string())?,
                            Message::Rollback(id) => {
                                manager.rollback(&id).map_err(|e| e.to_string())?
                            }
                            Message::Enable(id, enabled) => {
                                manager.enable(&id, enabled).map_err(|e| e.to_string())?
                            }
                            Message::Remove(id) => {
                                manager.remove(&id).map_err(|e| e.to_string())?
                            }
                            Message::Move(id, up) => {
                                let mut ids: Vec<String> = manager
                                    .list()
                                    .iter()
                                    .map(|p| p.manifest.id.clone())
                                    .collect();
                                if let Some(i) = ids.iter().position(|p| p == &id) {
                                    let next = if up {
                                        i.saturating_sub(1)
                                    } else {
                                        (i + 1).min(ids.len() - 1)
                                    };
                                    ids.swap(i, next);
                                    manager.order(&ids).map_err(|e| e.to_string())?;
                                }
                            }
                            _ => {}
                        }
                        Ok(())
                    })
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()))
                },
                move |r| {
                    wrap(if installing {
                        Message::Installed(r)
                    } else {
                        Message::Finished(r)
                    })
                },
            )
        }
    }
}
pub fn view(app: &App) -> El<'_> {
    let p = &app.options.plugins;
    let mut body = column![].spacing(14).padding([0, 8]);
    if let Some((name, welcome)) = &p.welcome {
        body = body.push(
            container(
                column![
                    text(format!("{} — {}", tr("Getting started"), name))
                        .size(theme::FONT_SIZE + 2.0),
                    text(welcome)
                        .size(theme::FONT_SIZE)
                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
                ]
                .spacing(8),
            )
            .padding(12)
            .width(iced::Length::Fill)
            .style(theme::panel),
        );
    }
    if let Some(prompt) = &p.prompt {
        body = body.push(
            text(format!(
                "{} — {}",
                prompt.plugin,
                prompt.form.title.as_deref().unwrap_or("Plugin input")
            ))
            .size(theme::FONT_SIZE),
        );
        for field in &prompt.form.fields {
            let value = p.answers.get(&field.key).map(String::as_str).unwrap_or("");
            body = body.push(field_control(field, value, EditTarget::Prompt));
        }
        body = body.push(
            row![
                button(text(tr("Continue")).size(theme::FONT_SIZE))
                    .padding([5, 10])
                    .style(theme::btn)
                    .on_press(wrap(Message::SubmitPrompt(true))),
                button(text(tr("Cancel")).size(theme::FONT_SIZE))
                    .padding([5, 10])
                    .style(theme::btn)
                    .on_press(wrap(Message::SubmitPrompt(false)))
            ]
            .spacing(8),
        );
    } else if let Some(detail) = &p.detail {
        let id = match detail {
            Detail::Settings(id) | Detail::Info(id) | Detail::Logs(id) => id,
        };
        body = body.push(
            button(text(tr("Back to plugins")).size(theme::FONT_SIZE))
                .padding([5, 10])
                .style(theme::btn)
                .on_press(wrap(Message::Detail(None))),
        );
        if let Some(installed) = p.installed.iter().find(|x| &x.manifest.id == id) {
            body = body.push(text(&installed.manifest.name).size(crate::theme::FONT_SIZE + 2.0));
            match detail {
                Detail::Settings(_) => {
                    body = body.push(
                        button(text(tr("View plugin logs")).size(theme::FONT_SIZE))
                            .padding([5, 10])
                            .style(theme::btn)
                            .on_press(wrap(Message::Detail(Some(Detail::Logs(id.clone()))))),
                    );
                    if installed.manifest.settings.is_empty() {
                        body = body
                            .push(text(tr("This plugin has no settings.")).size(theme::FONT_SIZE));
                    }
                    for field in &installed.manifest.settings {
                        let value = p
                            .edits
                            .get(&(id.clone(), field.key.clone()))
                            .cloned()
                            .unwrap_or_else(|| {
                                installed
                                    .settings
                                    .get(&field.key)
                                    .or(field.default.as_ref())
                                    .map(value_text)
                                    .unwrap_or_default()
                            });
                        body = body.push(
                            container(
                                column![
                                    field_control(field, &value, EditTarget::Settings(id.clone())),
                                    button(text(tr("Save setting")).size(theme::FONT_SIZE))
                                        .padding([5, 10])
                                        .style(theme::btn)
                                        .on_press_maybe((!p.busy).then(|| {
                                            wrap(Message::SaveSetting(
                                                id.clone(),
                                                field.key.clone(),
                                                value,
                                            ))
                                        }),)
                                ]
                                .spacing(8),
                            )
                            .padding(12)
                            .width(iced::Length::Fill)
                            .style(theme::panel),
                        );
                    }
                }
                Detail::Logs(_) => {
                    body = body.push(
                        text(if p.logs.is_empty() {
                            tr("No plugin log entries yet.")
                        } else {
                            p.logs.clone()
                        })
                        .size(theme::FONT_SIZE),
                    );
                }
                Detail::Info(_) => {
                    body = body.push(
                        row![
                            button(text(tr("Check plugin")).size(theme::FONT_SIZE))
                                .padding([5, 10])
                                .style(theme::btn)
                                .on_press_maybe(
                                    (!p.busy).then(|| wrap(Message::Check(id.clone())))
                                ),
                            button(text(tr("View plugin logs")).size(theme::FONT_SIZE))
                                .padding([5, 10])
                                .style(theme::btn)
                                .on_press(wrap(Message::Detail(Some(Detail::Logs(id.clone())))))
                        ]
                        .spacing(8),
                    );
                    if let Some(welcome) = &installed.manifest.welcome {
                        body = body.push(
                            container(
                                column![
                                    text(tr("Getting started")).size(theme::FONT_SIZE + 2.0),
                                    text(welcome)
                                        .size(theme::FONT_SIZE)
                                        .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                                ]
                                .spacing(8),
                            )
                            .padding(12)
                            .width(iced::Length::Fill)
                            .style(theme::panel),
                        );
                    }
                    body = body.push(manifest_info(
                        &installed.manifest,
                        Some(&installed.directory),
                        &installed.signing,
                    ));
                    body = body.push(text(tr("Permissions")).size(theme::FONT_SIZE + 2.0));
                    for (capability, granted) in capabilities(installed) {
                        body = body.push(
                            row![
                                column![
                                    text(capability.clone()).size(theme::FONT_SIZE),
                                    text(if granted {
                                        tr("Granted")
                                    } else {
                                        tr("Not granted")
                                    })
                                    .size(theme::FONT_SIZE - 1.0)
                                    .color(theme::dim_text(&iced::Theme::Light))
                                ]
                                .spacing(3)
                                .width(iced::Length::Fill),
                                button(
                                    text(if granted { tr("Revoke") } else { tr("Grant") })
                                        .size(theme::FONT_SIZE)
                                )
                                .padding([5, 10])
                                .style(theme::btn)
                                .on_press_maybe((!p.busy).then(|| wrap(Message::Permission(
                                    id.clone(),
                                    capability,
                                    !granted
                                ))))
                            ]
                            .spacing(8),
                        );
                    }
                    if installed.previous.is_some() {
                        body = body.push(
                            button(text(tr("Roll back version")).size(theme::FONT_SIZE))
                                .padding([5, 10])
                                .style(theme::btn)
                                .on_press_maybe(
                                    (!p.busy).then(|| wrap(Message::Rollback(id.clone()))),
                                ),
                        );
                    }
                }
            }
        }
    } else {
        body = body.push(
            text(tr(
                "Install a .hyaplugin package, HTTPS URL or development folder.",
            ))
            .size(theme::FONT_SIZE),
        );
        body = body.push(
            row![
                text_input(&tr("Package path or HTTPS URL"), &p.path)
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(iced::Length::Fill)
                    .on_input(|v| wrap(Message::Path(v))),
                button(text(tr("Review permissions")).size(theme::FONT_SIZE))
                    .padding([5, 10])
                    .style(theme::btn)
                    .on_press_maybe((!p.busy).then_some(wrap(Message::Review)))
            ]
            .spacing(8),
        );
        if let Some(manifest) = &p.review {
            let mut review = column![
                text(&manifest.name).size(theme::FONT_SIZE + 2.0),
                manifest_info(manifest, None, &p.review_signing),
                text(tr("Permissions")).size(theme::FONT_SIZE + 2.0),
            ]
            .spacing(10);
            if p.prepared.as_ref().is_some_and(|p| p.remote()) && manifest.publisher_key.is_none() {
                review = review.push(
                    text(tr(
                        "This package is unsigned. Its checksum does not identify the publisher.",
                    ))
                    .size(theme::FONT_SIZE),
                );
            }
            if let Some(old) = p
                .installed
                .iter()
                .find(|old| old.manifest.id == manifest.id)
            {
                if old.manifest.publisher_key != manifest.publisher_key {
                    let key = |key: &Option<String>| {
                        key.as_deref()
                            .map(hya_plugin::package::key_fingerprint)
                            .unwrap_or_else(|| tr("Unsigned"))
                    };
                    review = review.push(
                        text(format!(
                            "{}: {} → {}",
                            tr("Publisher changed"),
                            key(&old.manifest.publisher_key),
                            key(&manifest.publisher_key)
                        ))
                        .size(theme::FONT_SIZE),
                    );
                }
            }
            for line in permission_summary(&manifest.permissions) {
                review = review.push(text(line).size(theme::FONT_SIZE));
            }
            review = review.push(
                button(
                    text(if p.permission_details {
                        "Hide permission details"
                    } else {
                        "Show permission details"
                    })
                    .size(theme::FONT_SIZE),
                )
                .padding([5, 10])
                .style(theme::btn)
                .on_press(wrap(Message::PermissionDetails(!p.permission_details))),
            );
            if p.permission_details {
                let details = column(
                    hya_plugin::consent(&manifest.permissions)
                        .into_iter()
                        .map(|line| text(line).size(theme::FONT_SIZE).into()),
                )
                .spacing(6);
                review = review.push(scrollable(details).height(180));
            }
            review = review.push(
                button(text(tr("Accept permissions and install")).size(theme::FONT_SIZE))
                    .padding([5, 10])
                    .style(theme::btn)
                    .on_press_maybe((!p.busy).then_some(wrap(Message::Install))),
            );
            body = body.push(
                container(review)
                    .padding(12)
                    .width(iced::Length::Fill)
                    .style(theme::panel),
            );
        }
        body = body.push(text(tr("Installed plugins")).size(theme::FONT_SIZE + 2.0));
        if p.installed.is_empty() {
            body = body.push(text(tr("No plugins installed.")).size(theme::FONT_SIZE));
        }
        for (index, installed) in p.installed.iter().enumerate() {
            let id = &installed.manifest.id;
            let available = p.updates.iter().find(|entry| entry.id == *id);
            let mut description = column![
                text(format!(
                    "{}{}",
                    installed.manifest.name,
                    if installed.dev { " (dev)" } else { "" }
                ))
                .size(theme::FONT_SIZE),
                text(format!(
                    "{} · {} · {}",
                    installed.manifest.version,
                    installed.manifest.id,
                    installed.manifest.author.as_deref().unwrap_or("—")
                ))
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::dim_text(&iced::Theme::Light)),
            ]
            .spacing(3)
            .width(iced::Length::Fill);
            if let Some(entry) = available {
                description = description.push(
                    container(
                        text(format!(
                            "{} · {}",
                            tr("Plugin update available"),
                            entry.version
                        ))
                        .size(theme::FONT_SIZE - 1.0),
                    )
                    .padding([2, 6])
                    .style(|_| container::Style {
                        background: Some(theme::success_text().scale_alpha(0.12).into()),
                        text_color: Some(theme::success_text()),
                        border: iced::Border {
                            color: theme::success_text().scale_alpha(0.4),
                            width: 1.0,
                            radius: 4.0.into(),
                        },
                        ..Default::default()
                    }),
                );
            }
            let update_button = match available {
                Some(entry) => icon_button(
                    crate::icons::resume(!p.busy),
                    tr("Install plugin update"),
                    (!p.busy).then(|| Message::ReviewUpdate(entry.clone())),
                ),
                None => icon_button(
                    crate::icons::refresh(!p.busy),
                    tr("Check for updates"),
                    (!p.busy).then(|| Message::CheckUpdate(id.clone())),
                ),
            };
            body = body.push(
                container(
                    row![
                        windows::check(installed.enabled, String::new()).on_toggle({
                            let id = id.clone();
                            move |enabled| wrap(Message::Enable(id.clone(), enabled))
                        }),
                        description,
                        update_button,
                        icon_button(
                            crate::icons::options(!p.busy),
                            tr("Plugin settings"),
                            (!p.busy).then(|| Message::Detail(Some(Detail::Settings(id.clone()))))
                        ),
                        icon_button(
                            crate::icons::delete(!p.busy),
                            tr("Remove plugin"),
                            (!p.busy).then(|| Message::Remove(id.clone()))
                        ),
                        icon_button(
                            crate::icons::info(),
                            tr("Plugin information"),
                            Some(Message::Detail(Some(Detail::Info(id.clone()))))
                        ),
                        button("↑")
                            .padding([5, 10])
                            .style(theme::btn)
                            .on_press_maybe(
                                (!p.busy && index > 0)
                                    .then(|| wrap(Message::Move(id.clone(), true)))
                            ),
                        button("↓")
                            .padding([5, 10])
                            .style(theme::btn)
                            .on_press_maybe(
                                (!p.busy && index + 1 < p.installed.len())
                                    .then(|| wrap(Message::Move(id.clone(), false)))
                            ),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                )
                .padding(10)
                .width(iced::Length::Fill)
                .style(theme::panel),
            );
        }
    }
    if p.busy {
        body = body.push(text(tr("Working…")).size(theme::FONT_SIZE));
    }
    if p.detail.is_none() && p.prompt.is_none() && p.review.is_none() {
        body = body
            .push(text(tr("Plugin indexes")).size(theme::FONT_SIZE + 2.0))
            .push(
                row![
                    text_input(&tr("Index HTTPS URL"), &p.index_url)
                        .size(theme::FONT_SIZE)
                        .style(theme::input)
                        .width(iced::Length::Fill)
                        .on_input(|v| wrap(Message::IndexUrl(v))),
                    button(text(tr("Add index")).size(theme::FONT_SIZE))
                        .padding([5, 10])
                        .style(theme::btn)
                        .on_press_maybe((!p.busy).then_some(wrap(Message::IndexAdd)))
                ]
                .spacing(8),
            )
            .push(
                text_input(&tr("Optional trusted signing key"), &p.index_key)
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(iced::Length::Fill)
                    .on_input(|v| wrap(Message::IndexKey(v))),
            );
        for source in &p.indexes {
            let index = row![text(source.url.clone())
                .size(theme::FONT_SIZE)
                .width(iced::Length::Fill),]
            .spacing(8);
            body = body.push(if source.is_default() {
                index.push(text(tr("Default catalog (read-only)")).size(theme::FONT_SIZE))
            } else {
                index.push(
                    button(text(tr("Remove index")).size(theme::FONT_SIZE))
                        .padding([5, 10])
                        .style(theme::btn)
                        .on_press_maybe(
                            (!p.busy).then(|| wrap(Message::IndexRemove(source.url.clone()))),
                        ),
                )
            });
        }
        body = body.push(
            button(text(tr("Check plugin updates")).size(theme::FONT_SIZE))
                .padding([5, 10])
                .style(theme::btn)
                .on_press_maybe((!p.busy).then_some(wrap(Message::CheckUpdates))),
        );
    }
    if let Some(error) = &p.error {
        body = body.push(
            text(error)
                .size(theme::FONT_SIZE)
                .color(crate::theme::error_text()),
        );
    }
    scrollable(body)
        .id("plugin-page")
        .height(iced::Length::Fill)
        .into()
}

fn permission_summary(permissions: &hya_plugin_api::Permissions) -> Vec<String> {
    let mut lines = Vec::new();
    for (label, hosts) in [
        ("Connects to", &permissions.http),
        ("Downloads from", &permissions.sources),
        ("Uses browser cookies for", &permissions.cookies),
    ] {
        if !hosts.is_empty() {
            lines.push(format!("{label}: {}", hosts.join(", ")));
        }
    }
    let mut programs = std::collections::BTreeMap::new();
    for entry in &permissions.exec {
        *programs.entry(entry.program.as_str()).or_insert(0usize) += 1;
    }
    for (program, count) in programs {
        lines.push(format!(
            "Runs {program} using {count} permitted argument patterns."
        ));
    }
    for id in &permissions.native {
        lines.push(format!(
            "Loads native module {id} with full operating-system access."
        ));
    }
    if permissions.data {
        lines.push("Keeps files in its own folder.".into());
    }
    if permissions.exec_from_data {
        lines.push("Can download programs and run them.".into());
    }
    lines
}

fn manifest_info<'a>(
    manifest: &'a Manifest,
    directory: Option<&std::path::Path>,
    signing: &hya_plugin::package::Signing,
) -> El<'a> {
    let mut rows = vec![
        (
            Some(tr("Plugin ID")),
            text(&manifest.id).size(theme::FONT_SIZE).into(),
        ),
        (
            Some(tr("Version")),
            text(&manifest.version).size(theme::FONT_SIZE).into(),
        ),
    ];
    rows.push((
        Some(tr("Author")),
        text(manifest.author.as_deref().unwrap_or("—"))
            .size(theme::FONT_SIZE)
            .into(),
    ));
    let signature: El<'a> = match signing {
        hya_plugin::package::Signing::Unsigned => {
            text(tr("Unsigned")).size(theme::FONT_SIZE).into()
        }
        hya_plugin::package::Signing::Verified { fingerprint } => column![
            container(
                text(format!("✓ {}", tr("Signed (verified)")))
                    .size(theme::FONT_SIZE)
                    .color(theme::success_text())
            )
            .padding([3, 8])
            .style(theme::panel),
            text(format!(
                "{}: {fingerprint}",
                tr("Publisher fingerprint (SHA-256)")
            ))
            .size(theme::FONT_SIZE - 1.0)
        ]
        .spacing(5)
        .into(),
    };
    rows.push((Some(tr("Signature")), signature));
    if let Some(license) = &manifest.license {
        rows.push((
            Some(tr("License")),
            text(license).size(theme::FONT_SIZE).into(),
        ));
    }
    if let Some(homepage) = &manifest.homepage {
        rows.push((
            Some(tr("Homepage")),
            text(homepage)
                .size(theme::FONT_SIZE)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                .into(),
        ));
    }
    if let Some(directory) = directory {
        rows.push((
            Some(tr("Location")),
            text(directory.display().to_string())
                .size(theme::FONT_SIZE)
                .wrapping(iced::widget::text::Wrapping::WordOrGlyph)
                .into(),
        ));
    }
    windows::label_column(rows, 90.0, 8.0)
}

fn icon_button(
    icon: iced::widget::svg::Handle,
    label: String,
    action: Option<Message>,
) -> El<'static> {
    iced::widget::tooltip(
        button(iced::widget::svg(icon).width(22).height(22))
            .style(theme::btn)
            .padding(5)
            .on_press_maybe(action.map(wrap)),
        text(label).size(theme::FONT_SIZE),
        iced::widget::tooltip::Position::Bottom,
    )
    .into()
}

#[derive(Clone)]
enum EditTarget {
    Prompt,
    Settings(String),
}
impl EditTarget {
    fn message(&self, key: String, value: String) -> crate::app::Message {
        wrap(match self {
            Self::Prompt => Message::Answer(key, value),
            Self::Settings(id) => Message::Edit(id.clone(), key, value),
        })
    }
}
fn value_text(value: &Value) -> String {
    match value {
        Value::Text(s) => s.clone(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
    }
}
fn field_control<'a>(field: &'a hya_plugin_api::Field, value: &str, target: EditTarget) -> El<'a> {
    let key = field.key.clone();
    let mut form = column![].spacing(6);
    if !matches!(field.kind, FieldKind::Bool | FieldKind::Checkbox) {
        form = form.push(text(&field.label).size(theme::FONT_SIZE));
    }
    match field.kind {
        FieldKind::Bool | FieldKind::Checkbox => {
            form = form.push(
                windows::check(value == "true", field.label.clone())
                    .on_toggle(move |v| target.message(key.clone(), v.to_string())),
            )
        }
        FieldKind::Choice => {
            form = form.push(
                iced::widget::pick_list(
                    field.options.clone(),
                    (!value.is_empty()).then(|| value.to_owned()),
                    move |v| target.message(key.clone(), v),
                )
                .text_size(theme::FONT_SIZE)
                .padding([5, 8])
                .style(theme::picker)
                .menu_style(theme::picker_menu)
                .width(iced::Length::Fill),
            )
        }
        FieldKind::Radio => {
            let selected = field.options.iter().position(|s| s == value);
            for (index, option) in field.options.iter().enumerate() {
                let key = key.clone();
                let target = target.clone();
                let choice = option.clone();
                form = form.push(
                    iced::widget::radio(option.clone(), index, selected, move |_| {
                        target.message(key.clone(), choice.clone())
                    })
                    .size(15)
                    .text_size(theme::FONT_SIZE)
                    .style(theme::radio),
                );
            }
        }
        FieldKind::Number => {
            let number = value.parse::<f64>().unwrap_or(0.0);
            let less = target.message(key.clone(), (number - 1.0).to_string());
            let more = target.message(key.clone(), (number + 1.0).to_string());
            form = form.push(
                row![
                    button("−")
                        .padding([5, 10])
                        .style(theme::btn)
                        .on_press(less),
                    text_input("0", value)
                        .size(theme::FONT_SIZE)
                        .style(theme::input)
                        .width(iced::Length::Fill)
                        .on_input(move |v| target.message(key.clone(), v)),
                    button("+")
                        .padding([5, 10])
                        .style(theme::btn)
                        .on_press(more)
                ]
                .spacing(6),
            );
        }
        _ => {
            form = form.push(
                text_input("", value)
                    .size(theme::FONT_SIZE)
                    .style(theme::input)
                    .width(iced::Length::Fill)
                    .secure(field.kind == FieldKind::Secret)
                    .on_input(move |v| target.message(key.clone(), v)),
            )
        }
    }
    if let Some(help) = &field.help {
        form = form.push(
            text(help)
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::dim_text(&iced::Theme::Light)),
        );
    }
    form.into()
}
fn capabilities(installed: &Installed) -> Vec<(String, bool)> {
    let mut result = Vec::new();
    for (kind, declared, granted) in [
        (
            "http",
            &installed.manifest.permissions.http,
            &installed.grants.http,
        ),
        (
            "sources",
            &installed.manifest.permissions.sources,
            &installed.grants.sources,
        ),
        (
            "cookies",
            &installed.manifest.permissions.cookies,
            &installed.grants.cookies,
        ),
    ] {
        for host in declared {
            result.push((format!("{kind}:{host}"), granted.contains(host)));
        }
    }
    for id in &installed.manifest.permissions.native {
        result.push((format!("native:{id}"), installed.grants.native.contains(id)));
    }
    for entry in &installed.manifest.permissions.exec {
        let capability = format!("exec:{}", entry.program);
        if !result.iter().any(|(c, _)| c == &capability) {
            result.push((
                capability,
                installed
                    .grants
                    .exec
                    .iter()
                    .any(|e| e.program == entry.program),
            ));
        }
    }
    if installed.manifest.permissions.exec_from_data {
        result.push(("exec_from_data".into(), installed.grants.exec_from_data));
    }
    if installed.manifest.permissions.data {
        result.push(("data".into(), installed.grants.data));
    }
    result
}

pub struct Frontend {
    pub plugin: String,
}
impl hya_plugin::host::Frontend for Frontend {
    fn prompt(&mut self, form: Form) -> Result<Answers, PluginError> {
        let ctl = hya_plugin::runtime::CallCtl::new(std::time::Duration::from_secs(600));
        self.prompt_with_ctl(form, &ctl)
    }
    fn prompt_with_ctl(
        &mut self,
        form: Form,
        ctl: &hya_plugin::runtime::CallCtl,
    ) -> Result<Answers, PluginError> {
        let (reply, receive) = std::sync::mpsc::sync_channel(1);
        crate::engine::send(crate::engine::Cmd::PluginPrompt(Prompt {
            plugin: self.plugin.clone(),
            form,
            reply,
        }));
        loop {
            ctl.check()?;
            match receive.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(answer) => return answer,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => {
                    return Err(PluginError::new(
                        ErrorCode::Cancelled,
                        "plugin prompt closed",
                    ))
                }
            }
        }
    }
    fn log(&mut self, level: &str, message: &str) {
        let line = format!("plugin {}: {message}", self.plugin);
        match level {
            "debug" | "trace" => crate::log::debug(&line),
            "warn" | "warning" => crate::log::warn(&line),
            "error" => crate::log::error(&line),
            _ => crate::log::info(&line),
        }
    }
    fn progress(&mut self, _: u64, _: Option<u64>, _: Option<&str>) {}
}

fn form_answers(
    form: &Form,
    inputs: &std::collections::BTreeMap<String, String>,
) -> Result<Answers, PluginError> {
    let mut answers = Answers::new();
    for field in &form.fields {
        let raw = inputs.get(&field.key).map(String::as_str).unwrap_or("");
        let value = match field.kind {
            FieldKind::Bool | FieldKind::Checkbox => Value::Bool(raw == "true"),
            FieldKind::Number => Value::Number(raw.parse().map_err(|_| {
                PluginError::new(
                    ErrorCode::InvalidInput,
                    format!("{} requires a number", field.label),
                )
            })?),
            _ => Value::Text(raw.into()),
        };
        field
            .validate(&value)
            .map_err(|e| PluginError::new(ErrorCode::InvalidInput, e))?;
        answers.insert(field.key.clone(), value);
    }
    Ok(answers)
}

/// Whether an enabled plugin claims the address.
pub fn claimed(app: &App, address: &str) -> bool {
    app.options.plugins.installed.iter().any(|plugin| {
        plugin.enabled && hya_plugin::matcher::claims_input(&plugin.manifest, address)
    })
}

/// File actions declared by enabled plugins in resolver precedence order.
pub fn input_actions(app: &App) -> Vec<(String, usize, hya_plugin_api::InputAction)> {
    app.options
        .plugins
        .installed
        .iter()
        .filter(|plugin| plugin.enabled)
        .flat_map(|plugin| {
            plugin
                .manifest
                .input_actions
                .iter()
                .enumerate()
                .map(|(index, action)| (plugin.manifest.id.clone(), index, action.clone()))
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn resolver_preserves_browser_authentication_cookie_scope() {
        let mut cookie = hya_net::cookies::Cookie::new("SID", "signed-in", "youtube.com");
        cookie.host_only = false;
        cookie.secure = true;
        cookie.http_only = true;
        cookie.expires = Some(2_000_000_000);
        let mut imported = hya_net::CookieJar::new();
        imported.insert(cookie.clone());
        let jar = resolver_cookies(
            "https://www.youtube.com/watch?v=example",
            Some("SID=signed-in"),
            Some(imported),
        );
        assert_eq!(jar.iter().next(), Some(&cookie));
        assert_eq!(
            jar.header_value("youtubei.youtube.com", "/youtubei/v1/player", true, 0),
            Some("SID=signed-in".into())
        );
        assert!(jar.header_value("example.com", "/", true, 0).is_none());
        assert!(jar.header_value("www.youtube.com", "/", false, 0).is_none());
    }

    #[test]
    fn typed_resolver_cookies_remain_scoped_to_the_entered_host() {
        let jar = resolver_cookies(
            "https://www.youtube.com/watch?v=example",
            Some("SID=typed"),
            None,
        );
        assert_eq!(
            jar.header_value("www.youtube.com", "/", true, 0),
            Some("SID=typed".into())
        );
        assert!(jar
            .header_value("youtubei.youtube.com", "/", true, 0)
            .is_none());
        assert!(resolver_cookies("invalid", Some("SID=typed"), None).is_empty());
        assert!(resolver_cookies("https://www.youtube.com/", None, None).is_empty());
    }
    fn dispatch(app: &mut App, message: Message) {
        let _ = update(app, message);
    }
    pub(crate) fn plan() -> PlanInfo {
        PlanInfo {plugin:"example.video".into(), plan:serde_json::from_value(serde_json::json!({"id":"video","title":"A video","assemble":"mux","tracks":[
            {"id":"v","kind":"video","height":720,"codec":"avc1.4d401f","container":"mp4","sources":[{"url":"https://example.com/video"}]},
            {"id":"a","kind":"audio","codec":"mp4a.40.2","container":"m4a","sources":[{"url":"https://example.com/audio"}]},
            {"id":"s","kind":"subtitle","language":"en","container":"vtt","sources":[{"url":"https://example.com/subtitle"}]}
        ]})).unwrap(), preferences:hya_plugin_api::Preferences {include_files:true,container:Some("mp4".into()),..Default::default()} }
    }

    pub(crate) fn file_plugin(id: &str, label: &str, extension: &str) -> Installed {
        serde_json::from_value(serde_json::json!({"manifest": {"id":id,"name":label,"version":"1.0.0","api":1,"module":"plugin.wasm",
            "input_actions":[{"label":label,"extensions":[extension]}]}, "directory":"/plugins/test", "dev":false,"enabled":true,"grants":{},"pins":{}})).unwrap()
    }

    pub(crate) fn claiming_plugin(id: &str, claims: &[&str]) -> Installed {
        serde_json::from_value(serde_json::json!({"manifest": {"id":id,"name":id,"version":"1.0.0","api":1,"module":"plugin.wasm",
            "hooks":["resolve"],"claims":claims}, "directory":"/plugins/test", "dev":false,"enabled":true,"grants":{},"pins":{}})).unwrap()
    }

    #[test]
    fn only_enabled_plugins_claim_an_address() {
        let mut app = App::default();
        let magnet = "magnet:?xt=urn:btih:abc";
        assert!(!claimed(&app, magnet));
        app.options.plugins.installed = vec![claiming_plugin("hydra.torrent", &["magnet:*"])];
        assert!(claimed(&app, magnet));
        assert!(!claimed(&app, "https://example.com/magnet"));
        app.options.plugins.installed[0].enabled = false;
        assert!(!claimed(&app, magnet));
    }

    pub(crate) fn transfer_plan() -> PlanInfo {
        PlanInfo {plugin:"example.x".into(), plan:serde_json::from_value(serde_json::json!({"id":"transfer", "title":"Native download", "tracks":[],
            "transfer":{"engine":"x-engine","metadata":{},"files":[{"index":0,"path":"one.x","size":10},{"index":1,"path":"two.x","size":20}],"notice":"Select the files to download."}})).unwrap(), preferences:Default::default()}
    }

    #[test]
    fn file_buttons_follow_enabled_plugins_and_keep_each_plugins_label() {
        let mut app = App::default();
        app.options.plugins.installed = vec![
            file_plugin("example.x", "Browse X File", "x"),
            file_plugin("example.torrent", "Browse Torrent File", "torrent"),
        ];
        let actions = input_actions(&app);
        assert_eq!(
            actions
                .iter()
                .map(|(_, _, action)| action.label.as_str())
                .collect::<Vec<_>>(),
            ["Browse X File", "Browse Torrent File"]
        );
        app.options.plugins.installed[0].enabled = false;
        assert_eq!(input_actions(&app).len(), 1);
        app.options.plugins.installed.clear();
        assert!(input_actions(&app).is_empty());
    }

    #[test]
    fn native_file_selection_is_preserved_and_stale_probe_replies_are_ignored() {
        let mut app = App::default();
        app.add_url.plugin_plan = Some(transfer_plan());
        let _ = app.update(crate::app::Message::TransferFileSelected(0, false));
        let _ = app.update(crate::app::Message::TransferFileSelected(99, true));
        assert_eq!(
            app.add_url
                .plugin_plan
                .as_ref()
                .unwrap()
                .preferences
                .transfer_files
                .as_deref(),
            Some([1].as_slice())
        );
        let _ = app.update(crate::app::Message::TransferFileSelected(0, true));
        assert_eq!(
            app.add_url
                .plugin_plan
                .as_ref()
                .unwrap()
                .preferences
                .transfer_files
                .as_deref(),
            Some([1, 0].as_slice())
        );
        let current = hya_plugin::runtime::CallCtl::new(std::time::Duration::from_secs(10));
        app.add_url.plugin_ctl = Some(current);
        let stale = hya_plugin::runtime::CallCtl::new(std::time::Duration::from_secs(10));
        let _ = app.update(crate::app::Message::PluginProbeFinished(
            "file:///tmp/input.x".into(),
            stale,
            Box::new(Err("stale failure".into())),
        ));
        assert!(app.add_url.error.is_none());
    }

    #[test]
    fn permission_summary_retains_hosts_programs_cookies_and_downloaded_execution() {
        let manifest: Manifest = toml::from_str(include_str!(
            "../../../plugins/hydra-youtube/hydra-plugin.toml"
        ))
        .unwrap();
        let summary = permission_summary(&manifest.permissions);
        assert_eq!(summary.len(), 6);
        assert!(summary
            .iter()
            .any(|line| line.contains("cookies") && line.contains("youtube.com")));
        assert!(summary
            .iter()
            .any(|line| line.contains("yt-dlp") && line.contains("17 permitted")));
        assert!(summary
            .iter()
            .any(|line| line.contains("download programs")));
        assert!(permission_summary(&hya_plugin_api::Permissions::default()).is_empty());
    }
    #[test]
    fn installation_shows_optional_welcome_only_after_success() {
        let mut app = App::default();
        let mut manifest: Manifest = toml::from_str(include_str!(
            "../../../plugins/hydra-youtube/hydra-plugin.toml"
        ))
        .unwrap();
        app.options.plugins.review = Some(manifest.clone());
        dispatch(&mut app, Message::Finished(Ok(())));
        assert!(app.options.plugins.welcome.is_none());
        assert!(app.options.plugins.review.is_some());
        dispatch(&mut app, Message::Installed(Err("install failed".into())));
        assert!(app.options.plugins.welcome.is_none());
        dispatch(&mut app, Message::Installed(Ok(())));
        assert_eq!(
            app.options.plugins.welcome.as_ref().unwrap().1,
            manifest.welcome.clone().unwrap()
        );
        manifest.welcome = None;
        app.options.plugins.review = Some(manifest);
        dispatch(&mut app, Message::Installed(Ok(())));
        assert!(app.options.plugins.welcome.is_none());
    }

    #[test]
    fn track_selection_keeps_subtitles_and_sets_output_name() {
        let mut app = App::default();
        app.add_url.address = "https://example.com/video".into();
        let info = plan();
        assert_eq!(file_name(&info), "A video.mp4");
        let _ = app.update(crate::app::Message::PluginProbed(
            app.add_url.address.clone(),
            Box::new(Ok(Some(info))),
        ));
        for message in [
            crate::app::Message::PluginSubtitle("s".into(), true),
            crate::app::Message::PluginTrack(hya_plugin_api::TrackKind::Video, "v".into()),
            crate::app::Message::PluginTrack(hya_plugin_api::TrackKind::Audio, "None".into()),
            crate::app::Message::PluginContainer("mkv".into()),
        ] {
            let _ = app.update(message);
        }
        let info = app.add_url.plugin_plan.as_ref().unwrap();
        assert_eq!(info.preferences.track_ids, vec!["s", "v"]);
        assert_eq!(info.preferences.audio, hya_plugin_api::AudioPref::None);
        assert_eq!(info.preferences.container.as_deref(), Some("mkv"));
        assert_eq!(file_name(info), "A video.mp4");
        let _ = app.update(crate::app::Message::PluginSubtitle("s".into(), false));
        let _ = app.update(crate::app::Message::PluginTrack(
            hya_plugin_api::TrackKind::Audio,
            "Best".into(),
        ));
        assert_eq!(
            file_name(app.add_url.plugin_plan.as_ref().unwrap()),
            "A video.mkv"
        );
        let _ = app.update(crate::app::Message::PluginProbed(
            app.add_url.address.clone(),
            Box::new(Err("resolver failed".into())),
        ));
        assert_eq!(app.add_url.error.as_deref(), Some("resolver failed"));
    }
    #[test]
    fn reviewing_an_update_discards_previous_consent_and_uses_the_catalog_package_url() {
        let mut app = App::default();
        app.options.plugins.review = Some(
            toml::from_str(include_str!(
                "../../../plugins/hydra-youtube/hydra-plugin.toml"
            ))
            .unwrap(),
        );
        app.options.plugins.path = "old-package.hyaplugin".into();
        app.options.plugins.error = Some("old error".into());
        let entry = hya_plugin::distribution::parse_index(
            include_bytes!("../../../docs/plugins.json"),
            None,
            None,
        )
        .unwrap()
        .plugins
        .remove(0);
        let source = entry.package.clone();
        dispatch(&mut app, Message::ReviewUpdate(entry));
        assert_eq!(app.options.plugins.path, source);
        assert!(app.options.plugins.review.is_none());
        assert!(app.options.plugins.error.is_none());
        assert!(app.options.plugins.busy);
        dispatch(
            &mut app,
            Message::Reviewed(Box::new(Err("download failed".into()))),
        );
        assert!(app.options.plugins.review.is_none());
        assert!(app.options.plugins.prepared.is_none());
        assert_eq!(
            app.options.plugins.error.as_deref(),
            Some("download failed")
        );
    }

    #[test]
    fn checking_one_plugin_preserves_other_badges_and_clears_its_own_when_current() {
        let mut app = App::default();
        let mut available = hya_plugin::distribution::parse_index(
            include_bytes!("../../../docs/plugins.json"),
            None,
            None,
        )
        .unwrap()
        .plugins
        .remove(0);
        available.version = "0.2.0".into();
        let id = available.id.clone();
        let mut other = available.clone();
        other.id = "community.other".into();
        app.options.plugins.updates = vec![other.clone()];
        dispatch(&mut app, Message::CheckUpdate(id.clone()));
        assert!(app.options.plugins.busy);
        dispatch(&mut app, Message::CheckUpdate(other.id.clone()));
        dispatch(
            &mut app,
            Message::UpdateLoaded(id.clone(), Ok(vec![available.clone()])),
        );
        assert!(!app.options.plugins.busy);
        assert_eq!(app.options.plugins.updates.len(), 2);
        assert!(app
            .options
            .plugins
            .updates
            .iter()
            .any(|entry| entry.id == other.id));
        dispatch(
            &mut app,
            Message::UpdateLoaded(id.clone(), Err("unreachable catalog".into())),
        );
        assert_eq!(app.options.plugins.updates.len(), 2);
        assert_eq!(
            app.options.plugins.error.as_deref(),
            Some("unreachable catalog")
        );
        dispatch(&mut app, Message::CheckUpdate(id.clone()));
        assert!(app.options.plugins.error.is_none());
        dispatch(&mut app, Message::UpdateLoaded(id, Ok(vec![])));
        assert_eq!(app.options.plugins.updates.len(), 1);
        assert_eq!(app.options.plugins.updates[0].id, other.id);
    }

    #[test]
    fn reloading_installed_plugins_removes_completed_catalog_updates() {
        let mut app = App::default();
        let index = hya_plugin::distribution::parse_index(
            include_bytes!("../../../docs/plugins.json"),
            None,
            None,
        )
        .unwrap();
        let mut manifest: Manifest = toml::from_str(include_str!(
            "../../../plugins/hydra-youtube/hydra-plugin.toml"
        ))
        .unwrap();
        manifest.version = "0.0.1".into();
        let mut installed: Installed = serde_json::from_value(serde_json::json!({
            "manifest": manifest, "directory": "plugins/hydra.youtube", "dev": false,
            "enabled": true, "grants": {}, "pins": {}
        }))
        .unwrap();
        app.options.plugins.updates = index.plugins;
        let indexes =
            hya_plugin::distribution::sources(tempfile::tempdir().unwrap().path()).unwrap();
        dispatch(
            &mut app,
            Message::Loaded(Ok((vec![installed.clone()], indexes.clone()))),
        );
        assert_eq!(app.options.plugins.updates.len(), 1);
        assert!(app.options.plugins.indexes[0].is_default());
        installed.manifest.version = app.options.plugins.updates[0].version.clone();
        dispatch(&mut app, Message::Loaded(Ok((vec![installed], indexes))));
        assert!(app.options.plugins.updates.is_empty());
    }

    #[test]
    fn settings_navigation_discards_secret_edits_and_reports_async_errors() {
        let mut app = App::default();
        dispatch(
            &mut app,
            Message::Edit("example.video".into(), "token".into(), "private".into()),
        );
        assert_eq!(app.options.plugins.edits.len(), 1);
        dispatch(
            &mut app,
            Message::Detail(Some(Detail::Settings("example.video".into()))),
        );
        assert!(app.options.plugins.edits.is_empty());
        dispatch(
            &mut app,
            Message::IndexUrl("https://example.com/index".into()),
        );
        dispatch(&mut app, Message::IndexKey("key".into()));
        dispatch(&mut app, Message::UpdatesLoaded(Ok(vec![])));
        app.options.plugins.detail = Some(Detail::Logs("example.video".into()));
        dispatch(
            &mut app,
            Message::LogsLoaded("other.plugin".into(), Ok("stale".into())),
        );
        assert!(app.options.plugins.logs.is_empty());
        dispatch(
            &mut app,
            Message::LogsLoaded("example.video".into(), Ok("checked".into())),
        );
        assert_eq!(app.options.plugins.logs, "checked");
        dispatch(
            &mut app,
            Message::LogsLoaded("example.video".into(), Err("log error".into())),
        );
        assert_eq!(app.options.plugins.error.as_deref(), Some("log error"));
        dispatch(&mut app, Message::UpdatesLoaded(Err("index error".into())));
        assert_eq!(app.options.plugins.error.as_deref(), Some("index error"));
        dispatch(&mut app, Message::Loaded(Ok((vec![], vec![]))));
        dispatch(&mut app, Message::Loaded(Err("state error".into())));
        assert_eq!(app.options.plugins.error.as_deref(), Some("state error"));
        dispatch(
            &mut app,
            Message::Reviewed(Box::new(Err("bad package".into()))),
        );
        assert!(!app.options.plugins.busy);
        assert_eq!(app.options.plugins.error.as_deref(), Some("bad package"));
        dispatch(&mut app, Message::Path("plugin.hyaplugin".into()));
        assert_eq!(app.options.plugins.path, "plugin.hyaplugin");
        dispatch(&mut app, Message::Finished(Err("install failed".into())));
        assert_eq!(app.options.plugins.error.as_deref(), Some("install failed"));
        dispatch(&mut app, Message::Finished(Ok(())));
        assert!(app.options.plugins.error.is_none());
    }
    #[test]
    fn prompt_validation_keeps_dialog_open_until_valid_or_cancelled() {
        let form: Form = serde_json::from_value(serde_json::json!({"fields":[
            {"key":"n","label":"Count","type":"number"},
            {"key":"b","label":"Enabled","type":"boolean"},
            {"key":"s","label":"Secret","type":"secret"},
            {"key":"c","label":"Quality","type":"dropdown","options":["best","small"]}
        ]}))
        .unwrap();
        let mut app = App::default();
        let (reply, receiver) = std::sync::mpsc::sync_channel(1);
        app.options.plugins.prompt = Some(Prompt {
            plugin: "example.video".into(),
            form: form.clone(),
            reply,
        });
        dispatch(&mut app, Message::SubmitPrompt(true));
        assert!(app.options.plugins.prompt.is_some());
        assert!(receiver.try_recv().is_err());
        for (key, value) in [("n", "2"), ("b", "true"), ("s", "private"), ("c", "best")] {
            dispatch(&mut app, Message::Answer(key.into(), value.into()));
        }
        dispatch(&mut app, Message::SubmitPrompt(true));
        let values = receiver.recv().unwrap().unwrap();
        assert_eq!(values["n"], Value::Number(2.0));
        assert_eq!(values["b"], Value::Bool(true));
        assert!(app.options.plugins.answers.is_empty());
        assert!(app.options.plugins.prompt.is_none());
        let (reply, receiver) = std::sync::mpsc::sync_channel(1);
        app.options.plugins.prompt = Some(Prompt {
            plugin: "example.video".into(),
            form,
            reply,
        });
        dispatch(&mut app, Message::SubmitPrompt(false));
        assert_eq!(
            receiver.recv().unwrap().unwrap_err().code,
            ErrorCode::Cancelled
        );
    }
}
