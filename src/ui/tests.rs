use gpui::{Focusable, KeyBinding, Modifiers, TestAppContext, px, size};

use super::*;

#[gpui::test]
fn search_input_accepts_typed_text_and_requires_login(cx: &mut TestAppContext) {
    cx.update(|app| {
        super::search_input::bind_keys(app);
        app.bind_keys([KeyBinding::new("enter", SubmitSearch, None)]);
    });
    let (view, cx) = cx.add_window_view(|_, cx| {
        let search_input = cx.new(super::search_input::SearchInput::new);
        Desktop {
            provider: Provider::Qq,
            libraries: Default::default(),
            search_input,
            music: None,
            startup_error: None,
            startup_warning: None,
            folder: None,
            show_downloads: false,
            downloads: Queue::default(),
            download_notice: None,
        }
    });

    let handle = cx.update(|_, app| view.read(app).search_input.read(app).focus_handle(app));
    cx.update(|window, _| window.focus(&handle));
    cx.simulate_input("稻香");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, app| view.read(app).search_input.read(app).content()),
        "稻香"
    );

    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, app| view.read(app).download_notice.clone()),
        Some("请先登录后再搜索".to_owned())
    );
}

#[gpui::test]
fn track_columns_stay_aligned_at_two_window_widths(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, cx| {
        let search_input = cx.new(super::search_input::SearchInput::new);
        let song = |id: &str, name: &str, artist: &str| Song {
            id: id.into(),
            mid: id.into(),
            media_mid: String::new(),
            name: name.into(),
            artist: artist.into(),
            album: "专辑".into(),
            cover: None,
            seconds: 209,
        };
        let tracks = vec![
            song("1", "短歌", "甲"),
            song("2", &"有很长的标题".repeat(30), "很长的歌手名字"),
        ];
        let mut downloads = Queue::default();
        assert!(downloads.enqueue(Provider::Qq, tracks[1].clone(), PathBuf::from("/unused")));
        assert!(downloads.next().is_some());
        Desktop {
            provider: Provider::Qq,
            libraries: [
                Library {
                    account: Some(Arc::new(Account {
                        name: "测试账号".into(),
                        id: "1".into(),
                        cookie: String::new(),
                    })),
                    playlists: vec![Playlist {
                        id: "1".into(),
                        name: "我的歌单".into(),
                        qq_dir: None,
                    }],
                    selected: Some(0),
                    tracks,
                    ..Library::default()
                },
                Library::default(),
            ],
            search_input,
            music: None,
            startup_error: None,
            startup_warning: None,
            folder: None,
            show_downloads: false,
            downloads,
            download_notice: None,
        }
    });

    let mut widths = Vec::new();
    for width in [760.0, 1060.0] {
        cx.simulate_resize(size(px(width), px(720.0)));
        cx.run_until_parked();
        let header = cx.debug_bounds("track-header").expect("table header");
        assert!(cx.debug_bounds("track-download-0").is_some());
        assert!(cx.debug_bounds("track-running-1").is_some());
        assert!(cx.debug_bounds("track-running-0").is_none());
        let first = cx.debug_bounds("track-row-0").expect("short track row");
        let second = cx.debug_bounds("track-row-1").expect("long track row");
        assert_eq!(first.origin.x, header.origin.x);
        assert_eq!(first.size.width, header.size.width);
        assert_eq!(second.size.width, header.size.width);
        for (header, first, second) in [
            ("track-time-header", "track-time-0", "track-time-1"),
            ("track-action-header", "track-action-0", "track-action-1"),
        ] {
            let header = cx.debug_bounds(header).expect("column header");
            assert_eq!(header.origin.x, cx.debug_bounds(first).unwrap().origin.x);
            assert_eq!(header.origin.x, cx.debug_bounds(second).unwrap().origin.x);
        }
        widths.push(first.size.width);
    }
    assert!(widths[1] > widths[0]);

    let select_first = cx.debug_bounds("track-select-0").unwrap().center();
    cx.simulate_click(select_first, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("batch-bar").is_some());
    assert_eq!(
        cx.update(|_, app| view.read(app).libraries[0].selected_tracks.len()),
        1
    );

    let select_second = cx.debug_bounds("track-select-1").unwrap().center();
    cx.simulate_click(select_second, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, app| view.read(app).libraries[0].selected_tracks.len()),
        2
    );

    let select_all = cx.debug_bounds("track-select-all").unwrap().center();
    cx.simulate_click(select_all, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.update(|_, app| view.read(app).libraries[0].selected_tracks.is_empty()));

    let manager = cx.debug_bounds("nav-downloads").unwrap().center();
    cx.simulate_click(manager, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("download-row-0").is_some());
    assert!(cx.update(|_, app| view.read(app).show_downloads));

    let library = cx.debug_bounds("nav-library").unwrap().center();
    cx.simulate_click(library, Modifiers::none());
    cx.run_until_parked();
    assert!(!cx.update(|_, app| view.read(app).show_downloads));
    assert!(cx.debug_bounds("track-row-1").is_some());
}
