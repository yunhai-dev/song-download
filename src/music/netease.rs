use super::*;

use anyhow::{Context, Result};

/// The bridge exposes the official package's exported methods under these routes.
const ROUTE_QR_KEY: &str = "/login/qr/key";
const ROUTE_QR_CREATE: &str = "/login/qr/create";
const ROUTE_QR_CHECK: &str = "/login/qr/check";
const ROUTE_ACCOUNT: &str = "/user/account";
const ROUTE_PLAYLISTS: &str = "/user/playlist";
const ROUTE_PLAYLIST_TRACKS: &str = "/playlist/track/all";
const ROUTE_SONG_URL: &str = "/song/url/v1";
const ROUTE_LYRIC: &str = "/lyric/new";

/// `/user/playlist` is read whole here; the API takes at most this many entries per page.
const PLAYLIST_PAGE: usize = 1000;

// Request exhigh without enabling any unblocking behavior.
const AUDIO_LEVEL: &str = "exhigh";

pub(super) fn qr(m: &Music) -> Result<Qr> {
    let key_response = m.api(Provider::Netease, ROUTE_QR_KEY, &[], "")?;
    // login_qr_key wraps NetEase's own body in `data`.
    let key = text(&key_response["data"]["unikey"]).trim().to_owned();
    if key.is_empty() {
        anyhow::bail!(
            "网易云未返回二维码标识（状态码：{}）",
            status_text(&key_response["data"]["code"])
        );
    }

    let image_response = m.api(
        Provider::Netease,
        ROUTE_QR_CREATE,
        &[("key", key.as_str()), ("qrimg", "true")],
        "",
    )?;
    let image = text(&image_response["data"]["qrimg"]).trim().to_owned();
    if image.is_empty() {
        anyhow::bail!(
            "网易云未返回二维码图片（状态码：{}）",
            status_text(&image_response["code"])
        );
    }

    Ok(Qr { key, image })
}

pub(super) fn poll(m: &Music, key: &str) -> Result<Poll> {
    let response = m.api(Provider::Netease, ROUTE_QR_CHECK, &[("key", key)], "")?;
    match poll_step(&response)? {
        PollStep::Waiting => Ok(Poll::Waiting),
        PollStep::Scanned => Ok(Poll::Scanned),
        PollStep::Expired => Ok(Poll::Expired),
        PollStep::SignedIn(cookie) => read_account(m, &cookie)
            .map(Poll::SignedIn)
            .context("网易云已确认登录，但读取账号失败"),
    }
}

pub(super) fn playlists(m: &Music, account: &Account) -> Result<Vec<Playlist>> {
    let mut playlists = Vec::new();
    let mut offset = 0usize;

    // The API answers `more`; the page length is also respected so a malformed answer cannot loop.
    for _ in 0..64 {
        let limit = PLAYLIST_PAGE.to_string();
        let offset_text = offset.to_string();
        let value = checked(
            m.api(
                Provider::Netease,
                ROUTE_PLAYLISTS,
                &[
                    ("uid", account.id.as_str()),
                    ("limit", limit.as_str()),
                    ("offset", offset_text.as_str()),
                ],
                &account.cookie,
            )?,
            200,
            "获取网易云歌单",
        )?;

        let batch = value
            .get("playlist")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if batch.is_empty() {
            break;
        }

        for raw in batch {
            let id = text(&raw["id"]).trim().to_owned();
            if id.is_empty() {
                continue;
            }
            playlists.push(Playlist {
                id,
                name: text(&raw["name"]),
                qq_dir: None,
            });
        }

        offset = offset.saturating_add(batch.len());
        let more = more_flag(&value, batch.len() >= PLAYLIST_PAGE);
        if !more {
            break;
        }
    }

    Ok(playlists)
}

pub(super) fn tracks(
    m: &Music,
    account: &Account,
    playlist: &Playlist,
    offset: usize,
    limit: usize,
) -> Result<Page<Song>> {
    if limit == 0 {
        return Ok(Page {
            items: Vec::new(),
            more: false,
        });
    }

    // Ask for one more song than the page needs: a full page alone cannot tell the last page from
    // one that merely happens to end on the limit, and `playlist_track_all` reports no total.
    let fetch = limit.saturating_add(1).to_string();
    let offset_text = offset.to_string();
    let value = checked(
        m.api(
            Provider::Netease,
            ROUTE_PLAYLIST_TRACKS,
            &[
                ("id", playlist.id.as_str()),
                ("limit", fetch.as_str()),
                ("offset", offset_text.as_str()),
            ],
            &account.cookie,
        )?,
        200,
        "获取网易云歌单歌曲",
    )?;

    let songs = value
        .get("songs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    Ok(page_from_songs(songs, limit))
}

pub(super) fn audio(m: &Music, account: &Account, song: &Song) -> Result<Audio> {
    let value = checked(
        m.api(
            Provider::Netease,
            ROUTE_SONG_URL,
            &[("id", song.id.as_str()), ("level", AUDIO_LEVEL)],
            &account.cookie,
        )?,
        200,
        "获取网易云音频",
    )?;

    let list = value
        .get("data")
        .and_then(Value::as_array)
        .context("网易云音频响应无效")?;
    let raw = list
        .iter()
        .find(|item| text(&item["id"]) == song.id)
        .context("网易云未返回该歌曲的音频地址")?;
    audio_from_value(raw, &song.id)
}

pub(super) fn lyric(m: &Music, account: &Account, song: &Song) -> Result<Option<Lyrics>> {
    let value = checked(
        m.api(
            Provider::Netease,
            ROUTE_LYRIC,
            &[("id", song.id.as_str())],
            &account.cookie,
        )?,
        200,
        "获取网易云歌词",
    )?;
    Ok(lyric_from_value(&value))
}

enum PollStep {
    Waiting,
    Scanned,
    Expired,
    SignedIn(String),
}

fn poll_step(value: &Value) -> Result<PollStep> {
    match platform_code(value) {
        Some(800) => Ok(PollStep::Expired),
        Some(801) => Ok(PollStep::Waiting),
        Some(802) => Ok(PollStep::Scanned),
        Some(803) => {
            let cookie = text(&value["cookie"]).trim().to_owned();
            if cookie.is_empty() {
                anyhow::bail!("网易云已确认登录，但未返回登录凭据");
            }
            Ok(PollStep::SignedIn(cookie))
        }
        Some(code) => {
            let message = response_message(value);
            let suffix = if message.is_empty() {
                String::new()
            } else {
                format!("：{message}")
            };
            anyhow::bail!("网易云扫码失败（状态码：{code}）{suffix}");
        }
        None => anyhow::bail!(
            "网易云扫码状态无效（状态码：{}）",
            status_text(&value["code"])
        ),
    }
}

fn read_account(m: &Music, cookie: &str) -> Result<Account> {
    let value = checked(
        m.api(Provider::Netease, ROUTE_ACCOUNT, &[], cookie)?,
        200,
        "读取网易云账号",
    )?;
    let profile = &value["profile"];

    let id = [
        text(&value["account"]["id"]),
        text(&profile["userId"]),
        text(&profile["id"]),
    ]
    .into_iter()
    .find(|id| !id.trim().is_empty() && id.trim() != "0")
    .context("网易云未返回账号标识")?;

    let name = [
        text(&profile["nickname"]),
        text(&value["account"]["userName"]),
    ]
    .into_iter()
    .find(|name| !name.trim().is_empty())
    .unwrap_or_else(|| id.clone());

    Ok(Account {
        name,
        id,
        cookie: cookie.to_owned(),
    })
}

fn page_from_songs(songs: &[Value], limit: usize) -> Page<Song> {
    let items = songs
        .iter()
        .take(limit)
        .filter_map(song_from_value)
        .collect();
    Page {
        items,
        more: songs.len() > limit,
    }
}

fn song_from_value(raw: &Value) -> Option<Song> {
    let id = text(&raw["id"]).trim().to_owned();
    if id.is_empty() {
        return None;
    }

    let album = raw.get("album").or_else(|| raw.get("al"));
    let cover = album
        .and_then(|album| album.get("picUrl").or_else(|| album.get("coverUrl")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| https_url(url.to_owned()));

    let seconds = number(&raw["duration"])
        .filter(|millis| *millis > 0)
        .or_else(|| number(&raw["dt"]).filter(|millis| *millis > 0))
        .map_or(0, |millis| millis / 1000);

    Some(Song {
        // NetEase identifies a song by one id only; both mid fields carry it so callers have the
        // same shape as the other provider.
        mid: id.clone(),
        media_mid: id.clone(),
        id,
        name: text(&raw["name"]),
        artist: artist_names(raw),
        album: album.map(|album| text(&album["name"])).unwrap_or_default(),
        cover,
        seconds,
    })
}

fn artist_names(raw: &Value) -> String {
    raw.get("artists")
        .or_else(|| raw.get("ar"))
        .and_then(Value::as_array)
        .map(|artists| {
            artists
                .iter()
                .map(|artist| text(&artist["name"]))
                .filter(|name| !name.trim().is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn audio_from_value(raw: &Value, song_id: &str) -> Result<Audio> {
    // A trial is an object carrying where the snippet sits in the whole song. A whole song has
    // `null`; the package's unblock option writes the string "null", which is not a trial either.
    if raw.get("freeTrialInfo").is_some_and(Value::is_object) {
        anyhow::bail!("网易云只返回了试听片段，当前账号无权下载整首（歌曲 ID：{song_id}）");
    }

    let url = text(&raw["url"]).trim().to_owned();
    if url.is_empty() {
        anyhow::bail!(
            "网易云未返回整首音频地址（歌曲 ID：{song_id}，状态码：{}，付费标记：{}）",
            status_text(&raw["code"]),
            status_text(&raw["fee"])
        );
    }
    let url = https_url(url);

    let ext = audio_ext(&text(&raw["type"]), &url)
        .with_context(|| format!("网易云返回了无法识别的音频格式（歌曲 ID：{song_id}）"))?;
    Ok(Audio { url, ext })
}

fn lyric_from_value(value: &Value) -> Option<Lyrics> {
    // `lyric_new` returns NetEase's body as is; a wrapped answer is accepted too.
    let source = if value.get("yrc").is_some() || value.get("lrc").is_some() {
        value
    } else {
        &value["data"]
    };

    // Word-by-word (YRC) first; a normal timed lyric only when the song has no YRC. Nothing is
    // derived from one to make the other.
    lyrics(&source["yrc"]["lyric"], "yrc")
        .or_else(|| lyrics(&source["lrc"]["yrc"]["lyric"], "yrc"))
        .or_else(|| lyrics(&source["lrc"]["lyric"], "lrc"))
        .or_else(|| lyrics(&source["lyric"], "lrc"))
}

fn audio_ext(kind: &str, url: &str) -> Option<&'static str> {
    let mut name = kind.trim().trim_start_matches('.').to_ascii_lowercase();
    if name.is_empty() {
        name = url_extension(url);
    }

    match name.as_str() {
        "mp3" => Some("mp3"),
        "flac" => Some("flac"),
        "m4a" => Some("m4a"),
        "aac" => Some("aac"),
        "wav" => Some("wav"),
        "ogg" => Some("ogg"),
        "opus" => Some("opus"),
        "ape" => Some("ape"),
        "wma" => Some("wma"),
        _ => None,
    }
}

fn url_extension(url: &str) -> String {
    let path = url
        .split(|c| c == '?' || c == '#')
        .next()
        .unwrap_or_default();
    path.rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

fn https_url(url: String) -> String {
    if let Some(rest) = url.strip_prefix("http://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        url
    }
}

fn more_flag(value: &Value, fallback: bool) -> bool {
    match value.get("more") {
        Some(Value::Bool(more)) => *more,
        Some(Value::Number(more)) => more.as_u64().is_some_and(|more| more != 0),
        Some(Value::String(more)) => matches!(more.trim(), "true" | "1"),
        Some(_) => false,
        None => fallback,
    }
}

fn platform_code(value: &Value) -> Option<i64> {
    let code = value.get("code")?;
    code.as_i64()
        .or_else(|| code.as_str()?.trim().parse::<i64>().ok())
}

fn response_message(value: &Value) -> String {
    let message = text(&value["message"]);
    if message.trim().is_empty() {
        text(&value["msg"]).trim().to_owned()
    } else {
        message.trim().to_owned()
    }
}

fn status_text(value: &Value) -> String {
    let status = text(value);
    if status.trim().is_empty() {
        "缺失".to_owned()
    } else {
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_song_identity_album_cover_and_duration() {
        let song = song_from_value(&json!({
            "id": 123456,
            "name": "一首歌",
            "artists": [{ "name": "甲" }, { "name": "乙" }],
            "album": { "name": "专辑", "picUrl": "http://p1.music.126.net/cover.jpg" },
            "duration": 245678
        }))
        .expect("song");

        assert_eq!(song.id, "123456");
        assert_eq!(song.mid, "123456");
        assert_eq!(song.media_mid, "123456");
        assert_eq!(song.name, "一首歌");
        assert_eq!(song.artist, "甲, 乙");
        assert_eq!(song.album, "专辑");
        assert_eq!(
            song.cover.as_deref(),
            Some("https://p1.music.126.net/cover.jpg")
        );
        assert_eq!(song.seconds, 245);
    }

    #[test]
    fn track_page_looks_one_song_ahead_at_the_boundary() {
        let songs = vec![json!({ "id": 1 }), json!({ "id": 2 })];
        let page = page_from_songs(&songs, 2);
        assert_eq!(page.items.len(), 2);
        assert!(!page.more);

        let page = page_from_songs(&songs, 1);
        assert_eq!(page.items.len(), 1);
        assert!(page.more);

        let page = page_from_songs(&[json!({}), json!({ "id": 2 })], 1);
        assert!(page.items.is_empty());
        assert!(page.more);

        let page = page_from_songs(&[], 1);
        assert!(page.items.is_empty());
        assert!(!page.more);
    }

    #[test]
    fn refuses_trial_audio_and_keeps_whole_song_extension() {
        let trial = json!({
            "id": 1,
            "url": "http://m10.music.126.net/trial.mp3",
            "type": "mp3",
            "freeTrialInfo": { "start": 0, "end": 30 }
        });
        let error = audio_from_value(&trial, "1")
            .err()
            .expect("trial must fail");
        assert!(format!("{error:#}").contains("试听"));

        let whole = json!({
            "id": 1,
            "url": "http://m10.music.126.net/whole.flac",
            "type": "flac",
            "freeTrialInfo": null
        });
        let audio = audio_from_value(&whole, "1").expect("whole song");
        assert_eq!(audio.url, "https://m10.music.126.net/whole.flac");
        assert_eq!(audio.ext, "flac");

        let from_url = json!({
            "id": 1,
            "url": "https://m10.music.126.net/whole.ogg?token=x",
            "type": "",
            "freeTrialInfo": null
        });
        assert_eq!(audio_from_value(&from_url, "1").unwrap().ext, "ogg");
    }

    #[test]
    fn lyric_prefers_yrc_then_falls_back_to_lrc() {
        let both = json!({
            "yrc": { "lyric": "[0,100](0,100,0)逐字" },
            "lrc": { "lyric": "[00:00.00]整行" }
        });
        let chosen = lyric_from_value(&both).expect("yrc");
        assert_eq!(chosen.ext, "yrc");
        assert_eq!(chosen.text, "[0,100](0,100,0)逐字");

        let lrc_only = json!({
            "yrc": { "lyric": "  " },
            "lrc": { "lyric": "[00:00.00]整行" }
        });
        let chosen = lyric_from_value(&lrc_only).expect("lrc");
        assert_eq!(chosen.ext, "lrc");
        assert_eq!(chosen.text, "[00:00.00]整行");

        assert!(lyric_from_value(&json!({ "nolyric": true })).is_none());
    }

    #[test]
    fn maps_qr_states_including_confirmed_cookie() {
        assert!(matches!(
            poll_step(&json!({ "code": 800 })).unwrap(),
            PollStep::Expired
        ));
        assert!(matches!(
            poll_step(&json!({ "code": 801 })).unwrap(),
            PollStep::Waiting
        ));
        assert!(matches!(
            poll_step(&json!({ "code": 802 })).unwrap(),
            PollStep::Scanned
        ));
        match poll_step(&json!({ "code": 803, "cookie": "MUSIC_U=abc" })).unwrap() {
            PollStep::SignedIn(cookie) => assert_eq!(cookie, "MUSIC_U=abc"),
            _ => panic!("confirmed login expected"),
        }
        assert!(poll_step(&json!({ "code": 803, "cookie": "" })).is_err());
        assert!(poll_step(&json!({})).is_err());
    }
}
