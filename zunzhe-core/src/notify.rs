use anyhow::{Context, Result, bail};
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::SmtpTransport;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, Transport};
use serde::Serialize;
use sqlx::sqlite::SqlitePool;

use crate::config::{Config, EmailSettings};
use crate::db;

/// 通知通道：邮箱 / 微信 / 不通知（微信后续接 Server酱）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[allow(dead_code)] // 每主播覆盖通道（微信/不通知）预留
pub enum Channel {
    Email,
    Wechat,
    None,
}

impl Channel {
    #[allow(dead_code)] // 按监控偏好字符串解析通道
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "微信" => Channel::Wechat,
            "不通知" => Channel::None,
            _ => Channel::Email,
        }
    }
}

/// 通知总线：按主播偏好发送，并把记录写入 notify 表。
pub struct Notifier<'a> {
    pool: &'a SqlitePool,
    config: &'a Config,
}

impl<'a> Notifier<'a> {
    pub fn new(pool: &'a SqlitePool, config: &'a Config) -> Self {
        Self { pool, config }
    }

    pub async fn send(
        &self,
        event: &str,
        anchor: &str,
        title: &str,
        content: &str,
        channel: Channel,
    ) -> Result<bool> {
        let channel_name = match channel {
            Channel::Email => "邮箱",
            Channel::Wechat => "微信",
            Channel::None => "不通知",
        };
        let id = db::add_notify(self.pool, event, anchor, title, content, channel_name).await?;
        if channel == Channel::None {
            return Ok(false);
        }
        let sent = match channel {
            Channel::Email => send_email(self.config.email_settings(), title, content)?,
            Channel::Wechat => bail!("微信通道尚未接入，请先用邮箱或配置 Server酱"),
            Channel::None => false,
        };
        if sent {
            db::mark_notify_done(self.pool, id).await?;
        }
        Ok(sent)
    }

    /// 仅记录已发送事件（test-email 先发信再落库，避免失败也留下“已发送”记录）。
    pub async fn record_sent(
        &self,
        event: &str,
        anchor: &str,
        title: &str,
        content: &str,
        channel: Channel,
    ) -> Result<()> {
        let id = db::add_notify(
            self.pool,
            event,
            anchor,
            title,
            content,
            match channel {
                Channel::Email => "邮箱",
                Channel::Wechat => "微信",
                Channel::None => "不通知",
            },
        )
        .await?;
        db::mark_notify_done(self.pool, id).await?;
        Ok(())
    }
}

pub fn send_email(cfg: EmailSettings, subject: &str, body: &str) -> Result<bool> {
    if cfg.host.is_empty() || cfg.login.is_empty() || cfg.auth.is_empty() || cfg.to.is_empty() {
        bail!("邮箱未配置，缺少 SMTP 服务器/账号/授权码/收件人");
    }
    let from = Mailbox::new(
        Some(cfg.sender_name.clone()),
        cfg.login
            .parse()
            .with_context(|| format!("发件邮箱不合法: {}", cfg.login))?,
    );
    let to: Vec<Mailbox> = cfg
        .to
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().with_context(|| format!("收件邮箱不合法: {s}")))
        .collect::<Result<Vec<_>>>()?;
    let message = Message::builder()
        .from(from)
        .to(to[0].clone())
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body.to_string())
        .context("构造邮件失败")?;
    let port = cfg
        .port
        .parse::<u16>()
        .unwrap_or(if cfg.ssl { 465 } else { 587 });
    let creds = Credentials::new(cfg.login.clone(), cfg.auth.clone());
    let mailer = if cfg.ssl {
        SmtpTransport::relay(&cfg.host)
            .with_context(|| format!("无法解析 SMTP 服务器: {}", cfg.host))?
            .port(port)
            .credentials(creds)
            .build()
    } else {
        SmtpTransport::starttls_relay(&cfg.host)
            .with_context(|| format!("无法解析 SMTP 服务器: {}", cfg.host))?
            .port(port)
            .credentials(creds)
            .build()
    };
    mailer
        .send(&message)
        .context("SMTP 发送失败，请检查账号/授权码/端口/网络")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_channel() {
        assert_eq!(Channel::parse("邮箱"), Channel::Email);
        assert_eq!(Channel::parse("微信"), Channel::Wechat);
        assert_eq!(Channel::parse("不通知"), Channel::None);
        assert_eq!(Channel::parse("跟随全局"), Channel::Email);
    }
}
