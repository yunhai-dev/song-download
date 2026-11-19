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
        PollStep::SignedIn(cookie) => {
            match restore(m, &cookie).context("网易云已确认登录，但读取账号失败")? {
                Some(account) => Ok(Poll::SignedIn(account)),
                None => anyhow::bail!("网易云已确认登录，但登录凭据无效或为匿名会话，请重新扫码"),
            }
        }
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

/// `/user/account`：未登录时回 200 加空 profile；平台拒绝凭据时回 301/401。
pub(super) fn restore(m: &Music, cookie: &str) -> Result<Option<Account>> {
    let cookie = cookie.trim();
    // 没有 MUSIC_U 时请求层会自动补游客 MUSIC_A，那样的 200 绝不是登录账号。
    if !has_music_u(cookie) {
        return Ok(None);
    }
    let value = m.api(Provider::Netease, ROUTE_ACCOUNT, &[], cookie)?;
    account_from_response(&value, cookie)
}

fn has_music_u(cookie: &str) -> bool {
    cookie.split(';').any(|pair| {
        pair.split_once('=')
            .is_some_and(|(name, value)| name.trim() == "MUSIC_U" && !value.trim().is_empty())
    })
}

/// 只有包声明的「未登录/游客」应答才算 None；其他形状异常或上游故障必须冒泡，不能当成登出。
fn account_from_response(value: &Value, cookie: &str) -> Result<Option<Account>> {
    let code = platform_code(value).context("网易云账号信息返回异常：缺少状态码")?;
    match code {
        200 => {}
        301 | 401 => return Ok(None),
        other => anyhow::bail!("读取网易云账号失败（平台状态码：{other}）"),
    }

    let account = value.get("account");
    if account.is_some_and(anonymous_account) {
        return Ok(None);
    }
    let profile = match value.get("profile") {
        // 上游的未登录/游客应答是 200 加空 profile（null、缺失或空对象）。
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(profile)) if profile.is_empty() => return Ok(None),
        Some(Value::Object(profile)) => profile,
        Some(_) => anyhow::bail!("网易云账号信息返回异常：账号资料格式错误"),
    };
    if anonymous_profile(profile) {
        return Ok(None);
    }

    let id = profile.get("userId").map(text).unwrap_or_default();
    let id = id.trim();
    if id.is_empty() {
        anyhow::bail!("网易云账号信息返回异常：账号资料缺少账号标识");
    }
    if id == "0" {
        // 游客资料会带 userId 0；正常账号不会。
        return Ok(None);
    }
    let id = id.to_owned();

    let name = [
        profile.get("nickname").map(text).unwrap_or_default(),
        value
            .get("account")
            .and_then(|account| account.get("userName"))
            .map(text)
            .unwrap_or_default(),
    ]
    .into_iter()
    .find(|name| !name.trim().is_empty())
    .unwrap_or_else(|| id.clone());

    Ok(Some(Account {
        name,
        id,
        cookie: cookie.to_owned(),
    }))
}

fn anonymous_account(account: &Value) -> bool {
    account.as_object().is_some_and(anonymous_marker) || number(&account["type"]) == Some(0)
}

fn anonymous_profile(profile: &serde_json::Map<String, Value>) -> bool {
    anonymous_marker(profile)
}

fn anonymous_marker(object: &serde_json::Map<String, Value>) -> bool {
    ["anonimousUser", "anonymousUser", "anonymous"]
        .iter()
        .any(|key| object.get(*key).and_then(Value::as_bool) == Some(true))
}

pub(super) fn search(
    m: &Music,
    account: &Account,
    query: &str,
    offset: usize,
    limit: usize,
) -> Result<Page<Song>> {
    let query = query.trim();
    if query.is_empty() || limit == 0 {
        return Ok(Page {
            items: Vec::new(),
            more: false,
        });
    }
    let limit = limit.to_string();
    let offset = offset.to_string();
    let value = checked(
        m.api(
            Provider::Netease,
            "/search",
            &[
                ("keywords", query),
                ("type", "1"),
                ("limit", limit.as_str()),
                ("offset", offset.as_str()),
            ],
            &account.cookie,
        )?,
        200,
        "搜索网易云音乐",
    )?;
    let result = value
        .get("result")
        .filter(|result| result.is_object())
        .context("网易云搜索返回异常：缺少结果")?;
    search_page(result, offset.parse().unwrap_or(0))
}

fn search_page(result: &Value, offset: usize) -> Result<Page<Song>> {
    let songs = result
        .get("songs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let items = songs.iter().filter_map(song_from_value).collect::<Vec<_>>();
    let total = number(&result["songCount"]);
    let more = total.is_some_and(|total| offset as u64 + (songs.len() as u64) < total);
    Ok(Page { items, more })
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
    fn search_maps_song_count_and_items() {
        let result = json!({
            "songCount": 21,
            "songs": [{
                "id": 509781655,
                "name": "想你就写信",
                "duration": 238698,
                "artists": [{ "name": "周杰伦" }],
                "album": { "name": "中国新歌声", "picUrl": "http://p1.music.126.net/a.jpg" }
            }]
        });
        let page = search_page(&result, 0).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "509781655");
        assert_eq!(page.items[0].artist, "周杰伦");
        assert_eq!(page.items[0].seconds, 238);
        assert!(page.more);

        let last = search_page(&result, 20).unwrap();
        assert!(!last.more);
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

    #[test]
    fn restore_requires_a_login_music_u_cookie() {
        assert!(has_music_u("MUSIC_U=abc; __csrf=x"));
        assert!(has_music_u(" __csrf=x ; MUSIC_U = abc "));
        assert!(!has_music_u(""));
        assert!(!has_music_u("MUSIC_A=guest; __csrf=x"));
        assert!(!has_music_u("MUSIC_U= ; __csrf=x"));
        assert!(!has_music_u("XMUSIC_U=abc"));
    }

    #[test]
    fn account_response_only_treats_known_empty_answers_as_signed_out() {
        let signed_in = json!({
            "code": 200,
            "account": { "id": 7, "userName": "u", "type": 1 },
            "profile": { "userId": 7, "nickname": "甲" }
        });
        let account = account_from_response(&signed_in, "MUSIC_U=secret")
            .expect("valid session")
            .expect("account");
        assert_eq!(account.id, "7");
        assert_eq!(account.name, "甲");
        assert_eq!(account.cookie, "MUSIC_U=secret");

        // The package's logged-out answer.
        for body in [
            json!({ "code": 200, "account": null, "profile": null }),
            json!({ "code": 200 }),
            json!({ "code": 200, "account": null, "profile": {} }),
        ] {
            assert!(account_from_response(&body, "MUSIC_U=x").unwrap().is_none());
        }
        assert!(
            account_from_response(&json!({ "code": 301 }), "MUSIC_U=x")
                .unwrap()
                .is_none()
        );
        assert!(
            account_from_response(&json!({ "code": 401 }), "MUSIC_U=x")
                .unwrap()
                .is_none()
        );

        // Anonymous profiles are never accepted as a login.
        for body in [
            json!({ "code": 200, "account": { "id": 9, "type": 0 }, "profile": { "userId": 9 } }),
            json!({
                "code": 200,
                "account": { "id": 9, "type": 1 },
                "profile": { "userId": 9, "anonimousUser": true }
            }),
            json!({ "code": 200, "account": { "id": 9, "type": 1 }, "profile": { "userId": 0 } }),
        ] {
            assert!(account_from_response(&body, "MUSIC_U=x").unwrap().is_none());
        }

        // Anything else is a protocol failure, never a logout.
        for body in [
            json!({ "code": 200, "account": { "id": 9 }, "profile": "nope" }),
            json!({ "code": 200, "account": { "id": 9 }, "profile": { "nickname": "甲" } }),
            json!({ "code": 500 }),
            json!({}),
        ] {
            let error = account_from_response(&body, "MUSIC_U=secret")
                .err()
                .expect("malformed");
            assert!(!format!("{error:#}").contains("secret"));
        }
    }
}
