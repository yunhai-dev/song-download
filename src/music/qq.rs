use super::*;

use anyhow::anyhow;
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;

/// QQ 音乐官方专辑封面地址模板（T002 是专辑，尺寸 300）。
const QQ_ALBUM_COVER: &str = "https://y.gtimg.cn/music/photo_new/T002R300x300M000";
/// Folia Major 读 QRC 用的公开 musicu 入口，不经过本地适配器，也不带登录凭据。
const QQ_MUSICU: &str = "https://u.y.qq.com/cgi-bin/musicu.fcg";

fn require_code(value: Value, code: u64, operation: &str) -> anyhow::Result<Value> {
    if number(&value["code"]) == Some(401) {
        return Err(anyhow!("QQ 音乐会话已失效，请重新扫码"));
    }
    checked(value, code, operation)
}

fn bridge(m: &Music, route: &str, params: &[(&str, &str)], cookie: &str) -> anyhow::Result<Value> {
    m.api(Provider::Qq, route, params, cookie)
        .map_err(|error| anyhow!("请求QQ音乐本地适配器失败（{route}）：{error}"))
}

fn api(
    m: &Music,
    account: &Account,
    route: &str,
    params: &[(&str, &str)],
) -> anyhow::Result<Value> {
    bridge(m, route, params, &account.cookie)
}

fn field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let object = value.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key).filter(|entry| !entry.is_null()))
}

fn string_field(value: &Value, keys: &[&str]) -> String {
    field(value, keys).map(text).unwrap_or_default()
}

fn nonempty_field(value: &Value, keys: &[&str]) -> Option<String> {
    let value = string_field(value, keys);
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn object_at<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.get(key).filter(|entry| entry.is_object())
}

/// `/login/status` 的 profile 里，账号标识在顶层和 `info` 里都可能出现；占位值 `0` 不算。
fn account_id(profile: &Value) -> String {
    let read = |source: &Value| -> Option<String> {
        for key in ["str_musicid", "musicid", "uin", "id"] {
            let Some(value) = source.get(key) else {
                continue;
            };
            let value = text(value);
            if !value.is_empty() && value != "0" {
                return Some(value);
            }
        }
        None
    };
    read(profile)
        .or_else(|| profile.get("info").and_then(read))
        .unwrap_or_default()
}

fn account_name(profile: &Value) -> String {
    let read = |source: &Value| -> Option<String> {
        nonempty_field(source, &["nickname", "nick", "name", "userName"])
    };
    read(profile)
        .or_else(|| profile.get("info").and_then(read))
        .unwrap_or_else(|| "QQ音乐用户".to_string())
}

/// `/login/status`：包在凭证不可用时返回 200 加空 `data`，凭证被上游明确拒绝时返回 301/401。
pub(super) fn restore(m: &Music, cookie: &str) -> anyhow::Result<Option<Account>> {
    let cookie = cookie.trim();
    if cookie.is_empty() {
        return Ok(None);
    }
    let body = bridge(m, "/login/status", &[], cookie)?;
    account_from_status(&body, cookie)
}

/// 只有包声明的「未登录」应答才算 None；其他形状异常或上游故障必须冒泡，不能当成登出。
fn account_from_status(body: &Value, cookie: &str) -> anyhow::Result<Option<Account>> {
    let code = body
        .get("code")
        .and_then(number)
        .ok_or_else(|| anyhow!("QQ音乐登录状态返回异常：缺少状态码"))?;
    match code {
        200 => {}
        301 | 401 => return Ok(None),
        other => anyhow::bail!("QQ音乐登录状态返回异常（平台状态码：{other}）"),
    }

    let data = body
        .get("data")
        .filter(|data| data.is_object())
        .ok_or_else(|| anyhow!("QQ音乐登录状态返回异常：缺少账号数据"))?;
    let Some(profile) = data.get("profile") else {
        // 登录状态始终 200；没有 profile 就是包的「未登录」应答。
        return Ok(None);
    };
    if !profile.is_object() {
        anyhow::bail!("QQ音乐登录状态返回异常：账号资料格式错误");
    }

    let id = account_id(profile);
    if id.is_empty() {
        anyhow::bail!("QQ音乐登录状态返回异常：账号资料缺少账号标识");
    }
    Ok(Some(Account {
        name: account_name(profile),
        id,
        cookie: cookie.to_string(),
    }))
}

pub(super) fn qr(m: &Music) -> anyhow::Result<Qr> {
    // QQ 音乐 App 扫码通道：`qqmusic` 是包内声明的通道名。
    let key_body = bridge(m, "/login/qr/key", &[("channel", "qqmusic")], "")?;
    let key_body = require_code(key_body, 200, "获取QQ音乐登录二维码")?;
    let key = key_body
        .pointer("/data/unikey")
        .map(text)
        .or_else(|| key_body.pointer("/data/key").map(text))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("QQ音乐登录二维码生成失败：未返回二维码标识"))?;

    let image_body = bridge(m, "/login/qr/create", &[("key", &key)], "")?;
    let image_body = require_code(image_body, 200, "获取QQ音乐登录二维码图片")?;
    let image = image_body
        .pointer("/data/qrimg")
        .map(text)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("QQ音乐登录二维码生成失败：未返回二维码图片"))?;

    Ok(Qr { key, image })
}

pub(super) fn poll(m: &Music, key: &str) -> anyhow::Result<Poll> {
    let body = bridge(m, "/login/qr/check", &[("key", key)], "")?;
    let code = body
        .get("code")
        .and_then(number)
        .ok_or_else(|| anyhow!("QQ音乐登录状态返回异常：缺少状态码"))?;

    match code {
        801 => Ok(Poll::Waiting),
        802 => Ok(Poll::Scanned),
        800 => Ok(Poll::Expired),
        803 => {
            let cookie = string_field(&body, &["cookie"]);
            if cookie.trim().is_empty() {
                return Err(anyhow!(
                    "QQ音乐登录确认成功，但本地适配器未返回会话，请重新扫码"
                ));
            }
            match restore(m, cookie.trim())? {
                Some(account) => Ok(Poll::SignedIn(account)),
                None => Err(anyhow!("QQ音乐登录确认成功，但会话无效，请重新扫码登录")),
            }
        }
        other => Err(anyhow!("QQ音乐登录状态返回未知状态码：{other}")),
    }
}

fn playlist_from(item: &Value) -> Option<Playlist> {
    if !item.is_object() {
        return None;
    }
    // 自建歌单由 GetPlaylistByUin 给出 `dirName`；收藏条目的字段是 name/songnum/orderTime。
    let owned = item.get("dirName").is_some();
    let dir = field(item, &["dirId", "dirid"]).and_then(number);
    let mut id = string_field(item, &["tid", "dissid", "id", "dirId", "dirid"]);
    if id.is_empty() {
        id = dir.map(|dir| dir.to_string()).unwrap_or_default();
    }
    if id.trim().is_empty() {
        return None;
    }
    let name = nonempty_field(item, &["dirName", "dirname", "dissname", "title", "name"])
        .unwrap_or_else(|| "未命名歌单".to_string());
    Some(Playlist {
        id,
        name,
        // 只有自建歌单才走带凭据的目录详情；收藏歌单的 dirId 是创建者的目录号，不能拿来读。
        qq_dir: if owned {
            dir.filter(|dir| *dir > 0)
        } else {
            None
        },
    })
}

pub(super) fn playlists(m: &Music, account: &Account) -> anyhow::Result<Vec<Playlist>> {
    let mut offset: u64 = 0;
    let mut seen = HashSet::new();
    let mut playlists = Vec::new();
    for _ in 0..64 {
        let offset_str = offset.to_string();
        let params = [("favoriteOffset", offset_str.as_str())];
        let body = api(
            m,
            account,
            "/user/playlist",
            if offset == 0 { &[] } else { &params },
        )?;
        let body = require_code(body, 200, "读取QQ音乐歌单")?;
        let list = body["playlist"]
            .as_array()
            .context("QQ音乐歌单列表返回异常：缺少歌单数据")?;
        for item in list {
            if let Some(playlist) = playlist_from(item).filter(|p| seen.insert(p.id.clone())) {
                playlists.push(playlist);
            }
        }
        match number(&body["nextFavoriteOffset"]) {
            Some(next) if next > offset => offset = next,
            _ => break,
        }
    }
    Ok(playlists)
}

fn song_from(item: &Value) -> Option<Song> {
    if !item.is_object() {
        return None;
    }
    let mid = nonempty_field(item, &["mid", "songmid", "songMid"])?;
    let album = object_at(item, "album");
    let file = object_at(item, "file");

    let id = nonempty_field(item, &["songid", "songId", "id"])
        .filter(|id| id != "0")
        .unwrap_or_else(|| mid.clone());
    let album_mid = album
        .and_then(|album| nonempty_field(album, &["mid", "albummid", "albumMid", "albumMID"]))
        .or_else(|| nonempty_field(item, &["albummid", "albumMid", "albumMID"]))
        .unwrap_or_default();
    let media_mid = file
        .and_then(|file| nonempty_field(file, &["media_mid", "mediaMid"]))
        .or_else(|| nonempty_field(item, &["media_mid", "mediaMid", "media_mid"]))
        .unwrap_or_else(|| mid.clone());

    let name = nonempty_field(item, &["title", "name", "songname"])
        .unwrap_or_else(|| "未知歌曲".to_string());
    let artist = item
        .get("singer")
        .or_else(|| item.get("singers"))
        .or_else(|| item.get("artists"))
        .and_then(Value::as_array)
        .map(|singers| {
            singers
                .iter()
                .filter_map(|singer| nonempty_field(singer, &["name", "title", "singername"]))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|artist| !artist.is_empty())
        .unwrap_or_default();
    let album_name = album
        .and_then(|album| nonempty_field(album, &["name", "title", "albumname"]))
        .or_else(|| nonempty_field(item, &["albumname", "albumName"]))
        .unwrap_or_default();

    let cover = (!album_mid.is_empty())
        .then(|| format!("{QQ_ALBUM_COVER}{album_mid}.jpg?max_age=2592000"))
        .or_else(|| {
            album
                .and_then(|album| nonempty_field(album, &["coverUrl", "picUrl"]))
                .or_else(|| nonempty_field(item, &["coverUrl", "picUrl"]))
                .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
        });

    let seconds = field(item, &["interval", "duration"])
        .and_then(number)
        .or_else(|| {
            field(item, &["durationMs", "duration_ms"])
                .and_then(number)
                .map(|millis| millis / 1000)
        })
        .unwrap_or(0);

    Some(Song {
        id,
        mid,
        media_mid,
        name,
        artist,
        album: album_name,
        cover,
        seconds,
    })
}

fn user_song_page(body: &Value, offset: usize) -> Page<Song> {
    let songs = body
        .get("songs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let items: Vec<Song> = songs.iter().filter_map(song_from).collect();
    let total = body.get("total").and_then(number);
    let flag = body.get("more").and_then(Value::as_bool).unwrap_or(false)
        || body.get("more").and_then(number) == Some(1);
    let consumed = offset as u64 + songs.len() as u64;
    let more = flag || total.map(|total| consumed < total).unwrap_or(false);
    Page { items, more }
}

fn page_from_list(songs: &[Value], offset: usize, limit: usize) -> Page<Song> {
    let items: Vec<Song> = songs
        .iter()
        .skip(offset)
        .take(limit)
        .filter_map(song_from)
        .collect();
    let taken = limit.min(songs.len().saturating_sub(offset));
    Page {
        items,
        more: offset + taken < songs.len(),
    }
}

pub(super) fn tracks(
    m: &Music,
    account: &Account,
    playlist: &Playlist,
    offset: usize,
    limit: usize,
) -> anyhow::Result<Page<Song>> {
    if playlist.id.trim().is_empty() {
        return Err(anyhow!("歌单标识为空，无法读取歌曲"));
    }
    let limit = limit.max(1);

    match playlist.qq_dir {
        // 「我喜欢」走独立路由；其他自建歌单走带凭据的目录详情，不公开的歌单也读得到。
        Some(dir) if dir == 201 => {
            let offset = offset.to_string();
            let limit = limit.to_string();
            let body = api(
                m,
                account,
                "/user/liked-songs",
                &[("offset", offset.as_str()), ("limit", limit.as_str())],
            )?;
            let body = require_code(body, 200, "读取QQ音乐“我喜欢”")?;
            Ok(user_song_page(&body, offset.parse().unwrap_or(0)))
        }
        Some(dir) => {
            let dir = dir.to_string();
            let offset = offset.to_string();
            let limit = limit.to_string();
            let mut params: Vec<(&str, &str)> = vec![
                ("dirid", dir.as_str()),
                ("offset", offset.as_str()),
                ("limit", limit.as_str()),
            ];
            if playlist.id.bytes().all(|byte| byte.is_ascii_digit()) {
                params.push(("tid", playlist.id.as_str()));
            }
            let body = api(m, account, "/user/playlist-detail", &params)?;
            let body = require_code(body, 200, "读取QQ音乐自建歌单")?;
            Ok(user_song_page(&body, offset.parse().unwrap_or(0)))
        }
        None => {
            let body = api(
                m,
                account,
                "/getSongListDetail",
                &[("disstid", &playlist.id)],
            )?;
            if let Some(error) = body.get("error") {
                return Err(anyhow!("读取QQ音乐歌单失败：{}", text(error)));
            }
            let response = body.get("response").unwrap_or(&body);
            let detail = response
                .get("cdlist")
                .and_then(Value::as_array)
                .and_then(|list| list.first())
                .ok_or_else(|| {
                    anyhow!("无法读取该QQ音乐歌单：可能不是公开歌单，或当前账号无权访问")
                })?;
            let songs = detail
                .get("songlist")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(page_from_list(&songs, offset, limit))
        }
    }
}

pub(super) fn search(
    m: &Music,
    account: &Account,
    query: &str,
    offset: usize,
    limit: usize,
) -> anyhow::Result<Page<Song>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Page {
            items: Vec::new(),
            more: false,
        });
    }
    let limit = limit.clamp(1, 20);
    let page = offset / limit + 1;
    let limit = limit.to_string();
    let page = page.to_string();
    let body = api(
        m,
        account,
        "/search",
        &[
            ("keyword", query),
            ("type", "song"),
            ("page", page.as_str()),
            ("limit", limit.as_str()),
        ],
    )?;
    let body = require_code(body, 200, "搜索QQ音乐")?;
    search_page(&body, offset)
}

fn search_page(body: &Value, offset: usize) -> anyhow::Result<Page<Song>> {
    let songs = body["items"]
        .as_array()
        .context("QQ音乐搜索返回异常：缺少歌曲列表")?;
    let items = songs.iter().filter_map(song_from).collect::<Vec<_>>();
    let total = number(&body["total"]);
    let more = body["more"].as_bool().unwrap_or(false)
        || total.is_some_and(|total| offset as u64 + (songs.len() as u64) < total);
    Ok(Page { items, more })
}

fn audio_ext(url: &str) -> &'static str {
    let path = url.split(|ch| ch == '?' || ch == '#').next().unwrap_or(url);
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".m4a") {
        "m4a"
    } else if lower.ends_with(".flac") {
        "flac"
    } else if lower.ends_with(".ape") {
        "ape"
    } else if lower.ends_with(".ogg") {
        "ogg"
    } else {
        // 标准音质（128k）是 mp3；地址不带扩展名时按同一档处理。
        "mp3"
    }
}

pub(super) fn audio(m: &Music, account: &Account, song: &Song) -> anyhow::Result<Audio> {
    if song.mid.trim().is_empty() {
        return Err(anyhow!("歌曲缺少 songmid，无法获取音频"));
    }
    let mut params: Vec<(&str, &str)> = vec![
        ("songmid", song.mid.as_str()),
        // 只要标准音质的整首音频；不请求 preview，也不接受返回值里的试听标记。
        ("quality", "128"),
    ];
    if !song.media_mid.trim().is_empty() && song.media_mid != song.mid {
        params.push(("mediaId", song.media_mid.as_str()));
    }
    let body = api(m, account, "/getMusicPlay", &params)?;
    if number(&body["code"]) == Some(401) {
        return Err(anyhow!("QQ 音乐会话已失效，请重新扫码"));
    }
    let play_url = body
        .pointer("/data/playUrl")
        .or_else(|| body.get("playUrl"))
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("QQ音乐未返回播放链接"))?;
    let entry = play_url
        .get(&song.mid)
        .ok_or_else(|| anyhow!("QQ音乐未返回这首歌曲的播放链接"))?;

    if entry.get("preview").and_then(Value::as_bool) == Some(true) {
        return Err(anyhow!("QQ音乐只返回了试听片段，已拒绝使用"));
    }

    let url = match entry {
        Value::String(url) => url.trim().to_string(),
        Value::Object(_) => string_field(entry, &["url"]).trim().to_string(),
        _ => String::new(),
    };
    if url.is_empty() {
        let reason = field(entry, &["error", "message"])
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(str::to_string);
        return Err(anyhow!(
            "无法获取整首音频：{}",
            reason.unwrap_or_else(|| {
                "该账号可能无权播放整首歌曲（可能需要会员，或受地区限制）".to_string()
            })
        ));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(anyhow!("QQ音乐返回的音频地址无效"));
    }
    Ok(Audio {
        ext: audio_ext(&url),
        url,
    })
}

fn is_qrc(text: &str) -> bool {
    if !(text.contains('(') && text.contains(')')) {
        return false;
    }
    let bytes = text.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] != b'[' {
            continue;
        }
        let mut cursor = index + 1;
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor == start || cursor >= bytes.len() || bytes[cursor] != b',' {
            continue;
        }
        cursor += 1;
        let duration = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor > duration && cursor < bytes.len() && bytes[cursor] == b']' {
            return true;
        }
    }
    false
}

fn base64_text(value: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
}

/// 按 Folia Major 的公开 musicu 请求读 QRC：不需要登录，也不把会话交给第三方接口。
fn qrc_lyric(m: &Music, song: &Song) -> Option<Lyrics> {
    let song_id = song.id.trim().parse::<u64>().ok().filter(|id| *id > 0)?;
    let payload = json!({
        "comm": {
            "ct": 11,
            "cv": "1003006",
            "v": "1003006",
            "os_ver": "15",
            "phonetype": "24122RKC7C",
            "rom": "Redmi/miro/miro:15/AE3A.240806.005/OS2.0.102.0.VOMCNXM:user/release-keys",
            "tmeAppID": "qqmusiclight",
            "nettype": "NETWORK_WIFI",
            "udid": "0",
            "uid": "0"
        },
        "request": {
            "method": "GetPlayLyricInfo",
            "module": "music.musichallSong.PlayLyricInfo",
            "param": {
                "albumName": base64_text(&song.album),
                "crypt": 1,
                "ct": 19,
                "cv": 2111,
                "interval": song.seconds,
                "lrc_t": 0,
                "qrc": 1,
                "qrc_t": 0,
                "roma": 1,
                "roma_t": 0,
                "singerName": base64_text(&song.artist),
                "songID": song_id,
                "songName": base64_text(&song.name),
                "trans": 1,
                "trans_t": 0,
                "type": 0
            }
        }
    });

    let response = m
        .web()
        .post(QQ_MUSICU)
        .header("Content-Type", "application/json")
        .header("Cookie", "tmeLoginType=-1;")
        .header("User-Agent", "okhttp/3.14.9")
        .timeout(Duration::from_secs(20))
        .json(&payload)
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json::<Value>()
        .ok()?;

    if response.get("code").and_then(number) != Some(0) {
        return None;
    }
    let request = response.get("request")?;
    if request.get("code").and_then(number) != Some(0) {
        return None;
    }
    let encrypted = nonempty_field(request.get("data")?, &["lyric"])?;
    let plain = lyrics_crypto::decrypter::qrc::decrypter::decrypt_lyrics(&encrypted)?;
    let plain = plain.trim().to_string();
    if plain.is_empty() {
        return None;
    }
    let ext = if is_qrc(&plain) { "qrc" } else { "lrc" };
    Some(Lyrics { text: plain, ext })
}

fn bridge_lyric(m: &Music, account: &Account, song: &Song) -> anyhow::Result<Option<Lyrics>> {
    let body = api(m, account, "/getLyric", &[("songmid", &song.mid)])?;
    let lyric = body
        .pointer("/response/lyric")
        .or_else(|| body.get("lyric"))
        .unwrap_or(&Value::Null);
    Ok(lyrics(lyric, "lrc"))
}

pub(super) fn lyric(m: &Music, account: &Account, song: &Song) -> anyhow::Result<Option<Lyrics>> {
    if song.mid.trim().is_empty() {
        return Ok(None);
    }
    // 先要平台真正给逐字时间轴的 QRC；拿不到时才退回适配器已经解码好的 LRC。
    // 两者都是平台原文，不补任何推测出来的逐字时间。
    if let Some(qrc) = qrc_lyric(m, song) {
        return Ok(Some(qrc));
    }
    bridge_lyric(m, account, song)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_song_identity_cover_and_duration() {
        let raw = json!({
            "id": 123456,
            "mid": "song-mid",
            "title": "歌名",
            "interval": 245,
            "singer": [{ "name": "甲" }, { "name": "乙" }],
            "album": { "mid": "album-mid", "name": "专辑" },
            "file": { "media_mid": "media-mid" }
        });
        let song = song_from(&raw).expect("song");
        assert_eq!(song.id, "123456");
        assert_eq!(song.mid, "song-mid");
        assert_eq!(song.media_mid, "media-mid");
        assert_eq!(song.name, "歌名");
        assert_eq!(song.artist, "甲, 乙");
        assert_eq!(song.album, "专辑");
        assert_eq!(song.seconds, 245);
        assert_eq!(
            song.cover.as_deref(),
            Some(
                "https://y.gtimg.cn/music/photo_new/T002R300x300M000album-mid.jpg?max_age=2592000"
            )
        );
    }

    #[test]
    fn separates_owned_favorite_and_liked_playlists() {
        let owned = playlist_from(&json!({
            "tid": 1, "dirId": 5, "dirName": "自建", "songNum": 3
        }))
        .expect("owned");
        assert_eq!(owned.qq_dir, Some(5));

        let favorite = playlist_from(&json!({
            "tid": 2, "dirId": 36, "name": "收藏", "songnum": 37
        }))
        .expect("favorite");
        assert_eq!(favorite.qq_dir, None);

        let liked = playlist_from(&json!({
            "tid": 3, "dirId": 201, "dirName": "我喜欢", "songNum": 10
        }))
        .expect("liked");
        assert_eq!(liked.qq_dir, Some(201));
    }

    #[test]
    fn search_maps_lite_items_and_reports_more() {
        let body = json!({
            "code": 200,
            "total": 30,
            "more": true,
            "items": [{
                "mid": "0027oMO61wWi55",
                "name": "发如雪",
                "interval": 299,
                "singer": [{ "name": "周杰伦" }],
                "album": { "mid": "0024bjiL2aocxT", "name": "十一月的萧邦" },
                "file": { "media_mid": "002A6j2q1tdUUP" }
            }]
        });
        let page = search_page(&body, 0).expect("search page");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].mid, "0027oMO61wWi55");
        assert_eq!(page.items[0].artist, "周杰伦");
        assert!(page.items[0].cover.is_some());
        assert!(page.more);

        let empty = search_page(&json!({ "items": [], "total": 0 }), 0).unwrap();
        assert!(empty.items.is_empty());
        assert!(!empty.more);
    }

    #[test]
    fn audio_extension_comes_from_the_url_path() {
        assert_eq!(
            audio_ext("http://dl.stream.qqmusic.qq.com/M500x.mp3?guid=1"),
            "mp3"
        );
        assert_eq!(audio_ext("https://x/C400x.m4a"), "m4a");
        assert_eq!(audio_ext("https://x/F000x.flac#frag"), "flac");
        assert_eq!(audio_ext("https://x/no-extension"), "mp3");
    }

    #[test]
    fn qrc_detection_requires_a_time_range_not_an_lrc_stamp() {
        assert!(is_qrc("[1234,567]晚风(0,200)轻轻(200,300)"));
        assert!(!is_qrc("[00:12.34]普通歌词"));
    }

    #[test]
    fn login_status_only_treats_known_empty_answers_as_signed_out() {
        let signed_in = json!({
            "code": 200,
            "data": { "profile": { "str_musicid": "100", "nickname": "甲" } }
        });
        let account = account_from_status(&signed_in, "qm_keyst=secret")
            .expect("valid session")
            .expect("account");
        assert_eq!(account.id, "100");
        assert_eq!(account.name, "甲");
        assert_eq!(account.cookie, "qm_keyst=secret");

        // The package answers 200 with an empty `data` for a missing or unusable credential.
        assert!(
            account_from_status(&json!({ "code": 200, "data": {} }), "qm_keyst=x")
                .unwrap()
                .is_none()
        );
        assert!(
            account_from_status(&json!({ "code": 301 }), "qm_keyst=x")
                .unwrap()
                .is_none()
        );
        assert!(
            account_from_status(&json!({ "code": 401 }), "qm_keyst=x")
                .unwrap()
                .is_none()
        );

        // Anything else is a protocol failure, never a logout.
        for body in [
            json!({ "code": 200, "data": { "profile": {} } }),
            json!({ "code": 200, "data": { "profile": null } }),
            json!({ "code": 200, "data": [] }),
            json!({ "code": 500, "data": {} }),
            json!({ "data": {} }),
            json!({}),
        ] {
            let error = account_from_status(&body, "qm_keyst=secret")
                .err()
                .expect("malformed");
            assert!(!format!("{error:#}").contains("secret"));
        }
    }
}
