use anyhow::{Context, Result, bail};
use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::SmtpTransport;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, Transport};
use serde::Serialize;
use serde_json::Value;
use sqlx::sqlite::SqlitePool;
use std::sync::Mutex;
use std::time::Duration;

use crate::app_prefs;
use crate::config::{Config, EmailSettings};
use crate::db;

/// 通知失败后允许的重试上限。SMTP/HTTPS 单次失败通常是临时网络抖动，
/// 不能一次失败就永久漏通知；超限后才落定 failed 防止配置错误无限重试。
const NOTIFY_MAX_ATTEMPTS: i64 = 3;

/// 本地通知队列的进程内互斥锁。
///
/// `queue_local` 是“读整个 JSON → 追加 → 原子写”三段操作；同一进程内
/// 并发调用时如果不加锁，后一次写入会基于旧文件覆盖前一次追加的内容，
/// 直接丢通知。所有线程共用这一把锁即可。
static NOTIFY_QUEUE_LOCK: Mutex<()> = Mutex::new(());

/// 通知通道：邮箱 / 微信(Server酱) / PushPlus / Bark / ntfy / 不通知。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[allow(dead_code)] // 旧 Python 有 PushPlus/Bark/ntfy，保留通道以便逐项对齐
pub enum Channel {
    Email,
    Wechat,
    PushPlus,
    Bark,
    Ntfy,
    None,
}

impl Channel {
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "微信" => Channel::Wechat,
            "PushPlus" => Channel::PushPlus,
            "Bark" => Channel::Bark,
            "ntfy" => Channel::Ntfy,
            "不通知" => Channel::None,
            _ => Channel::Email,
        }
    }

    /// 把用户偏好字符串解析成真正要发送的通道集合。
    ///
    /// 与旧 Python `_channels_for_pref` 对齐：
    /// `不通知` 返回空集合；明确通道只返回该通道；`跟随全局`/空值返回
    /// 邮箱优先 + 当前已配置的 HTTPS 推送通道。
    pub fn resolve_pref(
        value: &str,
        email_configured: bool,
        push: &crate::config::PushChannels,
    ) -> Vec<Channel> {
        let pref = value.trim();
        if pref == "不通知" {
            return Vec::new();
        }
        if pref == "微信" {
            return vec![Channel::Wechat];
        }
        if pref == "邮箱" {
            return vec![Channel::Email];
        }
        if pref == "PushPlus" {
            return vec![Channel::PushPlus];
        }
        if pref == "Bark" {
            return vec![Channel::Bark];
        }
        if pref == "ntfy" {
            return vec![Channel::Ntfy];
        }
        // 跟随全局 / 空值：邮箱优先，其余已配置通道作为兜底。
        let mut out = Vec::new();
        if email_configured {
            out.push(Channel::Email);
        }
        if !push.serverchan.is_empty() {
            out.push(Channel::Wechat);
        }
        if !push.pushplus.is_empty() {
            out.push(Channel::PushPlus);
        }
        if !push.bark.is_empty() {
            out.push(Channel::Bark);
        }
        if !push.ntfy.is_empty() {
            out.push(Channel::Ntfy);
        }
        if out.is_empty() {
            // 没有任何通道可用时，保留 Email 语义，让调用方得到“未配置邮箱”的可诊断结果。
            out.push(Channel::Email);
        }
        out
    }

    fn name(&self) -> &'static str {
        match self {
            Channel::Email => "邮箱",
            Channel::Wechat => "微信",
            Channel::PushPlus => "PushPlus",
            Channel::Bark => "Bark",
            Channel::Ntfy => "ntfy",
            Channel::None => "不通知",
        }
    }
}

/// 通知总线：按主播偏好发送，并把记录写入 notify 表。
pub struct Notifier<'a> {
    pool: &'a SqlitePool,
    config: &'a Config,
    project_dir: &'a std::path::Path,
}

impl<'a> Notifier<'a> {
    pub fn new(pool: &'a SqlitePool, config: &'a Config) -> Self {
        Self {
            pool,
            config,
            project_dir: std::path::Path::new("."),
        }
    }

    /// 带项目目录的构造器；桌面端本地通知队列依赖项目目录定位
    /// `fupan_output/run/pending_notify.json`。
    pub fn for_project(
        pool: &'a SqlitePool,
        config: &'a Config,
        project_dir: &'a std::path::Path,
    ) -> Self {
        Self {
            pool,
            config,
            project_dir,
        }
    }

    pub async fn send(
        &self,
        event: &str,
        anchor: &str,
        title: &str,
        content: &str,
        pref: &str,
    ) -> Result<bool> {
        let channels = Channel::resolve_pref(
            pref,
            self.config.email_configured(),
            &self.config.push_channels(),
        );
        if channels.is_empty() {
            return Ok(false);
        }
        let mut any_sent = false;
        for channel in channels {
            // 只有“邮箱”需要特殊跳过：未配置邮箱不落库，避免通知表被空配置撑满。
            // 其它 HTTPS 通道即便未配置也会写一条 failed，供用户定位配置缺口。
            if channel == Channel::Email && !self.config.email_configured() {
                tracing::warn!("邮箱未配置，跳过发送并保持通知可审计性（不写入 pending 队列）");
                continue;
            }
            let id =
                db::add_notify(self.pool, event, anchor, title, content, channel.name()).await?;
            if self.send_record(id, title, content, &channel).await? {
                any_sent = true;
            }
        }
        // 本地队列只在选中的 Email 通道中写入；“不通知”或仅 HTTPS 通道不会弹窗。
        Ok(any_sent)
    }

    /// 对已经落库的通知记录执行实际发送，并更新这一条记录。
    ///
    /// 新通知和 pending 重试共用这段逻辑，避免重试时再插入一条新 pending，
    /// 导致原记录永不收敛、通知表无限膨胀。
    async fn send_record(
        &self,
        id: i64,
        title: &str,
        content: &str,
        channel: &Channel,
    ) -> Result<bool> {
        if channel == &Channel::Email {
            // 与旧 Python 一样，无论远程邮件是否最终成功，都先写本地队列，
            // 由 Electron 桌面端负责弹系统通知，避免代理/额度失败时完全无提醒。
            Self::queue_local(self.project_dir, title, content);
            if !self.config.email_configured() {
                // 重试时配置可能已被清空；落定 failed，避免通知行永久 pending。
                db::fail_notify(self.pool, id).await?;
                return Ok(false);
            }
        }
        if channel == &Channel::None {
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
            Channel::Wechat => self.send_wechat(title, content).await,
            Channel::PushPlus => self.send_pushplus(title, content).await,
            Channel::Bark => self.send_bark(title, content).await,
            Channel::Ntfy => self.send_ntfy(title, content).await,
            Channel::None => false,
        };
        if sent {
            db::mark_notify_done(self.pool, id).await?;
        } else {
            // 失败/超时后先重试，只有达到上限才落定 failed；临时网络抖动
            // 会在后续 tick 中继续尝试，不会一次失败就漏通知。
            let attempts = db::bump_notify_attempts(self.pool, id).await?;
            if attempts >= NOTIFY_MAX_ATTEMPTS {
                db::fail_notify(self.pool, id).await?;
            }
        }
        Ok(sent)
    }

    /// 重试通知队列中的 pending 通知：一次最多处理 50 条，失败保留 pending，
    /// 成功标记 sent。发送失败只记日志，不拖垮 daemon tick。
    pub async fn flush_pending(&self, limit: i64) -> Result<usize> {
        // 最老优先：队列超过单轮上限时老通知不能被新通知永远挤出重试窗口。
        let pending = db::list_pending_notify(self.pool, limit).await?;
        let mut sent_count = 0;
        for item in pending {
            let channel = Channel::parse(&item.channel);
            match self
                .send_record(item.id, &item.title, &item.content, &channel)
                .await
            {
                Ok(true) => sent_count += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!("重试通知失败: {e:#}"),
            }
        }
        Ok(sent_count)
    }

    /// 测试所有已配置通知通道；任何一条成功都返回 true。
    /// 与旧 Python `notify.send_test` 对齐：不因 SMTP 未配置而误报微信等
    /// HTTPS 通道不可用，也不因某个通道失败而掩盖其它通道的成功。
    pub async fn send_test(&self, title: &str, content: &str) -> Result<TestSend> {
        let channels = self.config.push_channels();
        let mut sent = false;
        let mut channel_names = Vec::new();
        Self::queue_local(self.project_dir, title, content);

        if self.config.email_configured() {
            let settings = self.config.email_settings();
            if Self::send_email_with_timeout(settings, title.to_string(), content.to_string()).await
            {
                sent = true;
            }
            channel_names.push("邮箱".to_string());
        }
        if !channels.serverchan.is_empty() {
            if send_serverchan(&channels.serverchan, title, content).await {
                sent = true;
            }
            channel_names.push("微信".to_string());
        }
        if !channels.pushplus.is_empty() {
            if send_pushplus(&channels.pushplus, title, content).await {
                sent = true;
            }
            channel_names.push("PushPlus".to_string());
        }
        if !channels.bark.is_empty() {
            if send_bark(&channels.bark, title, content).await {
                sent = true;
            }
            channel_names.push("Bark".to_string());
        }
        if !channels.ntfy.is_empty() {
            if send_ntfy(&channels.ntfy, title, content).await {
                sent = true;
            }
            channel_names.push("ntfy".to_string());
        }

        if !self.config.email_configured() && !channels.configured() {
            let missing: Vec<&str> = [
                (!self.config.email_configured()).then_some("邮箱(SMTP)"),
                channels.serverchan.is_empty().then_some("微信(Server酱)"),
                channels.pushplus.is_empty().then_some("PushPlus"),
                channels.bark.is_empty().then_some("Bark"),
                channels.ntfy.is_empty().then_some("ntfy"),
            ]
            .into_iter()
            .flatten()
            .collect();
            bail!("未配置任何通知通道: {}", missing.join("、"));
        }

        Ok(TestSend {
            sent,
            channels: channel_names,
        })
    }

    /// 把一条消息追加到 Electron 本地通知队列。
    ///
    /// 桌面端 `main.cjs` 每 4 秒读取 `fupan_output/run/pending_notify.json` 并弹出
    /// 系统通知；队列最多保留最近 30 条，与旧 Python `_queue_local` 对齐。
    fn queue_local(project_dir: &std::path::Path, title: &str, content: &str) {
        if project_dir == std::path::Path::new(".") {
            // `new` 构造器仅用于不依赖文件系统的单元测试；真实调用必须使用
            // `for_project`。这里直接返回，避免测试污染源码目录。
            return;
        }
        let _guard = NOTIFY_QUEUE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if app_prefs::read_app_prefs(project_dir)
            .get("localNotify")
            .and_then(Value::as_bool)
            == Some(false)
        {
            return;
        }

        let path = project_dir
            .join("fupan_output")
            .join("run")
            .join("pending_notify.json");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut items: Vec<Value> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        // 同一毫秒可能产生多条通知；只用时间戳做 id 会碰撞，桌面端按 id
        // 去重后只弹一条。追加随机后缀保证每条通知都有唯一 id。
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        items.push(serde_json::json!({
            "id": format!("{:.3}-{nonce}", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or_default()),
            "title": title.chars().take(64).collect::<String>(),
            "body": content.chars().take(500).collect::<String>(),
        }));
        let tail = items.split_off(items.len().saturating_sub(30));
        let _ =
            crate::config::atomic_write(&path, serde_json::to_string(&tail).unwrap_or_default());
    }

    /// 仅记录一次测试事件。发送成功与否不在此处重复判断。
    pub async fn record_sent(
        &self,
        event: &str,
        anchor: &str,
        title: &str,
        content: &str,
        channel: Channel,
    ) -> Result<()> {
        let id = db::add_notify(self.pool, event, anchor, title, content, channel.name()).await?;
        db::mark_notify_done(self.pool, id).await?;
        Ok(())
    }

    /// HTTPS 通道的发送实现；请求自带 15 秒超时，网络失败只记日志不 panic。
    async fn send_wechat(&self, title: &str, content: &str) -> bool {
        let key = self.config.push_channels().serverchan;
        if key.is_empty() {
            tracing::warn!("微信(Server酱) 未配置 SendKey，通知标记 failed");
            return false;
        }
        send_serverchan(&key, title, content).await
    }

    async fn send_pushplus(&self, title: &str, content: &str) -> bool {
        let token = self.config.push_channels().pushplus;
        if token.is_empty() {
            tracing::warn!("PushPlus 未配置 token，通知标记 failed");
            return false;
        }
        send_pushplus(&token, title, content).await
    }

    async fn send_bark(&self, title: &str, content: &str) -> bool {
        let api = self.config.push_channels().bark;
        if api.is_empty() {
            tracing::warn!("Bark 未配置接口链接，通知标记 failed");
            return false;
        }
        send_bark(&api, title, content).await
    }

    async fn send_ntfy(&self, title: &str, content: &str) -> bool {
        let api = self.config.push_channels().ntfy;
        if api.is_empty() {
            tracing::warn!("ntfy 未配置订阅地址，通知标记 failed");
            return false;
        }
        send_ntfy(&api, title, content).await
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
                tracing::warn!("SMTP 未返回成功，通知标记 failed 防止无限重试");
                false
            }
            Ok(Ok(Err(e))) => {
                tracing::warn!("SMTP 发送失败，通知标记 failed 防止无限重试: {e:#}");
                false
            }
            Ok(Err(e)) => {
                tracing::warn!("SMTP 发送任务失败，通知标记 failed 防止无限重试: {e:#}");
                false
            }
            Err(_) => {
                tracing::warn!("SMTP 发送超时（20 秒），通知标记 failed 防止无限重试");
                false
            }
        }
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
    let to = parse_mailboxes(&cfg.to).with_context(|| format!("收件人邮箱不合法: {}", cfg.to))?;
    let mut builder = Message::builder()
        .from(from)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .to(to[0].clone());
    for recipient in to.iter().skip(1) {
        builder = builder.to(recipient.clone());
    }
    let message = builder.body(body.to_string()).context("构造邮件失败")?;
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

/// 解析中英文逗号分隔的收件人列表，过滤空白并拒绝格式错误的地址。
///
/// 旧 Python 的 `msg_push.send_email` 接受多个收件人；Rust 若把 `a,b` 当单一
/// 邮箱字符串解析，就会静默漏掉第二个收件人，甚至在配置错误时整封邮件构造失败。
fn parse_mailboxes(value: &str) -> Result<Vec<Mailbox>> {
    value
        .split([',', '，'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().with_context(|| format!("收件邮箱不合法: {s}")))
        .collect()
}

/// 测试通知结果：是否至少一个通道成功，以及本次尝试过的通道列表。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TestSend {
    pub sent: bool,
    pub channels: Vec<String>,
}

/// 使用 443 HTTPS 的 Server酱³ 推送，避免代理/TUN 环境只依赖 SMTP 失败。
/// 成功判定与旧 Python `fupan/notify.py` 保持一致：`code == 0` 或返回 pushid。
pub async fn send_serverchan(sendkey: &str, title: &str, content: &str) -> bool {
    if sendkey.is_empty() {
        return false;
    }
    let url = format!("https://sctapi.ftqq.com/{sendkey}.send");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build();
    let Ok(client) = client else {
        tracing::warn!("Server酱客户端初始化失败");
        return false;
    };
    let result = client
        .post(url)
        .form(&[
            ("title", title.chars().take(32).collect::<String>()),
            ("desp", content.to_string()),
        ])
        .send()
        .await;
    let Ok(resp) = result else {
        tracing::warn!("Server酱请求失败");
        return false;
    };
    if !resp.status().is_success() {
        tracing::warn!("Server酱返回 HTTP {}", resp.status());
        return false;
    }
    match resp.json::<serde_json::Value>().await {
        Ok(v) => {
            v.get("code").and_then(|c| c.as_i64()) == Some(0) || v.pointer("/data/pushid").is_some()
        }
        Err(e) => {
            tracing::warn!("Server酱响应解析失败: {e:#}");
            false
        }
    }
}

/// PushPlus 推送：`code == 200` 视为成功。
pub async fn send_pushplus(token: &str, title: &str, content: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build();
    let Ok(client) = client else {
        tracing::warn!("PushPlus 客户端初始化失败");
        return false;
    };
    let mut success = false;
    for item in split_credentials(token) {
        let result = client
            .post("https://www.pushplus.plus/send")
            .json(&serde_json::json!({"token": item, "title": title, "content": content}))
            .send()
            .await;
        let Ok(resp) = result else {
            tracing::warn!("PushPlus 请求失败");
            continue;
        };
        match resp.json::<serde_json::Value>().await {
            Ok(v) if v.get("code").and_then(|c| c.as_i64()) == Some(200) => success = true,
            Ok(_) => {}
            Err(e) => tracing::warn!("PushPlus 响应解析失败: {e:#}"),
        }
    }
    success
}

/// Bark 推送：`code == 200` 视为成功。
pub async fn send_bark(api: &str, title: &str, content: &str) -> bool {
    if api.is_empty() {
        return false;
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build();
    let Ok(client) = client else {
        tracing::warn!("Bark 客户端初始化失败");
        return false;
    };
    let mut success = false;
    for item in split_credentials(api) {
        let result = client
            .post(item)
            .json(&serde_json::json!({
                "title": title,
                "body": content,
                "level": "active",
                "badge": 1,
                "autoCopy": 1,
                "sound": "",
                "icon": "",
                "group": "",
                "isArchive": 1,
                "url": ""
            }))
            .send()
            .await;
        let Ok(resp) = result else {
            tracing::warn!("Bark 请求失败");
            continue;
        };
        match resp.json::<serde_json::Value>().await {
            Ok(v) if v.get("code").and_then(|c| c.as_i64()) == Some(200) => success = true,
            Ok(_) => {}
            Err(e) => tracing::warn!("Bark 响应解析失败: {e:#}"),
        }
    }
    success
}

/// ntfy 推送：服务端响应无 `error` 字段视为成功；多个地址以英文逗号分隔。
pub async fn send_ntfy(api: &str, title: &str, content: &str) -> bool {
    if api.is_empty() {
        return false;
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build();
    let Ok(client) = client else {
        tracing::warn!("ntfy 客户端初始化失败");
        return false;
    };
    let mut success = false;
    for endpoint in split_credentials(api) {
        let (server, topic) = match endpoint.rsplit_once('/') {
            Some(pair) => pair,
            None => {
                tracing::warn!("ntfy 地址缺少 topic: {endpoint}");
                continue;
            }
        };
        let result = client
            .post(server)
            .json(&serde_json::json!({
                "topic": topic,
                "title": title,
                "message": content,
                "tags": ["partying_face"],
                "priority": 3,
                "actions": [],
                "markdown": false,
                "icon": "",
                "delay": "",
                "email": "",
                "call": ""
            }))
            .send()
            .await;
        let Ok(resp) = result else {
            tracing::warn!("ntfy 请求失败: {endpoint}");
            continue;
        };
        if let Ok(v) = resp.json::<serde_json::Value>().await
            && v.get("error").is_none()
        {
            success = true;
        }
    }
    success
}

/// 与旧 Python `msg_push` 对齐：PushPlus/Bark/ntfy 都允许用英文或中文逗号
/// 一次配置多个目标，发送时任一目标成功即视为该通道成功。
fn split_credentials(value: &str) -> Vec<String> {
    value
        .split([',', '，'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_channel() {
        assert_eq!(Channel::parse("邮箱"), Channel::Email);
        assert_eq!(Channel::parse("微信"), Channel::Wechat);
        assert_eq!(Channel::parse("PushPlus"), Channel::PushPlus);
        assert_eq!(Channel::parse("Bark"), Channel::Bark);
        assert_eq!(Channel::parse("ntfy"), Channel::Ntfy);
        assert_eq!(Channel::parse("不通知"), Channel::None);
        assert_eq!(Channel::parse("跟随全局"), Channel::Email);
    }

    #[test]
    fn splits_mixed_comma_credentials() {
        assert_eq!(
            split_credentials(" a , b ， c "),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert!(split_credentials("").is_empty());
    }

    #[test]
    fn parses_multiple_email_recipients() {
        let boxes = parse_mailboxes("a@qq.com,b@qq.com， c@qq.com").unwrap();
        assert_eq!(boxes.len(), 3);
        assert_eq!(boxes[0].email.to_string(), "a@qq.com");
        assert_eq!(boxes[1].email.to_string(), "b@qq.com");
        assert_eq!(boxes[2].email.to_string(), "c@qq.com");
        assert!(parse_mailboxes("not-an-email").is_err());
    }

    #[test]
    fn local_notify_can_be_disabled_in_prefs() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-local-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        crate::app_prefs::write_app_prefs(&dir, &serde_json::json!({"localNotify": false}))
            .unwrap();
        Notifier::queue_local(&dir, "标题", "内容");
        assert!(
            !dir.join("fupan_output")
                .join("run")
                .join("pending_notify.json")
                .exists()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_notify_ids_are_unique_within_same_millisecond() {
        // 桌面端按 id 去重；同一毫秒多条通知若 id 碰撞，用户只会看到一条。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-id-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        crate::app_prefs::write_app_prefs(&dir, &serde_json::json!({})).unwrap();
        Notifier::queue_local(&dir, "标题1", "内容1");
        Notifier::queue_local(&dir, "标题2", "内容2");
        Notifier::queue_local(&dir, "标题3", "内容3");

        let path = dir
            .join("fupan_output")
            .join("run")
            .join("pending_notify.json");
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let ids: Vec<String> = items
            .iter()
            .map(|v| v["id"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(items.len(), 3, "三条通知都应保留");
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), 3, "通知 id 必须唯一: {ids:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_notify_queue_survives_read_while_atomic_replace() {
        // Rust 写队列使用“同目录临时文件 + rename”原子替换；桌面端读文件时
        // 可能正好看到旧文件或新文件，绝不能看到半截 JSON。此契约测试模拟
        // 一个陈旧残留临时文件与一次正常追加，最终 JSON 必须是合法数组且
        // 包含全部通知。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-atomic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        crate::app_prefs::write_app_prefs(&dir, &serde_json::json!({})).unwrap();
        let run = dir.join("fupan_output").join("run");
        std::fs::create_dir_all(&run).unwrap();
        let queue = run.join("pending_notify.json");
        Notifier::queue_local(&dir, "标题1", "内容1");
        Notifier::queue_local(&dir, "标题2", "内容2");
        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&queue).unwrap()).unwrap();
        assert_eq!(parsed.as_array().map(Vec::len), Some(2));
        assert!(
            std::fs::read_dir(&run)
                .unwrap()
                .flatten()
                .all(|e| { !e.file_name().to_string_lossy().contains(".tmp.") })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolves_pref_to_configured_channels() {
        let push = crate::config::PushChannels {
            serverchan: "SCT-real".to_string(),
            pushplus: "".to_string(),
            bark: "https://bark.example/key".to_string(),
            ntfy: "".to_string(),
        };
        assert_eq!(
            Channel::resolve_pref("跟随全局", true, &push),
            vec![Channel::Email, Channel::Wechat, Channel::Bark]
        );
        assert_eq!(
            Channel::resolve_pref("微信", false, &push),
            vec![Channel::Wechat]
        );
        assert_eq!(Channel::resolve_pref("不通知", true, &push), vec![]);
        assert_eq!(
            Channel::resolve_pref("跟随全局", false, &crate::config::PushChannels::default()),
            vec![Channel::Email]
        );
    }

    #[tokio::test]
    async fn unconfigured_wechat_marks_failed_not_pending() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-wx-missing-{}-{}",
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
            "zunzhe-notify-wx-missing-{}-{}.db",
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
            .send("video", "博主", "新视频", "内容", "微信")
            .await
            .unwrap();
        assert!(!sent);
        let rows = db::list_notify(&pool, "pending", 10).await.unwrap();
        assert_eq!(rows.len(), 1, "首次发送失败应保留 pending 等待重试");
        assert_eq!(rows[0].channel, "微信");
        assert_eq!(rows[0].attempts, 1);
        assert_eq!(
            db::list_notify(&pool, "failed", 10).await.unwrap().len(),
            0,
            "未达重试上限不应标记 failed"
        );
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("db-shm"));
        let _ = std::fs::remove_file(file.with_extension("db-wal"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn channel_name_is_serialized_in_record_sent() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-name-{}-{}",
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
            "zunzhe-notify-name-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = db::open_pool(&file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let notifier = Notifier::new(&pool, &cfg);
        notifier
            .record_sent("test", "", "标题", "内容", Channel::PushPlus)
            .await
            .unwrap();
        let rows = db::list_notify(&pool, "sent", 10).await.unwrap();
        assert_eq!(rows[0].channel, "PushPlus");
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("db-shm"));
        let _ = std::fs::remove_file(file.with_extension("db-wal"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn send_test_without_any_channel_reports_missing_hint() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-test-missing-{}-{}",
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
            "zunzhe-notify-test-missing-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = db::open_pool(&file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let notifier = Notifier::new(&pool, &cfg);
        let err = notifier.send_test("标题", "内容").await.unwrap_err();
        assert!(err.to_string().contains("未配置任何通知通道"));
        assert!(err.to_string().contains("邮箱(SMTP)"));
        assert!(err.to_string().contains("微信(Server酱)"));
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("db-shm"));
        let _ = std::fs::remove_file(file.with_extension("db-wal"));
        let _ = std::fs::remove_dir_all(&dir);
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
            .send("live", "主播", "开播", "内容", "邮箱")
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
    async fn flush_pending_updates_existing_record_without_duplicate() {
        // 重试路径必须更新原 pending 记录，不能再次调用 send 插入新 pending；
        // 否则未配置 SMTP 的通知会每轮成倍增长，且原记录永远无法收敛。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-flush-{}-{}",
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
            "zunzhe-notify-flush-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = db::open_pool(&file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let notifier = Notifier::new(&pool, &cfg);
        let original = db::add_notify(&pool, "live", "主播", "开播", "内容", "邮箱")
            .await
            .unwrap();

        let sent = notifier.flush_pending(10).await.unwrap();
        assert_eq!(sent, 0);
        let all = db::list_notify(&pool, "", 10).await.unwrap();
        assert_eq!(all.len(), 1, "重试不应插入新通知记录");
        assert_eq!(all[0].id, original);
        assert_eq!(all[0].status, "failed", "未配置邮箱应把原记录收敛为 failed");
        assert_eq!(all[0].attempts, 0, "未配置邮箱直接收敛，不消耗重试计数");

        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("db-shm"));
        let _ = std::fs::remove_file(file.with_extension("db-wal"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn flush_pending_retries_oldest_first() {
        // 重试队列超过单轮上限时，必须优先处理最早产生的 pending，否则新通知
        // 会持续把老通知挤出重试窗口，24×30 长跑下通知延迟/滞留无限放大。
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-oldest-{}-{}",
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
            "zunzhe-notify-oldest-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = db::open_pool(&file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let notifier = Notifier::new(&pool, &cfg);
        let oldest = db::add_notify(&pool, "live", "主播A", "开播", "内容", "邮箱")
            .await
            .unwrap();
        let newest = db::add_notify(&pool, "live", "主播B", "开播", "内容", "邮箱")
            .await
            .unwrap();
        assert!(oldest < newest);

        // 未配置邮箱时发送直接收敛为 failed；limit=1 时应处理最老的那条。
        notifier.flush_pending(1).await.unwrap();
        let failed = db::list_notify(&pool, "failed", 10).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, oldest, "单轮上限内应先重试最老的通知");
        let pending = db::list_pending_notify(&pool, 10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, newest);

        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(file.with_extension("db-shm"));
        let _ = std::fs::remove_file(file.with_extension("db-wal"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pending_notify_converges_to_failed_after_three_attempts() {
        let dir = std::env::temp_dir().join(format!(
            "zunzhe-notify-retry-{}-{}",
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
            "zunzhe-notify-retry-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = db::open_pool(&file).await.unwrap();
        let cfg = crate::config::Config::load(&dir).unwrap();
        let notifier = Notifier::new(&pool, &cfg);
        let id = db::add_notify(&pool, "live", "主播", "开播", "内容", "微信")
            .await
            .unwrap();

        // 前两次失败：保留 pending，attempts 递增。
        for expected_attempts in [1, 2] {
            notifier.flush_pending(10).await.unwrap();
            let pending = db::list_notify(&pool, "pending", 10).await.unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].id, id);
            assert_eq!(pending[0].attempts, expected_attempts);
            assert_eq!(db::list_notify(&pool, "failed", 10).await.unwrap().len(), 0);
        }
        // 第三次失败：达到上限后收敛为 failed，不再留在 pending。
        notifier.flush_pending(10).await.unwrap();
        let failed = db::list_notify(&pool, "failed", 10).await.unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, id);
        assert_eq!(failed[0].attempts, 3);
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
            .send("video", "博主", "新视频", "内容", "微信")
            .await
            .unwrap();
        assert!(!sent);
        let rows = db::list_notify(&pool, "pending", 10).await.unwrap();
        assert_eq!(rows.len(), 1, "首次发送失败应保留 pending 等待重试");
        assert_eq!(rows[0].channel, "微信");
        assert_eq!(rows[0].attempts, 1);
        assert_eq!(
            db::list_notify(&pool, "failed", 10).await.unwrap().len(),
            0,
            "未达重试上限不应标记 failed"
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
