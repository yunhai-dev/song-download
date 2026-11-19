use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{
    Url,
    blocking::{Client, Response},
    header::CONTENT_TYPE,
};
use serde::{Deserialize, Serialize};
use tempfile::{Builder, NamedTempFile};

use crate::music::{Account, Music, Provider, Song, is_http};

/// Outcome of one track download: the saved audio file plus the user-facing
/// report of every attempted asset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub audio_path: PathBuf,
    pub summary: String,
}

const AUDIO_LIMIT: u64 = 512 * 1024 * 1024;
const COVER_LIMIT: u64 = 16 * 1024 * 1024;
const LYRIC_LIMIT: u64 = 4 * 1024 * 1024;
const STEM_LIMIT: usize = 160;
const ATTEMPTS: usize = 1000;
const BUFFER: usize = 64 * 1024;

/// Downloads the full audio track and best-effort cover/lyrics into `folder`.
///
/// The receipt is user-facing and reports every attempted asset together with
/// its file name or the concrete reason it was skipped.
pub fn download(
    music: &Music,
    provider: Provider,
    account: &Account,
    song: &Song,
    folder: &Path,
) -> Result<Receipt> {
    let audio = music
        .audio(provider, account, song)
        .with_context(|| format!("{}：获取音频地址失败", provider.label()))?;
    let ext = safe_ext(audio.ext)
        .with_context(|| format!("平台返回了无法识别的音频格式（{}）", audio.ext))?;

    fs::create_dir_all(folder).with_context(|| format!("无法创建下载目录 {}", folder.display()))?;
    let stem = file_stem(song);

    let audio_name =
        save_audio(music.web(), &audio.url, folder, &stem, &ext).context("音频下载失败")?;
    let audio_path = folder.join(&audio_name);
    let stem = Path::new(&audio_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("无法读取已保存的音频文件名")?;
    let mut summary = format!("已保存 {audio_name}");

    match song
        .cover
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        Some(url) => match save_cover(music.web(), url, folder, stem) {
            Ok(name) => summary.push_str(&format!("\n封面：{name}")),
            Err(error) => summary.push_str(&format!("\n封面：失败（{error:#}）")),
        },
        None => summary.push_str("\n封面：无"),
    }

    match music.lyric(provider, account, song) {
        Ok(Some(lyric)) => {
            let ext = safe_ext(lyric.ext).unwrap_or_else(|| "lrc".to_owned());
            match save_lyrics(folder, stem, &ext, &lyric.text) {
                Ok(name) => {
                    let kind = if ext == "lrc" { "逐行" } else { "逐字" };
                    summary.push_str(&format!("\n歌词（{kind}）：{name}"));
                }
                Err(error) => summary.push_str(&format!("\n歌词：失败（{error:#}）")),
            }
        }
        Ok(None) => summary.push_str("\n歌词：无"),
        Err(error) => summary.push_str(&format!("\n歌词：失败（{error:#}）")),
    }

    Ok(Receipt {
        audio_path,
        summary,
    })
}

fn save_audio(client: &Client, url: &str, folder: &Path, stem: &str, ext: &str) -> Result<String> {
    let response = get(client, url)?;
    ensure_audio_content_type(content_type(&response).as_deref())?;
    let mut temp = temp_file(folder)?;
    stream(response, temp.as_file_mut(), AUDIO_LIMIT, |head| {
        ensure!(is_audio(head), "服务器返回的内容不是音频");
        Ok(())
    })?;
    finish(temp, folder, stem, ext)
}

fn save_cover(client: &Client, url: &str, folder: &Path, stem: &str) -> Result<String> {
    let response = get(client, url)?;
    if let Some(kind) = content_type(&response) {
        ensure!(
            kind.starts_with("image/")
                || matches!(
                    kind.as_str(),
                    "application/octet-stream" | "binary/octet-stream"
                ),
            "服务器返回的不是图片（{kind}）"
        );
    }
    let mut temp = temp_file(folder)?;
    let mut ext = None;
    stream(response, temp.as_file_mut(), COVER_LIMIT, |head| {
        ext = image_ext(head);
        ensure!(ext.is_some(), "服务器返回的内容不是常见图片格式");
        Ok(())
    })?;
    let ext = ext.context("封面格式无法识别")?;
    finish(temp, folder, stem, ext)
}

fn save_lyrics(folder: &Path, stem: &str, ext: &str, text: &str) -> Result<String> {
    ensure!(text.len() as u64 <= LYRIC_LIMIT, "歌词文件过大");
    let mut temp = temp_file(folder)?;
    temp.as_file_mut()
        .write_all(text.as_bytes())
        .context("无法写入歌词临时文件")?;
    finish(temp, folder, stem, ext)
}

fn get(client: &Client, url: &str) -> Result<Response> {
    let parsed = Url::parse(url).context("下载地址无效")?;
    ensure!(is_http(&parsed), "下载地址不是 HTTP 链接");
    let response = client
        .get(parsed)
        .send()
        .map_err(|error| error.without_url())
        .context("网络请求失败，无法下载文件")?;
    let status = response.status();
    ensure!(
        status == reqwest::StatusCode::OK,
        "服务器未返回完整文件（HTTP {status}）"
    );
    Ok(response)
}

fn content_type(response: &Response) -> Option<String> {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        })
        .filter(|value| !value.is_empty())
}

fn ensure_audio_content_type(kind: Option<&str>) -> Result<()> {
    if let Some(kind) = kind {
        ensure!(
            kind.starts_with("audio/")
                || matches!(
                    kind,
                    "application/octet-stream"
                        | "binary/octet-stream"
                        | "application/ogg"
                        | "video/ogg"
                        | "application/mp4"
                        | "video/mp4"
                ),
            "服务器返回的不是音频（{kind}）"
        );
    }
    Ok(())
}

/// Streams the body into `file`, rejecting empty/oversized payloads before or
/// during the copy. `validate` inspects the leading bytes while they are still
/// available.
fn stream(
    mut response: Response,
    file: &mut File,
    limit: u64,
    validate: impl FnOnce(&[u8]) -> Result<()>,
) -> Result<()> {
    if let Some(length) = response.content_length() {
        ensure!(length > 0, "服务器返回了空文件");
        ensure!(
            length <= limit,
            "文件超过大小上限（{} MiB）",
            limit / (1024 * 1024)
        );
    }

    let mut head = [0u8; 16];
    let mut filled = 0;
    while filled < head.len() {
        let read = response
            .read(&mut head[filled..])
            .context("读取下载内容失败")?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    ensure!(filled > 0, "服务器返回了空文件");
    validate(&head[..filled])?;
    file.write_all(&head[..filled])
        .context("写入临时文件失败")?;

    let mut written = filled as u64;
    let mut buffer = vec![0u8; BUFFER];
    loop {
        let read = response.read(&mut buffer).context("读取下载内容失败")?;
        if read == 0 {
            break;
        }
        written = written.saturating_add(read as u64);
        ensure!(
            written <= limit,
            "文件超过大小上限（{} MiB）",
            limit / (1024 * 1024)
        );
        file.write_all(&buffer[..read])
            .context("写入临时文件失败")?;
    }
    Ok(())
}

fn temp_file(folder: &Path) -> Result<NamedTempFile> {
    Builder::new()
        .prefix(".song-download-")
        .tempfile_in(folder)
        .with_context(|| format!("无法在 {} 创建临时文件", folder.display()))
}

fn finish(mut temp: NamedTempFile, folder: &Path, stem: &str, ext: &str) -> Result<String> {
    temp.as_file_mut().flush().context("无法写入临时文件")?;
    let path = persist(temp, folder, stem, ext)?;
    Ok(path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default())
}

/// Atomically moves `temp` into place, adding ` (2)`, ` (3)`, … instead of
/// overwriting an existing download.
fn persist(mut temp: NamedTempFile, folder: &Path, stem: &str, ext: &str) -> Result<PathBuf> {
    for index in 1..=ATTEMPTS {
        let name = if index == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem} ({index}).{ext}")
        };
        let path = folder.join(&name);
        match temp.persist_noclobber(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => temp = error.file,
            Err(error) => {
                return Err(error.error).with_context(|| format!("无法保存到 {}", path.display()));
            }
        }
    }
    bail!("同名文件过多，无法保存 {stem}.{ext}")
}

fn file_stem(song: &Song) -> String {
    let artist = clean(&song.artist);
    let title = clean(&song.name);
    let mut stem = match (artist.is_empty(), title.is_empty()) {
        (true, true) => "未命名歌曲".to_owned(),
        (true, false) => title,
        (false, true) => artist,
        (false, false) => format!("{artist} - {title}"),
    };
    stem = truncate(stem, STEM_LIMIT);
    if is_reserved(&stem) {
        stem.insert(0, '_');
    }
    stem
}

fn clean(value: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_control() || matches!(ch, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            if pending_space && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            pending_space = false;
            out.push('_');
            continue;
        }
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if pending_space && !out.ends_with(' ') {
            out.push(' ');
        }
        pending_space = false;
        out.push(ch);
    }
    while out.ends_with(' ') || out.ends_with('.') {
        out.pop();
    }
    out
}

fn truncate(mut value: String, limit: usize) -> String {
    if value.len() > limit {
        let mut end = limit;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    while value.ends_with(' ') || value.ends_with('.') {
        value.pop();
    }
    if value.is_empty() {
        value.push_str("未命名歌曲");
    }
    value
}

fn is_reserved(stem: &str) -> bool {
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn safe_ext(ext: &str) -> Option<String> {
    let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    (!ext.is_empty() && ext.len() <= 8 && ext.chars().all(|ch| ch.is_ascii_alphanumeric()))
        .then_some(ext)
}

fn is_audio(head: &[u8]) -> bool {
    head.len() >= 4
        && (head.starts_with(b"ID3")
            || head.starts_with(b"fLaC")
            || head.starts_with(b"OggS")
            || head.starts_with(b"MAC ")
            || head.starts_with(b"#!AMR")
            || (head[0] == 0xFF && head[1] & 0xE0 == 0xE0)
            || (head.starts_with(b"RIFF") && head.len() >= 12 && &head[8..12] == b"WAVE")
            || (head.starts_with(b"FORM") && head.len() >= 12 && &head[8..12] == b"AIFF")
            || (head.len() >= 8 && &head[4..8] == b"ftyp"))
}

fn image_ext(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if head.starts_with(b"\xFF\xD8\xFF") {
        Some("jpg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("gif")
    } else if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        Some("webp")
    } else if head.starts_with(b"BM") {
        Some("bmp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(name: &str, artist: &str) -> Song {
        Song {
            id: "1".into(),
            mid: "mid".into(),
            media_mid: "media".into(),
            name: name.into(),
            artist: artist.into(),
            album: "专辑".into(),
            cover: None,
            seconds: 200,
        }
    }

    #[test]
    fn builds_safe_bounded_stem() {
        assert_eq!(file_stem(&song("稻香", "周杰伦")), "周杰伦 - 稻香");

        let stem = file_stem(&song("A/B:C*?\"<>|", "  "));
        assert!(stem.contains("A_B_C"));
        assert!(
            stem.chars()
                .all(|ch| !matches!(ch, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        );

        let stem = file_stem(&song(&"音".repeat(500), "歌手"));
        assert!(stem.len() <= STEM_LIMIT);
        assert!(stem.is_char_boundary(stem.len()));
        assert!(!stem.ends_with(' '));
        assert!(!stem.ends_with('.'));

        assert_eq!(file_stem(&song("...", "  ")), "未命名歌曲");
    }

    #[test]
    fn never_clobbers_an_existing_download() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("歌手 - 歌.mp3");
        fs::write(&existing, b"old").unwrap();

        let mut first = NamedTempFile::new_in(dir.path()).unwrap();
        first.as_file_mut().write_all(b"new").unwrap();
        let first_path = persist(first, dir.path(), "歌手 - 歌", "mp3").unwrap();
        assert_eq!(
            first_path.file_name().unwrap().to_str().unwrap(),
            "歌手 - 歌 (2).mp3"
        );
        assert_eq!(fs::read(&first_path).unwrap(), b"new");
        assert_eq!(fs::read(&existing).unwrap(), b"old");

        let mut second = NamedTempFile::new_in(dir.path()).unwrap();
        second.as_file_mut().write_all(b"newer").unwrap();
        let second_path = persist(second, dir.path(), "歌手 - 歌", "mp3").unwrap();
        assert_eq!(
            second_path.file_name().unwrap().to_str().unwrap(),
            "歌手 - 歌 (3).mp3"
        );
        assert_eq!(fs::read(&second_path).unwrap(), b"newer");
        assert_eq!(fs::read(&existing).unwrap(), b"old");
    }

    #[test]
    fn accepts_real_audio_headers_and_rejects_web_pages() {
        assert!(is_audio(b"ID3\x04\x00\x00\x00"));
        assert!(is_audio(b"\xFF\xFB\x90\x64"));
        assert!(is_audio(b"fLaC\x00\x00\x00\x22"));
        assert!(is_audio(b"OggS\x00\x02\x00\x00"));
        assert!(is_audio(b"\x00\x00\x00\x20ftypM4A "));
        assert!(!is_audio(b"<!DOCTYPE html>"));
        assert!(!is_audio(b"{\"code\":404}"));
        assert!(!is_audio(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn derives_cover_extension_from_bytes() {
        assert_eq!(image_ext(b"\x89PNG\r\n\x1a\nrest"), Some("png"));
        assert_eq!(image_ext(b"\xFF\xD8\xFF\xE0rest"), Some("jpg"));
        assert_eq!(image_ext(b"GIF89a......"), Some("gif"));
        assert_eq!(image_ext(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("webp"));
        assert_eq!(image_ext(b"<html>no</html>"), None);
    }

    #[test]
    fn sanitizes_extensions() {
        assert_eq!(safe_ext(".MP3").as_deref(), Some("mp3"));
        assert_eq!(safe_ext("m4a").as_deref(), Some("m4a"));
        assert_eq!(safe_ext("../x").as_deref(), None);
        assert_eq!(safe_ext("").as_deref(), None);
    }
}
