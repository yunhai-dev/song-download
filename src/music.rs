mod netease;
mod qq;

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Qq,
    Netease,
}

impl Provider {
    pub const ALL: [Self; 2] = [Self::Qq, Self::Netease];

    pub fn index(self) -> usize {
        match self {
            Self::Qq => 0,
            Self::Netease => 1,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Qq => "QQ 音乐",
            Self::Netease => "网易云音乐",
        }
    }

    fn route(self) -> &'static str {
        match self {
            Self::Qq => "qq",
            Self::Netease => "netease",
        }
    }
}

pub struct Account {
    pub name: String,
    pub id: String,
    pub cookie: String,
}

pub struct Qr {
    pub key: String,
    pub image: String,
}

pub enum Poll {
    Waiting,
    Scanned,
    Expired,
    SignedIn(Account),
}

#[derive(Clone)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub qq_dir: Option<u64>,
}

#[derive(Clone)]
pub struct Song {
    pub id: String,
    pub mid: String,
    pub media_mid: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub cover: Option<String>,
    pub seconds: u64,
}

pub struct Audio {
    pub url: String,
    pub ext: &'static str,
}

pub struct Lyrics {
    pub text: String,
    pub ext: &'static str,
}

pub struct Page<T> {
    pub items: Vec<T>,
    pub more: bool,
}

struct Bridge {
    child: Child,
    endpoint: String,
    token: String,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone)]
pub struct Music {
    local: Client,
    web: Client,
    bridge: Arc<Bridge>,
}

impl Music {
    pub fn new() -> Result<Self> {
        let local = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(35))
            .build()?;
        let web = Client::builder()
            .redirect(Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || !is_http(attempt.url()) {
                    attempt.error("invalid media redirect")
                } else {
                    attempt.follow()
                }
            }))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let root = std::env::var_os("SONG_DOWNLOAD_ROOT")
            .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from);
        let child = Command::new("node")
            .arg(root.join("bridge/main.cjs"))
            .env("SONG_DOWNLOAD_BRIDGE_TOKEN", &token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("无法启动本地适配器，请安装 Node.js 22 或更新版本")?;
        let mut bridge = Bridge {
            child,
            endpoint: String::new(),
            token,
        };
        let stdout = bridge
            .child
            .stdout
            .take()
            .context("无法读取适配器启动状态")?;
        let (sender, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(port) = line
                    .strip_prefix("SONG_DOWNLOAD_PORT=")
                    .and_then(|value| value.parse::<u16>().ok())
                    .filter(|port| *port != 0)
                {
                    let _ = sender.try_send(port);
                }
            }
        });
        let port = receiver
            .recv_timeout(Duration::from_secs(15))
            .context("本地适配器未就绪，请在项目目录运行 npm ci --ignore-scripts")?;
        bridge.endpoint = format!("http://127.0.0.1:{port}");
        Ok(Self {
            local,
            web,
            bridge: Arc::new(bridge),
        })
    }

    fn api(
        &self,
        provider: Provider,
        route: &str,
        params: &[(&str, &str)],
        cookie: &str,
    ) -> Result<Value> {
        let endpoint = format!("{}/{}{route}", self.bridge.endpoint, provider.route());
        let params: std::collections::BTreeMap<_, _> = params.iter().copied().collect();
        let response = self
            .local
            .post(endpoint)
            .header("Authorization", &self.bridge.token)
            .json(&json!({ "params": params, "cookie": cookie }))
            .send()
            .with_context(|| format!("{}：本地适配器请求失败", provider.label()))?;
        let status = response.status();
        let value = response
            .json::<Value>()
            .with_context(|| format!("{}：接口响应无效（HTTP {status}）", provider.label()))?;
        if status.is_server_error() {
            anyhow::bail!("{}：接口暂不可用（HTTP {status}）", provider.label());
        }
        Ok(value)
    }

    pub fn web(&self) -> &Client {
        &self.web
    }

    pub fn qr(&self, provider: Provider) -> Result<Qr> {
        match provider {
            Provider::Qq => qq::qr(self),
            Provider::Netease => netease::qr(self),
        }
    }

    pub fn poll(&self, provider: Provider, key: &str) -> Result<Poll> {
        match provider {
            Provider::Qq => qq::poll(self, key),
            Provider::Netease => netease::poll(self, key),
        }
    }

    pub fn playlists(&self, provider: Provider, account: &Account) -> Result<Vec<Playlist>> {
        match provider {
            Provider::Qq => qq::playlists(self, account),
            Provider::Netease => netease::playlists(self, account),
        }
    }

    pub fn tracks(
        &self,
        provider: Provider,
        account: &Account,
        playlist: &Playlist,
        offset: usize,
        limit: usize,
    ) -> Result<Page<Song>> {
        match provider {
            Provider::Qq => qq::tracks(self, account, playlist, offset, limit),
            Provider::Netease => netease::tracks(self, account, playlist, offset, limit),
        }
    }

    pub fn audio(&self, provider: Provider, account: &Account, song: &Song) -> Result<Audio> {
        match provider {
            Provider::Qq => qq::audio(self, account, song),
            Provider::Netease => netease::audio(self, account, song),
        }
    }

    pub fn lyric(
        &self,
        provider: Provider,
        account: &Account,
        song: &Song,
    ) -> Result<Option<Lyrics>> {
        match provider {
            Provider::Qq => qq::lyric(self, account, song),
            Provider::Netease => netease::lyric(self, account, song),
        }
    }
}

pub fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn checked(value: Value, success: u64, operation: &str) -> Result<Value> {
    let code = number(&value["code"]);
    if matches!(code, Some(301 | 401)) {
        anyhow::bail!("{operation}失败：登录已失效，请重新扫码");
    }
    ensure!(
        code == Some(success),
        "{operation}失败（平台状态码：{}）",
        code.map_or_else(|| "缺失".into(), |code| code.to_string())
    );
    Ok(value)
}

fn lyrics(value: &Value, ext: &'static str) -> Option<Lyrics> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then(|| Lyrics {
        text: text.to_owned(),
        ext,
    })
}
