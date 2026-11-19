use gpui::{Modifiers, TestAppContext, px, size};

use super::*;

#[gpui::test]
fn track_columns_stay_aligned_at_two_window_widths(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_, _| {
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
            music: None,
            startup_error: None,
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
