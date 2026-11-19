use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use serde::Serialize;

use crate::music::Provider;

#[cfg(target_os = "macos")]
const SERVICE: &str = "song-download.login";
const QQ_KEY: &str = "qq-session-store-key";

#[derive(Serialize)]
pub struct QqStore {
    pub path: PathBuf,
    pub secret: String,
}

pub fn qq_store() -> Result<QqStore> {
    let path = dirs::data_local_dir()
        .context("无法定位应用数据目录")?
        .join("song-download/qq-sessions.enc");
    let secret = match load(QQ_KEY)? {
        Some(secret) => secret,
        None => {
            ensure!(!path.try_exists()?, "QQ 会话存储密钥缺失");
            let secret = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            save(QQ_KEY, &secret)?;
            secret
        }
    };
    ensure!(
        secret.len() == 64 && secret.bytes().all(|b| b.is_ascii_hexdigit()),
        "QQ 会话存储密钥无效"
    );
    Ok(QqStore { path, secret })
}

fn entry(provider: Provider) -> &'static str {
    match provider {
        Provider::Qq => "qq",
        Provider::Netease => "netease",
    }
}

pub fn load_cookie(provider: Provider) -> Result<Option<String>> {
    load(entry(provider))
}

pub fn save_cookie(provider: Provider, cookie: &str) -> Result<()> {
    save(entry(provider), cookie)
}

#[cfg(target_os = "macos")]
pub fn delete_cookie(provider: Provider) -> Result<()> {
    match security_framework::passwords::delete_generic_password(SERVICE, entry(provider)) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
        Err(error) => Err(error).context("无法清除钥匙串登录记录"),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn delete_cookie(_: Provider) -> Result<()> {
    // This build cannot create a persistent entry to delete.
    Ok(())
}

#[cfg(target_os = "macos")]
const ITEM_NOT_FOUND: i32 = -25300;

#[cfg(target_os = "macos")]
fn load(account: &str) -> Result<Option<String>> {
    match security_framework::passwords::get_generic_password(SERVICE, account) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| anyhow::anyhow!("钥匙串登录记录格式无效")),
        Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
        Err(error) => Err(error).context("无法读取登录钥匙串"),
    }
}

#[cfg(target_os = "macos")]
fn save(account: &str, value: &str) -> Result<()> {
    security_framework::passwords::set_generic_password(SERVICE, account, value.as_bytes())
        .context("无法写入登录钥匙串")
}

#[cfg(not(target_os = "macos"))]
fn load(_: &str) -> Result<Option<String>> {
    Ok(None)
}

#[cfg(not(target_os = "macos"))]
fn save(_: &str, _: &str) -> Result<()> {
    anyhow::bail!("当前系统未配置安全凭据存储")
}
