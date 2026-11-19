#[cfg(test)]
mod tests;
mod views;

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use gpui::{Context, Image, ImageFormat, PathPromptOptions};

use crate::{
    download,
    downloads::{Queue, TrackKey},
    music::{Account, Music, Playlist, Poll, Provider, Song},
};

const PAGE: usize = 100;
const QR_TTL: Duration = Duration::from_secs(180);

#[derive(Default)]
struct Library {
    account: Option<Arc<Account>>,
    auth_epoch: usize,
    authenticating: bool,
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
}

pub struct Desktop {
    provider: Provider,
    libraries: [Library; 2],
    music: Option<Music>,
    startup_error: Option<String>,
    folder: Option<PathBuf>,
    show_downloads: bool,
    downloads: Queue,
    download_notice: Option<String>,
}

impl Desktop {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (downloads, download_notice) = Queue::open();
        let mut app = Self {
            provider: Provider::Qq,
            libraries: Default::default(),
            music: None,
            startup_error: None,
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
                    Ok(music) => app.music = Some(music),
                    Err(error) => app.startup_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn start_login(&mut self, cx: &mut Context<Self>) {
        let Some(music) = self.music.clone() else {
            return;
        };
        let provider = self.provider;
        let at = provider.index();
        let state = &mut self.libraries[at];
        if state.authenticating || state.account.is_some() {
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
                                app.finish_auth(at, epoch, "", cx);
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
        let state = &mut self.libraries[self.provider.index()];
        *state = Library {
            auth_epoch: state.auth_epoch + 1,
            ..Library::default()
        };
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
        if let (Some(_), Some(song)) = (&state.account, state.tracks.get(index)) {
            if self.downloads.enqueue(self.provider, song.clone(), folder) {
                self.run_downloads(cx);
            }
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
