//! Rendered views for the desktop shell: navigation, sign-in, library browser
//! and the download manager.
//!
//! State and asynchronous actions live on [`Desktop`] in the parent module; this
//! module only reads that state and wires buttons to the existing handlers.

use std::sync::Arc;

use gpui::{
    ClickEvent, Context, Div, Image, IntoElement, Render, Stateful, Window, div, img, prelude::*,
    px, rgb, uniform_list,
};

use super::*;
use crate::{
    downloads::{Status, TrackKey},
    music::Provider,
};

const BG: u32 = 0xf9f9fb;
const SIDEBAR: u32 = 0xf3f3f6;
pub(super) const PANEL: u32 = 0xffffff;
const ELEVATED: u32 = 0xf0f0f4;
const HOVER: u32 = 0xececf1;
pub(super) const BORDER: u32 = 0xdcdce5;
const BORDER_SOFT: u32 = 0xe8e8ee;
const TEXT: u32 = 0x202027;
pub(super) const MUTED: u32 = 0x555564;
pub(super) const FAINT: u32 = 0x646478;
pub(super) const ACCENT: u32 = 0x5b50cd;
const ACCENT_HOVER: u32 = 0x4e43b9;
const ACCENT_SOFT: u32 = 0xeeecff;
const GREEN: u32 = 0x176c49;
const GREEN_SOFT: u32 = 0xe6f5ed;
const RED: u32 = 0xa8324a;
const RED_SOFT: u32 = 0xfdeef1;
const AMBER: u32 = 0x82550b;
const AMBER_SOFT: u32 = 0xfff4de;
const AMBER_BORDER: u32 = 0xedd6aa;
const WHITE: u32 = 0xffffff;

const INDEX_W: f32 = 36.0;
const TIME_W: f32 = 56.0;
const ACTION_W: f32 = 104.0;
const TRACK_ROW_H: f32 = 52.0;
const DOWNLOAD_ROW_H: f32 = 92.0;

impl Desktop {
    fn shell(&self, cx: &mut Context<Self>) -> Div {
        let provider = self.provider;
        let library = &self.libraries[provider.index()];
        let account = library.account.as_ref().map(|account| account.name.clone());
        let folder = self.folder.clone();

        let sidebar = self.sidebar(account.as_deref(), folder.as_ref(), cx);
        let body = if self.show_downloads {
            self.download_manager(cx)
        } else if library.account.is_some() {
            self.browser(library, cx)
        } else {
            self.sign_in(library, cx)
        };

        let title = if self.show_downloads {
            "下载管理"
        } else {
            "音乐库"
        };
        let subtitle = match (self.show_downloads, account.as_deref()) {
            (true, _) => {
                let pending = self.downloads.pending_count();
                if pending > 0 {
                    format!("{pending} 个任务进行中")
                } else {
                    "所有任务都在这里".to_owned()
                }
            }
            (false, Some(name)) => format!("{} · {name}", provider.label()),
            (false, None) => format!("{} · 未登录", provider.label()),
        };

        div()
            .size_full()
            .flex()
            .flex_row()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .on_action(cx.listener(Self::submit_search))
            .child(sidebar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.topbar(title, subtitle, cx))
                    .when_some(self.download_notice.clone(), |main, notice| {
                        main.child(notice_bar(notice))
                    })
                    .when_some(self.startup_warning.clone(), |main, notice| {
                        main.child(notice_bar(notice))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .flex()
                            .flex_col()
                            .p(px(18.0))
                            .child(body),
                    ),
            )
    }

    fn topbar(&self, title: &'static str, subtitle: String, cx: &mut Context<Self>) -> Div {
        div()
            .w_full()
            .flex_shrink_0()
            .h(px(56.0))
            .px(px(20.0))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(BORDER_SOFT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT))
                            .child(title),
                    )
                    .child(div().w(px(1.0)).h(px(12.0)).bg(rgb(BORDER)))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(subtitle)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(200.0)).child(self.search_input.clone()))
                    .child(accent_button("search-submit", "搜索").on_click(cx.listener(
                        |app, _: &ClickEvent, _, cx| {
                            let query = app.search_input.read(cx).content();
                            app.start_search(query, cx);
                        },
                    )))
                    .when(
                        self.libraries[self.provider.index()].showing_search,
                        |row| {
                            row.child(ghost_button("search-clear", "返回").on_click(
                                cx.listener(|app, _: &ClickEvent, _, cx| app.clear_search(cx)),
                            ))
                        },
                    )
                    .child(chip(
                        self.libraries[self.provider.index()]
                            .account
                            .as_ref()
                            .map_or("未登录", |_| "已登录"),
                        MUTED,
                        ELEVATED,
                    ))
                    .child(provider_dot(self.provider)),
            )
    }

    fn sidebar(
        &self,
        account: Option<&str>,
        folder: Option<&std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) -> Div {
        let pending = self.downloads.pending_count();
        div()
            .w(px(184.0))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(BORDER_SOFT))
            .child(
                div()
                    .h(px(56.0))
                    .flex_shrink_0()
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(BORDER_SOFT))
                    .child(
                        div()
                            .size(px(26.0))
                            .flex_shrink_0()
                            .rounded_md()
                            .bg(rgb(ACCENT))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(rgb(WHITE))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("♪"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("音乐下载"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap(px(18.0))
                    .p(px(12.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(section_label("平台"))
                            .children(Provider::ALL.map(|provider| {
                                let selected = provider == self.provider;
                                nav_row(("provider", provider.index()), selected)
                                    .on_click(cx.listener(move |app, _: &ClickEvent, _, cx| {
                                        app.provider = provider;
                                        cx.notify();
                                    }))
                                    .child(provider_dot(provider))
                                    .child(div().min_w_0().truncate().child(provider.label()))
                                    .when(selected, |row| {
                                        row.child(div().flex_1()).child(
                                            div().text_xs().text_color(rgb(ACCENT)).child("●"),
                                        )
                                    })
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(section_label("浏览"))
                            .child(
                                nav_row("nav-library", !self.show_downloads)
                                    .debug_selector(|| "nav-library".into())
                                    .on_click(cx.listener(|app, _: &ClickEvent, _, cx| {
                                        app.show_downloads = false;
                                        cx.notify();
                                    }))
                                    .child(nav_glyph("≡", !self.show_downloads))
                                    .child(div().min_w_0().truncate().child("音乐库")),
                            )
                            .child(
                                nav_row("nav-downloads", self.show_downloads)
                                    .debug_selector(|| "nav-downloads".into())
                                    .on_click(cx.listener(|app, _: &ClickEvent, _, cx| {
                                        app.show_downloads = true;
                                        cx.notify();
                                    }))
                                    .child(nav_glyph("↓", self.show_downloads))
                                    .child(div().min_w_0().truncate().child("下载管理"))
                                    .when(pending > 0, |row| {
                                        row.child(div().flex_1()).child(
                                            div()
                                                .h(px(18.0))
                                                .min_w(px(18.0))
                                                .px_1()
                                                .rounded_full()
                                                .bg(rgb(ACCENT))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_xs()
                                                .text_color(rgb(WHITE))
                                                .child(pending.to_string()),
                                        )
                                    }),
                            ),
                    )
                    .child(div().flex_1()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .p(px(12.0))
                    .border_t_1()
                    .border_color(rgb(BORDER_SOFT))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section_label("下载目录"))
                    .child(
                        div()
                            .w_full()
                            .text_xs()
                            .truncate()
                            .text_color(rgb(MUTED))
                            .child(folder.map_or_else(
                                || "未选择目录".to_owned(),
                                |path| path.display().to_string(),
                            )),
                    )
                    .child(
                        ghost_button("choose-folder", "选择下载目录")
                            .w_full()
                            .on_click(
                                cx.listener(|app, _: &ClickEvent, _, cx| app.choose_folder(cx)),
                            ),
                    )
                    .when_some(account, |footer, name| {
                        footer.child(
                            div()
                                .mt_1()
                                .pt_3()
                                .border_t_1()
                                .border_color(rgb(BORDER_SOFT))
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .size(px(24.0))
                                        .flex_shrink_0()
                                        .rounded_full()
                                        .bg(rgb(ACCENT_SOFT))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_xs()
                                        .text_color(rgb(ACCENT))
                                        .child(initial(name)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(rgb(TEXT))
                                        .child(name.to_owned()),
                                )
                                .child(ghost_button("sign-out", "退出").on_click(
                                    cx.listener(|app, _: &ClickEvent, _, cx| app.clear_account(cx)),
                                )),
                        )
                    }),
            )
    }

    fn sign_in(&self, state: &Library, cx: &mut Context<Self>) -> Div {
        let instructions = match self.provider {
            Provider::Qq => "使用 QQ 音乐 App 扫描二维码并确认登录",
            Provider::Netease => "使用网易云音乐 App 扫描二维码并确认登录",
        };
        let status = self
            .startup_error
            .as_deref()
            .unwrap_or(if self.music.is_none() {
                "正在启动本地适配器…"
            } else {
                &state.auth_status
            });
        let qr = state.qr.clone();

        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(430.0))
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL))
                    .p(px(28.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(provider_dot(self.provider))
                                    .child(
                                        div()
                                            .text_lg()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(format!("登录 {}", self.provider.label())),
                                    ),
                            )
                            .child(div().text_sm().text_color(rgb(MUTED)).child(instructions)),
                    )
                    .when_some(qr, |card, qr| card.child(qr_card(qr)))
                    .when(
                        qr_placeholder_visible(state, self.music.is_some()),
                        |card| {
                            card.child(
                                div()
                                    .size(px(232.0))
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(rgb(BORDER))
                                    .bg(rgb(ELEVATED))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .child(div().text_2xl().text_color(rgb(FAINT)).child("⌁"))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(FAINT))
                                            .child("二维码准备中"),
                                    ),
                            )
                        },
                    )
                    .when(!status.is_empty(), |card| {
                        card.child(
                            div()
                                .min_h(px(20.0))
                                .text_sm()
                                .text_color(rgb(if self.startup_error.is_some() {
                                    RED
                                } else {
                                    MUTED
                                }))
                                .child(status.to_owned()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(self.startup_error.is_some(), |row| {
                                row.child(accent_button("restart-adapter", "重启适配器").on_click(
                                    cx.listener(|app, _: &ClickEvent, _, cx| app.start_adapter(cx)),
                                ))
                            })
                            .when(
                                self.music.is_some()
                                    && !state.authenticating
                                    && !state.restoring
                                    && self.startup_error.is_none(),
                                |row| {
                                    row.child(
                                        accent_button("start-login", "生成登录二维码").on_click(
                                            cx.listener(|app, _: &ClickEvent, _, cx| {
                                                app.start_login(cx)
                                            }),
                                        ),
                                    )
                                },
                            )
                            .when(state.authenticating, |row| {
                                row.child(ghost_button("cancel-login", "取消登录").on_click(
                                    cx.listener(|app, _: &ClickEvent, _, cx| app.clear_account(cx)),
                                ))
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(FAINT))
                            .child("登录状态保存在系统钥匙串，下次打开自动恢复"),
                    ),
            )
    }

    fn browser(&self, state: &Library, cx: &mut Context<Self>) -> Div {
        let title = state.selected.and_then(|index| state.playlists.get(index));
        let playlist_count = state.playlists.len();
        let track_count = state.visible_tracks().len();
        let list_note = if state.loading_lists {
            Some("正在同步歌单…".to_owned())
        } else if let Some(error) = &state.list_error {
            Some(error.clone())
        } else if state.playlists.is_empty() {
            Some("这个账号暂无可读歌单".to_owned())
        } else {
            None
        };
        let track_note = if state.showing_search {
            if state.searching {
                Some("正在搜索…".to_owned())
            } else if let Some(error) = &state.search_error {
                Some(error.clone())
            } else if state.search_results.is_empty() {
                Some("没有找到匹配的歌曲".to_owned())
            } else {
                None
            }
        } else if state.loading_tracks {
            Some("正在加载歌曲…".to_owned())
        } else if let Some(error) = &state.track_error {
            Some(error.clone())
        } else if title.is_none() {
            Some("从左侧选择一个歌单，或搜索全站歌曲".to_owned())
        } else if state.tracks.is_empty() {
            Some("当前歌单没有可显示的歌曲".to_owned())
        } else {
            None
        };
        let more = if state.showing_search {
            state.search_more
        } else {
            state.more
        };
        let has_error = if state.showing_search {
            state.search_error.is_some()
        } else {
            state.track_error.is_some()
        };
        let display_title = if state.showing_search {
            format!("搜索：{}", state.search_query)
        } else {
            title.map_or_else(|| "歌曲".to_owned(), |playlist| playlist.name.clone())
        };
        let count_label = if state.showing_search {
            if track_count == 0 {
                "全站搜索结果".to_owned()
            } else {
                format!("{track_count} 首结果")
            }
        } else if track_count == 0 {
            "尚未载入歌曲".to_owned()
        } else {
            format!("{track_count} 首歌曲")
        };

        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_row()
            .gap_3()
            .child(
                div()
                    .w(px(180.0))
                    .flex_shrink_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER_SOFT))
                    .bg(rgb(PANEL))
                    .child(
                        div()
                            .flex_shrink_0()
                            .h(px(46.0))
                            .px_3()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(rgb(BORDER_SOFT))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child("歌单"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(MUTED))
                                            .child(playlist_count.to_string()),
                                    ),
                            )
                            .child(ghost_button("refresh-playlists", "↻").on_click(cx.listener(
                                |app, _: &ClickEvent, _, cx| app.load_playlists(app.provider, cx),
                            ))),
                    )
                    .when_some(list_note, |panel, message| {
                        panel.child(div().px_3().pt_2().child(note_text(message)))
                    })
                    .child(
                        uniform_list(
                            "playlists",
                            playlist_count,
                            cx.processor(|app, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .map(|i| {
                                        let state = &app.libraries[app.provider.index()];
                                        let selected = state.selected == Some(i);
                                        let name = state.playlists[i].name.clone();
                                        playlist_row(("playlist", i), selected, &name).on_click(
                                            cx.listener(move |app, _: &ClickEvent, _, cx| {
                                                app.load_tracks(i, false, cx)
                                            }),
                                        )
                                    })
                                    .collect()
                            }),
                        )
                        .flex_1()
                        .min_h_0()
                        .w_full(),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(BORDER_SOFT))
                    .bg(rgb(PANEL))
                    .child(
                        div()
                            .flex_shrink_0()
                            .h(px(52.0))
                            .px(px(16.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .border_b_1()
                            .border_color(rgb(BORDER_SOFT))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .child(display_title),
                                    )
                                    .child(
                                        div().text_xs().text_color(rgb(MUTED)).child(count_label),
                                    ),
                            )
                            .child(chip(self.provider.label(), MUTED, ELEVATED)),
                    )
                    .child(track_header(self, cx))
                    .when(!state.selected_tracks.is_empty(), |panel| {
                        panel.child(batch_bar(self, cx))
                    })
                    .when_some(track_note, |panel, message| {
                        panel.child(
                            div()
                                .px(px(16.0))
                                .py_2()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(message),
                        )
                    })
                    .child(
                        uniform_list(
                            "tracks",
                            track_count,
                            cx.processor(|app, range: std::ops::Range<usize>, _, cx| {
                                range.map(|i| track_row(app, i, cx)).collect()
                            }),
                        )
                        .flex_1()
                        .min_h_0()
                        .w_full(),
                    )
                    .when(more || has_error, |panel| {
                        panel.child(
                            div()
                                .flex_shrink_0()
                                .px(px(16.0))
                                .py_2()
                                .flex()
                                .justify_end()
                                .border_t_1()
                                .border_color(rgb(BORDER_SOFT))
                                .child(
                                    ghost_button(
                                        "load-more",
                                        if has_error { "重试" } else { "加载更多" },
                                    )
                                    .on_click(
                                        cx.listener(|app, _: &ClickEvent, _, cx| app.load_more(cx)),
                                    ),
                                ),
                        )
                    }),
            )
    }

    fn download_manager(&self, cx: &mut Context<Self>) -> Div {
        let entries = self.downloads.entries();
        let total = entries.len();
        let pending = self.downloads.pending_count();
        let mut completed = 0usize;
        let mut failed = 0usize;
        for entry in entries {
            match &entry.status {
                Status::Completed(_) => completed += 1,
                Status::Failed(_) => failed += 1,
                _ => {}
            }
        }
        let folder = self.folder.clone();

        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER_SOFT))
            .bg(rgb(PANEL))
            .child(
                div()
                    .flex_shrink_0()
                    .px(px(18.0))
                    .py(px(14.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .border_b_1()
                    .border_color(rgb(BORDER_SOFT))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("下载任务"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child(format!(
                                        "{total} 条记录 · {pending} 进行中 · {completed} 已完成 · {failed} 失败"
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .max_w(px(300.0))
                                    .truncate()
                                    .text_xs()
                                    .text_color(rgb(FAINT))
                                    .child(folder.as_ref().map_or_else(
                                        || "未选择下载目录".to_owned(),
                                        |path| path.display().to_string(),
                                    )),
                            )
                            .when_some(folder.clone(), |bar, path| {
                                bar.child(
                                    ghost_button("open-folder", "打开目录").on_click(
                                        cx.listener(move |_, _: &ClickEvent, _, cx| {
                                            cx.reveal_path(&path);
                                        }),
                                    ),
                                )
                            })
                            .child(
                                accent_button("change-folder", "选择目录").on_click(cx.listener(
                                    |app, _: &ClickEvent, _, cx| app.choose_folder(cx),
                                )),
                            ),
                    ),
            )
            .when(entries.is_empty(), |panel| {
                panel.child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_2xl()
                                        .text_color(rgb(FAINT))
                                        .child("↓"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(MUTED))
                                        .child("还没有下载任务"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(FAINT))
                                        .child("在音乐库中选择歌曲开始下载"),
                                ),
                        ),
                )
            })
            .when(!entries.is_empty(), |panel| {
                panel.child(
                    uniform_list(
                        "downloads",
                        total,
                        cx.processor(|app, range: std::ops::Range<usize>, _, cx| {
                            range
                                .map(|i| {
                                    let entry = app.downloads.entries()[i].clone();
                                    download_row(entry, i, cx)
                                })
                                .collect()
                        }),
                    )
                    .flex_1()
                    .min_h_0()
                    .w_full(),
                )
            })
    }
}

impl Render for Desktop {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shell(cx)
    }
}

// ---------------------------------------------------------------------------
// Track table
// ---------------------------------------------------------------------------

fn track_header(app: &Desktop, cx: &mut Context<Desktop>) -> Div {
    let state = &app.libraries[app.provider.index()];
    let visible = state.visible_tracks().len();
    let all_selected = visible > 0 && state.selected_tracks.len() == visible;
    div()
        .debug_selector(|| "track-header".to_owned())
        .flex_shrink_0()
        .w_full()
        .h(px(32.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .gap_3()
        .bg(rgb(BG))
        .border_b_1()
        .border_color(rgb(BORDER_SOFT))
        .child(
            checkbox("track-select-all", all_selected)
                .debug_selector(|| "track-select-all".to_owned())
                .on_click(cx.listener(|app, _: &ClickEvent, _, cx| app.select_all_tracks(cx))),
        )
        .child(
            div()
                .w(px(INDEX_W))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(FAINT))
                .child("#"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(rgb(FAINT))
                .child("歌曲"),
        )
        .child(
            div()
                .debug_selector(|| "track-time-header".to_owned())
                .w(px(TIME_W))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .text_xs()
                .text_color(rgb(FAINT))
                .child("时长"),
        )
        .child(
            div()
                .debug_selector(|| "track-action-header".to_owned())
                .w(px(ACTION_W))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .text_xs()
                .text_color(rgb(FAINT))
                .child("操作"),
        )
}

fn batch_bar(app: &Desktop, cx: &mut Context<Desktop>) -> Div {
    let count = app.libraries[app.provider.index()].selected_tracks.len();
    div()
        .debug_selector(|| "batch-bar".to_owned())
        .flex_shrink_0()
        .h(px(44.0))
        .px(px(16.0))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .bg(rgb(ACCENT_SOFT))
        .border_b_1()
        .border_color(rgb(BORDER_SOFT))
        .child(
            div()
                .debug_selector(|| "batch-count".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(rgb(ACCENT))
                .child(dot(ACCENT))
                .child(format!("已选 {count} 首")),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(ghost_button("clear-selection", "清除选择").on_click(
                    cx.listener(|app, _: &ClickEvent, _, cx| app.clear_track_selection(cx)),
                ))
                .child(
                    accent_button("batch-download", "加入下载").on_click(
                        cx.listener(|app, _: &ClickEvent, _, cx| app.download_selected(cx)),
                    ),
                ),
        )
}

fn checkbox(id: impl Into<gpui::ElementId>, checked: bool) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(16.0))
        .flex_shrink_0()
        .rounded(px(4.0))
        .border_1()
        .border_color(rgb(if checked { ACCENT } else { BORDER }))
        .bg(rgb(if checked { ACCENT } else { PANEL }))
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(rgb(WHITE))
        .when(checked, |box_| box_.child("✓"))
}

fn track_row(app: &mut Desktop, index: usize, cx: &mut Context<Desktop>) -> Div {
    let provider = app.provider;
    let state = &app.libraries[provider.index()];
    let visible = state.visible_tracks();
    let song = &visible[index];
    let selected = state.selected_tracks.contains(&index);
    let key = TrackKey::new(provider, song);
    let status = app.downloads.status(&key);
    let seconds = song.seconds;
    let number = index + 1;
    let info = if song.album.trim().is_empty() {
        song.artist.clone()
    } else {
        format!("{} · {}", song.artist, song.album)
    };

    let mut action = div()
        .debug_selector(move || format!("track-action-{index}"))
        .w(px(ACTION_W))
        .flex_shrink_0()
        .flex()
        .justify_end()
        .items_center();

    match status {
        None => {
            action = action.child(
                accent_button(("download", index), "下载")
                    .debug_selector(move || format!("track-download-{index}"))
                    .on_click(cx.listener(move |app, _: &ClickEvent, _, cx| app.save(index, cx))),
            );
        }
        Some(Status::Queued) => {
            action = action.child(chip("排队中", AMBER, AMBER_SOFT));
        }
        Some(Status::Running) => {
            action = action.child(
                div()
                    .debug_selector(move || format!("track-running-{index}"))
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(dot(AMBER))
                    .child(div().text_xs().text_color(rgb(AMBER)).child("下载中")),
            );
        }
        Some(Status::Completed(_)) => {
            action = action.child(chip("已下载", GREEN, GREEN_SOFT));
        }
        Some(Status::Failed(_)) => {
            let key = key.clone();
            action = action.child(
                ghost_button(("retry", index), "重试")
                    .text_color(rgb(AMBER))
                    .on_click(
                        cx.listener(move |app, _: &ClickEvent, _, cx| app.retry_download(&key, cx)),
                    ),
            );
        }
    }

    div()
        .debug_selector(move || format!("track-row-{index}"))
        .w_full()
        .h(px(TRACK_ROW_H))
        .px(px(12.0))
        .flex()
        .items_center()
        .gap_3()
        .bg(rgb(if selected { ACCENT_SOFT } else { PANEL }))
        .border_b_1()
        .border_color(rgb(BORDER_SOFT))
        .hover(|style| style.bg(rgb(BG)))
        .child(
            checkbox(("track-select", index), selected)
                .debug_selector(move || format!("track-select-{index}"))
                .on_click(cx.listener(move |app, _: &ClickEvent, _, cx| {
                    app.toggle_track_selection(index, cx)
                })),
        )
        .child(
            div()
                .w(px(INDEX_W))
                .flex_shrink_0()
                .text_xs()
                .text_color(rgb(FAINT))
                .child(format!("{number:02}")),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .child(song.name.clone()),
                )
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .child(info),
                ),
        )
        .child(
            div()
                .debug_selector(move || format!("track-time-{index}"))
                .w(px(TIME_W))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .text_xs()
                .text_color(rgb(MUTED))
                .child(format_duration(seconds)),
        )
        .child(action)
}

fn format_duration(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

// ---------------------------------------------------------------------------
// Download manager
// ---------------------------------------------------------------------------

fn download_row(entry: crate::downloads::Entry, index: usize, cx: &mut Context<Desktop>) -> Div {
    let key = entry.key.clone();
    let (tone, label, detail) = match &entry.status {
        Status::Queued => (MUTED, "排队中", "等待前面的任务完成，可随时取消".to_owned()),
        Status::Running => (AMBER, "下载中", "正在下载音频、封面与歌词…".to_owned()),
        Status::Completed(receipt) => (GREEN, "已完成", receipt.summary.replace('\n', " · ")),
        Status::Failed(error) => (RED, "失败", error.clone()),
    };
    let reveal = match &entry.status {
        Status::Completed(receipt) => Some(receipt.audio_path.clone()),
        _ => None,
    };

    let mut actions = div()
        .w(px(146.0))
        .flex_shrink_0()
        .flex()
        .justify_end()
        .items_center()
        .gap_2()
        .child(chip(
            label,
            tone,
            match &entry.status {
                Status::Completed(_) => GREEN_SOFT,
                Status::Failed(_) => RED_SOFT,
                Status::Running => AMBER_SOFT,
                Status::Queued => ELEVATED,
            },
        ));

    match &entry.status {
        Status::Queued => {
            actions = actions.child(ghost_button(("cancel-download", index), "取消").on_click(
                cx.listener(move |app, _: &ClickEvent, _, cx| app.cancel_download(&key, cx)),
            ));
        }
        Status::Failed(_) => {
            actions = actions.child(ghost_button(("retry-download", index), "重试").on_click(
                cx.listener(move |app, _: &ClickEvent, _, cx| app.retry_download(&key, cx)),
            ));
        }
        Status::Completed(_) => {
            if let Some(path) = reveal {
                actions =
                    actions.child(ghost_button(("reveal-download", index), "Finder").on_click(
                        cx.listener(move |_, _: &ClickEvent, _, cx| cx.reveal_path(&path)),
                    ));
            }
        }
        Status::Running => {}
    }

    div()
        .debug_selector(move || format!("download-row-{index}"))
        .w_full()
        .h(px(DOWNLOAD_ROW_H))
        .px(px(16.0))
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(rgb(BORDER_SOFT))
        .hover(|style| style.bg(rgb(BG)))
        .child(
            div()
                .w(px(3.0))
                .h(px(34.0))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgb(tone)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .text_color(rgb(TEXT))
                                .child(entry.title.clone()),
                        )
                        .child(
                            div()
                                .max_w(px(250.0))
                                .truncate()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .child(entry.artist.clone()),
                        )
                        .child(chip(entry.provider.label(), FAINT, ELEVATED)),
                )
                .child(
                    div()
                        .w_full()
                        .line_clamp(2)
                        .text_xs()
                        .text_color(rgb(if matches!(&entry.status, Status::Failed(_)) {
                            RED
                        } else {
                            MUTED
                        }))
                        .child(detail),
                ),
        )
        .child(actions)
}

// ---------------------------------------------------------------------------
// Reusable pieces
// ---------------------------------------------------------------------------

fn provider_dot(provider: Provider) -> Div {
    div()
        .size(px(8.0))
        .flex_shrink_0()
        .rounded_full()
        .bg(rgb(match provider {
            Provider::Qq => 0x2174b8,
            Provider::Netease => 0xc83d50,
        }))
}

fn initial(name: &str) -> String {
    name.chars()
        .next()
        .map_or_else(|| "?".to_owned(), |c| c.to_string())
}

fn dot(color: u32) -> Div {
    div()
        .size(px(6.0))
        .flex_shrink_0()
        .rounded_full()
        .bg(rgb(color))
}

fn section_label(text: &str) -> Div {
    div()
        .px_2()
        .pb_1()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(FAINT))
        .child(text.to_owned())
}

fn nav_row(id: impl Into<gpui::ElementId>, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(32.0))
        .px_2()
        .rounded_md()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .text_sm()
        .text_color(rgb(if selected { TEXT } else { MUTED }))
        .bg(rgb(if selected { ACCENT_SOFT } else { SIDEBAR }))
        .hover(|style| style.bg(rgb(HOVER)))
}

fn nav_glyph(glyph: &str, selected: bool) -> Div {
    div()
        .w(px(18.0))
        .flex_shrink_0()
        .flex()
        .justify_center()
        .text_sm()
        .text_color(rgb(if selected { ACCENT } else { FAINT }))
        .child(glyph.to_owned())
}

fn playlist_row(id: impl Into<gpui::ElementId>, selected: bool, name: &str) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(34.0))
        .w_full()
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .text_sm()
        .text_color(rgb(if selected { TEXT } else { MUTED }))
        .bg(rgb(if selected { ACCENT_SOFT } else { PANEL }))
        .hover(|style| style.bg(rgb(HOVER)))
        .child(
            div()
                .w(px(2.0))
                .h(px(14.0))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgb(if selected { ACCENT } else { PANEL })),
        )
        .child(div().min_w_0().truncate().child(name.to_owned()))
}

fn qr_card(qr: Arc<Image>) -> Div {
    div()
        .rounded_lg()
        .bg(rgb(WHITE))
        .border_1()
        .border_color(rgb(BORDER_SOFT))
        .p(px(12.0))
        .child(img(qr).size(px(208.0)))
}

fn qr_placeholder_visible(state: &Library, adapter_ready: bool) -> bool {
    state.authenticating && state.qr.is_none() && adapter_ready
}

fn note_text(message: String) -> Div {
    div().text_xs().text_color(rgb(MUTED)).child(message)
}

fn notice_bar(message: String) -> Div {
    div()
        .mx(px(18.0))
        .mt(px(14.0))
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(AMBER_SOFT))
        .border_1()
        .border_color(rgb(AMBER_BORDER))
        .flex()
        .items_center()
        .gap_2()
        .child(dot(AMBER))
        .child(div().text_xs().text_color(rgb(AMBER)).child(message))
}

fn chip(text: impl Into<String>, color: u32, background: u32) -> Div {
    div()
        .h(px(20.0))
        .flex_shrink_0()
        .px(px(7.0))
        .rounded_full()
        .bg(rgb(background))
        .flex()
        .items_center()
        .justify_center()
        .text_xs()
        .text_color(rgb(color))
        .child(text.into())
}

fn accent_button(id: impl Into<gpui::ElementId>, label: impl Into<String>) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.0))
        .px_3()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(rgb(ACCENT))
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(WHITE))
        .hover(|style| style.bg(rgb(ACCENT_HOVER)))
        .child(div().truncate().child(label.into()))
}

fn ghost_button(id: impl Into<gpui::ElementId>, label: impl Into<String>) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(28.0))
        .px_2()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(rgb(ELEVATED))
        .border_1()
        .border_color(rgb(BORDER))
        .text_xs()
        .text_color(rgb(MUTED))
        .hover(|style| style.bg(rgb(HOVER)).text_color(rgb(TEXT)))
        .child(div().truncate().child(label.into()))
}
