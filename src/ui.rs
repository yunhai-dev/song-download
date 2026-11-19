mod search_input;
#[cfg(test)]
mod tests;
mod views;

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use gpui::{AppContext, Context, Image, ImageFormat, KeyBinding, PathPromptOptions, actions};

use crate::{
    download,
    downloads::{Queue, TrackKey},
    music::{Account, Music, Playlist, Poll, Provider, Song},
};

const PAGE: usize = 100;
const SEARCH_PAGE: usize = 20;

actions!(ui_actions, [SubmitSearch]);
const QR_TTL: Duration = Duration::from_secs(180);

#[derive(Default)]
struct Library {
    account: Option<Arc<Account>>,
    auth_epoch: usize,
    authenticating: bool,
    restoring: bool,
    qr: Option<Arc<Image>>,
    auth_status: String,
    playlists: Vec<Playlist>,
    loading_lists: bool,
    list_error: Option<String>,
    selected: Option<usize>,
    tracks: Vec<Song>,
    offset: usize,
    more: bool,
    loading_tracks: bool,
    track_error: Option<String>,
    selected_tracks: HashSet<usize>,
    showing_search: bool,
    search_query: String,
    search_results: Vec<Song>,
    search_offset: usize,
    search_more: bool,
    searching: bool,
    search_error: Option<String>,
    search_epoch: usize,
}

impl Library {
    fn visible_tracks(&self) -> &[Song] {
        if self.showing_search {
            &self.search_results
        } else {
            &self.tracks
        }
    }
}

enum Restored {
    Missing,
    Expired,
    Failed(String),
    SignedIn(Account),
}

pub struct Desktop {
    provider: Provider,
    libraries: [Library; 2],
    search_input: gpui::Entity<search_input::SearchInput>,
    music: Option<Music>,
    startup_error: Option<String>,
    startup_warning: Option<String>,
    folder: Option<PathBuf>,
    show_downloads: bool,
    downloads: Queue,
    download_notice: Option<String>,
}

impl Desktop {
    pub fn new(cx: &mut Context<Self>) -> Self {
        search_input::bind_keys(cx);
        cx.bind_keys([KeyBinding::new("enter", SubmitSearch, None)]);
        let search_input = cx.new(search_input::SearchInput::new);
        let (downloads, download_notice) = Queue::open();
        let mut app = Self {
            provider: Provider::Qq,
            libraries: Default::default(),
            search_input,
            music: None,
            startup_error: None,
            startup_warning: None,
            folder: dirs::download_dir(),
            show_downloads: false,
            downloads,
            download_notice,
        };
        app.start_adapter(cx);
        app
    }

    fn start_adapter(&mut self, cx: &mut Context<Self>) {
        self.startup_error = None;
        let work = cx.background_executor().spawn(async { Music::new() });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |app, cx| {
                match result {
                    Ok(startup) => {
                        app.music = Some(startup.music);
                        if startup.warning.is_some() {
                            app.startup_warning = startup.warning;
                        }
                        app.restore_sessions(cx);
                    }
                    Err(error) => app.startup_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn restore_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(music) = self.music.clone() else {
            return;
        };
        for provider in Provider::ALL {
            let at = provider.index();
            {
                let state = &mut self.libraries[at];
                if state.account.is_some() || state.authenticating || state.restoring {
                    continue;
                }
                state.restoring = true;
                state.auth_status = "正在恢复上次登录…".into();
            }
            let executor = cx.background_executor().clone();
            let music = music.clone();
            cx.spawn(async move |this, cx| {
                let restored = executor
                    .spawn(async move {
                        let cookie = match crate::session::load_cookie(provider) {
                            Ok(Some(cookie)) => cookie,
                            Ok(None) => return Restored::Missing,
                            Err(error) => return Restored::Failed(format!("{error:#}")),
                        };
                        match music.restore(provider, &cookie) {
                            Ok(Some(account)) => Restored::SignedIn(account),
                            Ok(None) => {
                                if provider == Provider::Qq && !music.qq_store_ready() {
                                    Restored::Failed("QQ 音乐本地加密会话不可用".to_owned())
                                } else {
                                    let _ = crate::session::delete_cookie(provider);
                                    Restored::Expired
                                }
                            }
                            Err(error) => Restored::Failed(format!("{error:#}")),
                        }
                    })
                    .await;
                let _ = this.update(cx, |app, cx| {
                    let state = &mut app.libraries[at];
                    if !state.restoring {
                        return;
                    }
                    state.restoring = false;
                    match restored {
                        Restored::Missing => state.auth_status.clear(),
                        Restored::Expired => {
                            state.auth_status = "登录已过期，请重新扫码".into();
                        }
                        Restored::Failed(error) => {
                            state.auth_status = format!(
                                "暂时无法验证登录（{error}）；登录记录已保留，稍后可重启重试"
                            );
                        }
                        Restored::SignedIn(account) => {
                            app.libraries[at].account = Some(Arc::new(account));
                            app.load_playlists(provider, cx);
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn save_session(&mut self, provider: Provider, cookie: String, cx: &mut Context<Self>) {
        if provider == Provider::Qq {
            self.verify_qq_session(cookie.clone(), cx);
        }
        let work = cx
            .background_executor()
            .spawn(async move { crate::session::save_cookie(provider, &cookie) });
        cx.spawn(async move |this, cx| {
            if let Err(error) = work.await {
                let _ = this.update(cx, |app, cx| {
                    app.startup_warning = Some(format!("无法保存登录状态：{error:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn verify_qq_session(&mut self, cookie: String, cx: &mut Context<Self>) {
        let Some(music) = self.music.clone() else {
            return;
        };
        let work = cx
            .background_executor()
            .spawn(async move { music.qq_persisted(&cookie) });
        cx.spawn(async move |this, cx| {
            let message = match work.await {
                Ok(true) => return,
                Ok(false) => "QQ 音乐加密登录保存失败，本次登录可能无法在重启后恢复".to_owned(),
                Err(error) => format!("无法确认 QQ 音乐登录已加密保存（{error:#}）"),
            };
            let _ = this.update(cx, |app, cx| {
                app.startup_warning = Some(message);
                cx.notify();
            });
        })
        .detach();
    }

    fn start_login(&mut self, cx: &mut Context<Self>) {
        let Some(music) = self.music.clone() else {
            return;
        };
        let provider = self.provider;
        let at = provider.index();
        let state = &mut self.libraries[at];
        if state.authenticating || state.restoring || state.account.is_some() {
            return;
        }
        state.auth_epoch += 1;
        let epoch = state.auth_epoch;
        state.authenticating = true;
        state.auth_status = "正在生成二维码…".into();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let qr_music = music.clone();
            let qr = executor
                .spawn(async move {
                    let qr = qr_music.qr(provider)?;
                    Ok::<_, anyhow::Error>((qr.key, decode_qr(&qr.image)?))
                })
                .await;
            let (key, image) = match qr {
                Ok(qr) => qr,
                Err(error) => {
                    let _ = this.update(cx, |app, cx| {
                        app.finish_auth(at, epoch, &error.to_string(), cx)
                    });
                    return;
                }
            };
            let active = this
                .update(cx, |app, cx| {
                    let state = &mut app.libraries[at];
                    if state.auth_epoch != epoch {
                        return false;
                    }
                    state.qr = Some(image);
                    state.auth_status = "打开手机 App 扫描二维码".into();
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !active {
                return;
            }
            let start = Instant::now();
            loop {
                executor.timer(Duration::from_secs(2)).await;
                if !this
                    .update(cx, |app, _| app.libraries[at].auth_epoch == epoch)
                    .unwrap_or(false)
                {
                    return;
                }
                if start.elapsed() >= QR_TTL {
                    let _ = this.update(cx, |app, cx| {
                        app.finish_auth(at, epoch, "二维码已过期，请重新扫码", cx)
                    });
                    return;
                }
                let poll_music = music.clone();
                let poll_key = key.clone();
                let result = executor
                    .spawn(async move { poll_music.poll(provider, &poll_key) })
                    .await;
                let active = this
                    .update(cx, |app, cx| {
                        if app.libraries[at].auth_epoch != epoch {
                            return false;
                        }
                        match result {
                            Ok(Poll::Waiting) => return true,
                            Ok(Poll::Scanned) => {
                                app.libraries[at].auth_status = "已扫码，请在手机上确认登录".into();
                                cx.notify();
                                return true;
                            }
                            Ok(Poll::Expired) => {
                                app.finish_auth(at, epoch, "二维码已过期，请重新扫码", cx)
                            }
                            Ok(Poll::SignedIn(account)) => {
                                let cookie = account.cookie.clone();
                                app.finish_auth(at, epoch, "", cx);
                                app.save_session(provider, cookie, cx);
                                app.libraries[at].account = Some(Arc::new(account));
                                app.load_playlists(provider, cx);
                            }
                            Err(error) => app.finish_auth(at, epoch, &error.to_string(), cx),
                        }
                        false
                    })
                    .unwrap_or(false);
                if !active {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn finish_auth(&mut self, at: usize, epoch: usize, message: &str, cx: &mut Context<Self>) {
        let state = &mut self.libraries[at];
        if state.auth_epoch == epoch {
            state.qr = None;
            state.authenticating = false;
            state.auth_status = message.to_owned();
            cx.notify();
        }
    }

    fn clear_account(&mut self, cx: &mut Context<Self>) {
        let provider = self.provider;
        let waiting = self
            .downloads
            .entries()
            .iter()
            .filter(|entry| {
                entry.provider == provider
                    && matches!(entry.status, crate::downloads::Status::Queued)
            })
            .map(|entry| entry.key.clone())
            .collect::<Vec<_>>();
        for key in waiting {
            self.downloads.cancel(&key);
        }
        let account = self.libraries[provider.index()].account.take();
        let state = &mut self.libraries[provider.index()];
        *state = Library {
            auth_epoch: state.auth_epoch + 1,
            ..Library::default()
        };
        if let Some(account) = account {
            let music = self.music.clone();
            let work = cx.background_executor().spawn(async move {
                crate::session::delete_cookie(provider)?;
                if provider == Provider::Qq {
                    if let Some(music) = music {
                        music.forget_qq(&account.cookie)?;
                    }
                }
                Ok::<_, anyhow::Error>(())
            });
            cx.spawn(async move |this, cx| {
                if let Err(error) = work.await {
                    let _ = this.update(cx, |app, cx| {
                        app.startup_warning = Some(format!("无法完整清除本机登录：{error:#}"));
                        cx.notify();
                    });
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn load_playlists(&mut self, provider: Provider, cx: &mut Context<Self>) {
        let at = provider.index();
        let state = &mut self.libraries[at];
        let (Some(music), Some(account)) = (self.music.clone(), state.account.clone()) else {
            return;
        };
        if state.loading_lists || state.loading_tracks {
            return;
        }
        let epoch = state.auth_epoch;
        state.loading_lists = true;
        state.list_error = None;
        let work = cx
            .background_executor()
            .spawn(async move { music.playlists(provider, &account) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |app, cx| {
                let state = &mut app.libraries[at];
                if state.auth_epoch != epoch {
                    return;
                }
                state.loading_lists = false;
                match result {
                    Ok(playlists) => {
                        state.playlists = playlists;
                        state.selected = None;
                        state.tracks.clear();
                        state.selected_tracks.clear();
                        state.loading_tracks = false;
                        state.track_error = None;
                        state.more = false;
                    }
                    Err(error) => state.list_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_tracks(&mut self, selected: usize, append: bool, cx: &mut Context<Self>) {
        let provider = self.provider;
        let at = provider.index();
        let state = &mut self.libraries[at];
        let (Some(music), Some(account), Some(playlist)) = (
            self.music.clone(),
            state.account.clone(),
            state.playlists.get(selected).cloned(),
        ) else {
            return;
        };
        if state.loading_tracks || state.loading_lists {
            return;
        }
        if !append {
            state.tracks.clear();
            state.selected_tracks.clear();
            state.showing_search = false;
            state.search_results.clear();
            state.searching = false;
            state.search_error = None;
            state.search_epoch += 1;
            state.offset = 0;
            state.more = false;
        }
        let epoch = state.auth_epoch;
        let offset = state.offset;
        state.selected = Some(selected);
        state.loading_tracks = true;
        state.track_error = None;
        let work = cx
            .background_executor()
            .spawn(async move { music.tracks(provider, &account, &playlist, offset, PAGE) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |app, cx| {
                let state = &mut app.libraries[at];
                if state.auth_epoch != epoch || state.selected != Some(selected) {
                    return;
                }
                state.loading_tracks = false;
                match result {
                    Ok(page) => {
                        state.offset += PAGE;
                        state.more = page.more;
                        state.tracks.extend(page.items);
                    }
                    Err(error) => state.track_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn submit_search(&mut self, _: &SubmitSearch, _: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.search_input.read(cx).is_composing() {
            return;
        }
        let query = self.search_input.read(cx).content();
        self.start_search(query, cx);
    }

    fn start_search(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.show_downloads = false;
        let provider = self.provider;
        let (Some(music), Some(account)) = (
            self.music.clone(),
            self.libraries[provider.index()].account.clone(),
        ) else {
            self.download_notice = Some("请先登录后再搜索".into());
            cx.notify();
            return;
        };
        let at = provider.index();
        let state = &mut self.libraries[at];
        state.search_epoch += 1;
        let epoch = state.search_epoch;
        state.showing_search = true;
        state.selected = None;
        state.search_query = query.clone();
        state.search_results.clear();
        state.search_offset = 0;
        state.search_more = false;
        state.searching = true;
        state.search_error = None;
        state.selected_tracks.clear();
        let work = cx
            .background_executor()
            .spawn(async move { music.search(provider, &account, &query, 0, SEARCH_PAGE) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |app, cx| {
                let state = &mut app.libraries[at];
                if state.search_epoch != epoch {
                    return;
                }
                state.searching = false;
                match result {
                    Ok(page) => {
                        state.search_results = page.items;
                        state.search_offset = SEARCH_PAGE;
                        state.search_more = page.more;
                    }
                    Err(error) => state.search_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn clear_search(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.libraries[self.provider.index()];
        state.search_epoch += 1;
        state.showing_search = false;
        state.search_results.clear();
        state.searching = false;
        state.search_error = None;
        state.search_more = false;
        state.selected_tracks.clear();
        cx.notify();
    }

    fn load_more_search(&mut self, cx: &mut Context<Self>) {
        let provider = self.provider;
        let at = provider.index();
        let state = &self.libraries[at];
        if state.searching || !state.search_more {
            return;
        }
        let (Some(music), Some(account)) = (self.music.clone(), state.account.clone()) else {
            return;
        };
        let query = state.search_query.clone();
        let offset = state.search_offset;
        let epoch = state.search_epoch;
        self.libraries[at].searching = true;
        let work = cx
            .background_executor()
            .spawn(async move { music.search(provider, &account, &query, offset, SEARCH_PAGE) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |app, cx| {
                let state = &mut app.libraries[at];
                if state.search_epoch != epoch {
                    return;
                }
                state.searching = false;
                match result {
                    Ok(page) => {
                        state.search_results.extend(page.items);
                        state.search_offset += SEARCH_PAGE;
                        state.search_more = page.more;
                    }
                    Err(error) => state.search_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_more(&mut self, cx: &mut Context<Self>) {
        let state = &self.libraries[self.provider.index()];
        if state.showing_search {
            self.load_more_search(cx);
        } else if let Some(index) = state.selected {
            let append = state.offset > 0;
            self.load_tracks(index, append, cx);
        }
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择下载目录".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = picker.await else { return };
            let _ = this.update(cx, |app, cx| {
                match result {
                    Ok(Some(paths)) => app.folder = paths.into_iter().next(),
                    Ok(None) => return,
                    Err(error) => app.download_notice = Some(format!("无法选择目录：{error}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(folder) = self.folder.clone() else {
            self.download_notice = Some("请先选择下载目录".into());
            cx.notify();
            return;
        };
        let state = &self.libraries[self.provider.index()];
        if let (Some(_), Some(song)) = (&state.account, state.visible_tracks().get(index)) {
            if self.downloads.enqueue(self.provider, song.clone(), folder) {
                self.run_downloads(cx);
            }
        }
    }

    fn toggle_track_selection(&mut self, index: usize, cx: &mut Context<Self>) {
        let state = &mut self.libraries[self.provider.index()];
        if index >= state.visible_tracks().len() {
            return;
        }
        if !state.selected_tracks.remove(&index) {
            state.selected_tracks.insert(index);
        }
        cx.notify();
    }

    fn select_all_tracks(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.libraries[self.provider.index()];
        let visible = state.visible_tracks().len();
        if state.selected_tracks.len() == visible {
            state.selected_tracks.clear();
        } else {
            state.selected_tracks = (0..visible).collect();
        }
        cx.notify();
    }

    fn clear_track_selection(&mut self, cx: &mut Context<Self>) {
        self.libraries[self.provider.index()]
            .selected_tracks
            .clear();
        cx.notify();
    }

    fn download_selected(&mut self, cx: &mut Context<Self>) {
        let Some(folder) = self.folder.clone() else {
            self.download_notice = Some("请先选择下载目录".into());
            cx.notify();
            return;
        };
        let provider = self.provider;
        let songs = {
            let state = &self.libraries[provider.index()];
            if state.account.is_none() {
                return;
            }
            let mut indices = state
                .selected_tracks
                .iter()
                .copied()
                .filter(|index| *index < state.visible_tracks().len())
                .collect::<Vec<_>>();
            indices.sort_unstable();
            indices
                .into_iter()
                .map(|index| state.visible_tracks()[index].clone())
                .collect::<Vec<_>>()
        };
        if songs.is_empty() {
            return;
        }
        let mut added = 0;
        let mut skipped = 0;
        for song in songs {
            if self.downloads.enqueue(provider, song, folder.clone()) {
                added += 1;
            } else {
                skipped += 1;
            }
        }
        self.libraries[provider.index()].selected_tracks.clear();
        if skipped > 0 {
            self.download_notice = Some(format!(
                "已加入 {added} 首，跳过 {skipped} 首（已在队列或已下载）"
            ));
        }
        if added > 0 {
            self.run_downloads(cx);
        } else {
            cx.notify();
        }
    }

    fn retry_download(&mut self, key: &TrackKey, cx: &mut Context<Self>) {
        let Some(entry) = self
            .downloads
            .entries()
            .iter()
            .find(|entry| &entry.key == key)
        else {
            return;
        };
        if self.libraries[entry.provider.index()].account.is_none() {
            self.download_notice = Some(format!("请先登录 {} 后再重试", entry.provider.label()));
            cx.notify();
        } else if self.downloads.retry(key) {
            self.run_downloads(cx);
        }
    }

    fn cancel_download(&mut self, key: &TrackKey, cx: &mut Context<Self>) {
        self.downloads.cancel(key);
        cx.notify();
    }

    fn run_downloads(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        let Some(music) = self.music.clone() else {
            return;
        };
        while let Some(job) = self.downloads.next() {
            let Some(account) = self.libraries[job.provider.index()].account.clone() else {
                let _ = self.downloads.finish(
                    &job.key,
                    Err(anyhow::anyhow!("请先登录 {}", job.provider.label())),
                );
                continue;
            };
            let key = job.key;
            let work = cx.background_executor().spawn(async move {
                download::download(&music, job.provider, &account, &job.song, &job.folder)
            });
            cx.spawn(async move |this, cx| {
                let result = work.await;
                let _ = this.update(cx, |app, cx| {
                    if let Err(error) = app.downloads.finish(&key, result) {
                        app.download_notice = Some(format!("无法保存下载记录：{error:#}"));
                    }
                    app.run_downloads(cx);
                });
            })
            .detach();
            break;
        }
    }
}

fn decode_qr(data: &str) -> Result<Arc<Image>> {
    let (header, encoded) = data.split_once(',').context("二维码格式无效")?;
    let format = match header {
        "data:image/png;base64" => ImageFormat::Png,
        "data:image/jpeg;base64" => ImageFormat::Jpeg,
        "data:image/gif;base64" => ImageFormat::Gif,
        _ => anyhow::bail!("不支持的二维码图片格式"),
    };
    ensure!(encoded.len() <= 1024 * 1024, "二维码图片过大");
    let bytes = STANDARD.decode(encoded).context("无法解码二维码图片")?;
    Ok(Arc::new(Image::from_bytes(format, bytes)))
}
