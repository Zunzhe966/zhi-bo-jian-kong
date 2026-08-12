use anyhow::{Context, Result, bail};
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::SmtpTransport;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, Transport};
use serde::Serialize;
use sqlx::sqlite::SqlitePool;
use std::time::Duration;

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
        if channel == Channel::Email && !self.config.email_configured() {
            tracing::warn!("邮箱未配置，跳过发送并保持通知可审计性（不写入 pending 队列）");
            return Ok(false);
        }
        let channel_name = match channel {
            Channel::Email => "邮箱",
            Channel::Wechat => "微信",
            Channel::None => "不通知",
        };
        let id = db::add_notify(self.pool, event, anchor, title, content, channel_name).await?;
        if channel == Channel::None {
            // “不通知”本身是有效投递策略：保留审计记录但立即完成。
            db::mark_notify_done(self.pool, id).await?;
            return Ok(false);
        }
        let sent = match channel {
            Channel::Email => {
                let settings = self.config.email_settings();
                let subject = title.to_string();
                let body = content.to_string();
                Self::send_email_with_timeout(settings, subject, body).await
            }
            Channel::Wechat => {
                tracing::warn!(
                    "微信通道尚未接入，通知 id={} 标记 failed，避免无限 pending",
                    id
                );
                db::fail_notify(self.pool, id).await?;
                return Ok(false);
            }
            Channel::None => false,
        };
        if sent {
            db::mark_notify_done(self.pool, id).await?;
        }
        Ok(sent)
    }

    /// 重试通知队列中的 pending 通知：一次最多处理 50 条，失败保留 pending，
    /// 成功标记 sent。发送失败只记日志，不拖垮 daemon tick。
    pub async fn flush_pending(&self, limit: i64) -> Result<usize> {
        if !self.config.email_configured() {
            return Ok(0);
        }
        let pending = db::list_notify(self.pool, "pending", limit).await?;
        let mut sent_count = 0;
        for item in pending {
            let channel = Channel::parse(&item.channel);
            match self
                .send(
                    &item.event,
                    &item.anchor,
                    &item.title,
                    &item.content,
                    channel,
                )
                .await
            {
                Ok(true) => sent_count += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!("重试通知失败: {e:#}"),
            }
        }
        Ok(sent_count)
    }

    /// 把阻塞 SMTP 调用放进 blocking 线程池并限时执行，避免卡住 daemon tick。
    /// 超时后调用继续执行但调用方已返回，符合“通知失败不阻塞监控”的契约。
    async fn send_email_with_timeout(
        settings: EmailSettings,
        subject: String,
        body: String,
    ) -> bool {
        let join = tokio::task::spawn_blocking(move || send_email(settings, &subject, &body));
        match tokio::time::timeout(Duration::from_secs(20), join).await {
            Ok(Ok(Ok(true))) => true,
            Ok(Ok(Ok(false))) => {
                tracing::warn!("SMTP 未返回成功，通知保持 pending 待重试");
                false
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!("SMTP 发送失败，通知保持 pending 待重试: {e:#}");
                false
            }
            Ok(Err(e)) => {
                tracing::warn!("SMTP 发送任务失败，通知保持 pending 待重试: {e:#}");
                false
            }
            Err(_) => {
                tracing::warn!("SMTP 发送超时（20 秒），通知保持 pending 待重试");
                false
            }
        }
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

    #[tokio::test]
    async fn unconfigured_email_skips_pending_record() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[推送配置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();
        let file = std::env::temp_dir().join(format!(
            "zunzhe-notify-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = db::open_pool(&file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let notifier = Notifier::new(&pool, &cfg);
        let sent = notifier
            .send("live", "主播", "开播", "内容", Channel::Email)
            .await
            .unwrap();
        assert!(!sent);
        // 未配置邮箱时不写 pending，避免通知表无限膨胀。
        assert_eq!(db::list_notify(&pool, "", 10).await.unwrap().len(), 0);
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("db-shm"));
        let _ = std::fs::remove_file(file.with_extension("db-wal"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wechat_unimplemented_marks_failed_not_pending() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-wx-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::write(dir.join("config").join("config.ini"), "[推送配置]\n").unwrap();
        std::fs::write(dir.join("config").join("fupan_config.ini"), "").unwrap();
        let file = std::env::temp_dir().join(format!(
            "zunzhe-notify-wx-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = db::open_pool(&file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let notifier = Notifier::new(&pool, &cfg);
        let sent = notifier
            .send("video", "博主", "新视频", "内容", Channel::Wechat)
            .await
            .unwrap();
        assert!(!sent);
        let rows = db::list_notify(&pool, "failed", 10).await.unwrap();
        assert_eq!(rows.len(), 1, "微信未接入应标记 failed");
        assert_eq!(
            db::list_notify(&pool, "pending", 10).await.unwrap().len(),
            0
        );
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("db-shm"));
        let _ = std::fs::remove_file(file.with_extension("db-wal"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn blocking_smtp_call_is_cancelled_by_timeout() {
        // 模拟不可达 SMTP：把真实 send_email 放进 blocking 线程池，20 秒内必然返回。
        let settings = EmailSettings {
            host: "smtp.invalid.local".to_string(),
            port: "465".to_string(),
            ssl: true,
            login: "test@qq.com".to_string(),
            auth: "fake".to_string(),
            sender: "test@qq.com".to_string(),
            sender_name: "尊者".to_string(),
            to: "test@qq.com".to_string(),
        };
        let started = std::time::Instant::now();
        let result =
            Notifier::send_email_with_timeout(settings, "开播".to_string(), "内容".to_string())
                .await;
        assert!(!result);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "阻塞 SMTP 调用不能超过 30 秒"
        );
    }
}
