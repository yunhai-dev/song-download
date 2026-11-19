use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use gpui::{
    ClickEvent, Context, Div, Image, ImageFormat, PathPromptOptions, Render, Stateful, Window, div,
    img, prelude::*, px, rgb, uniform_list,
};

use crate::{
    download,
    music::{Account, Music, Playlist, Poll, Provider, Song},
};

const PAGE: usize = 100;
const QR_TTL: Duration = Duration::from_secs(180);
const INK: u32 = 0x18241e;
const MUTED: u32 = 0x596b61;
const LINE: u32 = 0xdce6df;
const ACCENT: u32 = 0x08704a;
const SOFT: u32 = 0xe8f4ed;
const PAPER: u32 = 0xffffff;

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
    saving: bool,
    download_status: String,
}

impl Desktop {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut app = Self {
            provider: Provider::Qq,
            libraries: Default::default(),
            music: None,
            startup_error: None,
            folder: dirs::download_dir(),
            saving: false,
            download_status: String::new(),
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
                    Err(error) => app.download_status = format!("无法选择目录：{error}"),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let Some(folder) = self.folder.clone() else {
            self.download_status = "请先选择下载目录".into();
            cx.notify();
            return;
        };
        let provider = self.provider;
        let state = &self.libraries[provider.index()];
        let (Some(music), Some(account), Some(song)) = (
            self.music.clone(),
            state.account.clone(),
            state.tracks.get(index).cloned(),
        ) else {
            return;
        };
        self.saving = true;
        self.download_status = format!("正在下载 {}…", song.name);
        let work = cx
            .background_executor()
            .spawn(async move { download::download(&music, provider, &account, &song, &folder) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |app, cx| {
                app.saving = false;
                app.download_status = result.unwrap_or_else(|error| format!("下载失败：{error:#}"));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn sign_in(&self, state: &Library, cx: &Context<Self>) -> Div {
        let instructions = match self.provider {
            Provider::Qq => "使用 QQ 音乐 App 扫码并确认登录",
            Provider::Netease => "使用网易云音乐 App 扫码并确认登录",
        };
        let status = self
            .startup_error
            .as_deref()
            .unwrap_or(if self.music.is_none() {
                "正在启动本地适配器…"
            } else {
                &state.auth_status
            });
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .child(
                div()
                    .text_2xl()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(self.provider.label()),
            )
            .child(div().text_color(rgb(MUTED)).child(instructions))
            .when_some(state.qr.clone(), |panel, qr| {
                panel.child(
                    div()
                        .p_3()
                        .bg(rgb(PAPER))
                        .rounded_lg()
                        .child(img(qr).size(px(208.0))),
                )
            })
            .when(!status.is_empty(), |panel| {
                panel.child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(status.to_owned()),
                )
            })
            .when(self.startup_error.is_some(), |panel| {
                panel.child(
                    button("restart", "重启适配器", true)
                        .on_click(cx.listener(|app, _: &ClickEvent, _, cx| app.start_adapter(cx))),
                )
            })
            .when(self.music.is_some() && !state.authenticating, |panel| {
                panel.child(
                    button("login", "生成登录二维码", true)
                        .on_click(cx.listener(|app, _: &ClickEvent, _, cx| app.start_login(cx))),
                )
            })
            .when(state.authenticating, |panel| {
                panel.child(
                    button("cancel", "取消登录", false)
                        .on_click(cx.listener(|app, _: &ClickEvent, _, cx| app.clear_account(cx))),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("关闭应用后需重新扫码"),
            )
    }

    fn browser(&self, state: &Library, cx: &Context<Self>) -> Div {
        let title = state.selected.and_then(|index| state.playlists.get(index));
        let playlist_count = state.playlists.len();
        let track_count = state.tracks.len();
        let library_status = if state.loading_lists {
            Some("正在拉取歌单…")
        } else if let Some(error) = &state.list_error {
            Some(error.as_str())
        } else if state.playlists.is_empty() {
            Some("这个账号暂无可读歌单")
        } else {
            None
        };
        let track_status = if state.loading_tracks {
            Some("正在拉取歌曲…")
        } else if let Some(error) = &state.track_error {
            Some(error.as_str())
        } else if title.is_none() {
            Some("从左侧选择一个歌单")
        } else if state.tracks.is_empty() {
            Some("当前歌单没有可显示的歌曲")
        } else {
            None
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .gap_3()
            .child(
                div()
                    .w(px(225.0))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(PAPER))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(format!("我的歌单 · {playlist_count}"))
                            .child(button("refresh", "刷新", false).on_click(cx.listener(
                                |app, _: &ClickEvent, _, cx| app.load_playlists(app.provider, cx),
                            ))),
                    )
                    .when_some(library_status, |panel, message| panel.child(note(message)))
                    .child(
                        uniform_list(
                            "playlists",
                            playlist_count,
                            cx.processor(|app, range: std::ops::Range<usize>, _, cx| {
                                let state = &app.libraries[app.provider.index()];
                                range
                                    .map(|i| {
                                        button(
                                            ("playlist", i),
                                            &state.playlists[i].name,
                                            state.selected == Some(i),
                                        )
                                        .h(px(38.0))
                                        .w_full()
                                        .justify_start()
                                        .on_click(
                                            cx.listener(move |app, _: &ClickEvent, _, cx| {
                                                app.load_tracks(i, false, cx)
                                            }),
                                        )
                                    })
                                    .collect()
                            }),
                        )
                        .flex_1()
                        .min_h_0(),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .bg(rgb(PAPER))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .truncate()
                            .child(title.map_or_else(|| "歌曲".to_string(), |p| p.name.clone())),
                    )
                    .when_some(track_status, |panel, message| panel.child(note(message)))
                    .child(
                        uniform_list(
                            "tracks",
                            track_count,
                            cx.processor(|app, range: std::ops::Range<usize>, _, cx| {
                                let state = &app.libraries[app.provider.index()];
                                range
                                    .map(|i| {
                                        let song = &state.tracks[i];
                                        div()
                                            .h(px(58.0))
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .px_2()
                                            .border_b_1()
                                            .border_color(rgb(LINE))
                                            .child(
                                                div()
                                                    .w(px(26.0))
                                                    .text_sm()
                                                    .text_color(rgb(MUTED))
                                                    .child(format!("{:02}", i + 1)),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex()
                                                    .flex_col()
                                                    .child(
                                                        div()
                                                            .w_full()
                                                            .truncate()
                                                            .child(song.name.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .w_full()
                                                            .truncate()
                                                            .text_xs()
                                                            .text_color(rgb(MUTED))
                                                            .child(format!(
                                                                "{} · {}",
                                                                song.artist, song.album
                                                            )),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .w(px(45.0))
                                                    .text_xs()
                                                    .text_color(rgb(MUTED))
                                                    .child(format!(
                                                        "{}:{:02}",
                                                        song.seconds / 60,
                                                        song.seconds % 60
                                                    )),
                                            )
                                            .child(
                                                button(
                                                    ("save", i),
                                                    if app.saving { "下载中" } else { "下载" },
                                                    !app.saving,
                                                )
                                                .when(!app.saving, |button| {
                                                    button.on_click(cx.listener(
                                                        move |app, _: &ClickEvent, _, cx| {
                                                            app.save(i, cx)
                                                        },
                                                    ))
                                                }),
                                            )
                                    })
                                    .collect()
                            }),
                        )
                        .flex_1()
                        .min_h_0(),
                    )
                    .when(state.more || state.track_error.is_some(), |panel| {
                        panel.child(
                            button(
                                "more",
                                if state.track_error.is_some() {
                                    "重试"
                                } else {
                                    "加载更多"
                                },
                                false,
                            )
                            .on_click(cx.listener(
                                |app, _: &ClickEvent, _, cx| {
                                    let state = &app.libraries[app.provider.index()];
                                    if let Some(index) = state.selected {
                                        app.load_tracks(index, state.offset > 0, cx);
                                    }
                                },
                            )),
                        )
                    }),
            )
    }
}

impl Render for Desktop {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = &self.libraries[self.provider.index()];
        let folder = self
            .folder
            .as_deref()
            .map_or_else(|| "未选择下载目录".into(), |p| p.display().to_string());
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(rgb(0xf1f5f2))
            .text_color(rgb(INK))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .mr_2()
                            .text_xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("音乐下载"),
                    )
                    .children(Provider::ALL.map(|provider| {
                        button(
                            ("provider", provider.index()),
                            provider.label(),
                            provider == self.provider,
                        )
                        .on_click(cx.listener(
                            move |app, _: &ClickEvent, _, cx| {
                                app.provider = provider;
                                cx.notify();
                            },
                        ))
                    }))
                    .child(div().flex_1())
                    .when_some(state.account.as_ref(), |bar, account| {
                        bar.child(
                            div()
                                .max_w(px(200.0))
                                .truncate()
                                .text_sm()
                                .child(account.name.clone()),
                        )
                        .child(
                            button("logout", "退出本地登录", false).on_click(
                                cx.listener(|app, _: &ClickEvent, _, cx| app.clear_account(cx)),
                            ),
                        )
                    }),
            )
            .child(if state.account.is_some() {
                self.browser(state, cx)
            } else {
                self.sign_in(state, cx)
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .pt_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(button("folder", "选择下载目录", false).on_click(
                                cx.listener(|app, _: &ClickEvent, _, cx| app.choose_folder(cx)),
                            ))
                            .child(
                                div()
                                    .min_w_0()
                                    .text_xs()
                                    .truncate()
                                    .text_color(rgb(MUTED))
                                    .child(folder),
                            ),
                    )
                    .when(!self.download_status.is_empty(), |footer| {
                        footer.child(note(&self.download_status))
                    }),
            )
    }
}

fn button(id: impl Into<gpui::ElementId>, text: &str, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h_8()
        .px_3()
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .bg(rgb(if primary { ACCENT } else { SOFT }))
        .text_color(rgb(if primary { PAPER } else { INK }))
        .hover(move |style| style.bg(rgb(if primary { 0x095d40 } else { 0xd6eadd })))
        .child(div().min_w_0().truncate().child(text.to_owned()))
}

fn note(message: &str) -> Div {
    div()
        .text_sm()
        .text_color(rgb(MUTED))
        .child(message.to_owned())
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
