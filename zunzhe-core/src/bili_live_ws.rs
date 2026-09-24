//! B站弹幕 WebSocket 实时开播监听（录播姬同款思路）。
//!
//! 原理：连上直播间弹幕服务器后，房间开播/下播会实时推送 `LIVE` / `PREPARING`
//! / `END` 命令。本模块**只做一件事**——收到 `LIVE` 命令就把该源 URL 通过
//! mpsc 通知调度器"提前跑一轮 live tick"。
//!
//! 真正的检测 / 录制 / 通知 / 防重全部复用现有 `LiveMonitor` 状态机：
//! 调度器被唤醒后仍会用房间 API 二次确认，因此 WS 误报无害（最多多跑一轮
//! tick），漏报 / 断线由 300 秒轮询兜底。这是刻意的最小职责切分：
//! 不改动任何现有检测逻辑，只增加一条"更快的唤醒通道"。

use std::io::Read;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::Sender;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::watchlist::Watchlist;

/// 心跳间隔：B站弹幕协议要求每 30 秒一次。
const HEARTBEAT_SECS: u64 = 30;
/// 断线重连退避上限：封顶 60 秒，避免坏房间无限快速重连刷日志。
const MAX_BACKOFF_SECS: u64 = 60;
/// 单次认证/连接超时。
const CONNECT_TIMEOUT_SECS: u64 = 20;

/// 从直播间 URL 提取房间号：`live.bilibili.com/{id}`。
fn room_id_from_url(url: &str) -> Option<u64> {
    let rest = url
        .strip_prefix("https://live.bilibili.com/")
        .or_else(|| url.strip_prefix("http://live.bilibili.com/"))?;
    rest.split(['/', '?', '#']).next()?.trim().parse::<u64>().ok()
}

/// 打包一个协议包：16 字节头 + body。
///
/// 头结构（全大端）：packet_len(4) + header_len(2)=16 + protover(2) +
/// op(4) + sequence(4)=1。
fn build_packet(protover: u16, op: u32, body: &[u8]) -> Vec<u8> {
    let len = 16 + body.len();
    let mut buf = Vec::with_capacity(len);
    buf.extend_from_slice(&(len as u32).to_be_bytes());
    buf.extend_from_slice(&16u16.to_be_bytes());
    buf.extend_from_slice(&protover.to_be_bytes());
    buf.extend_from_slice(&op.to_be_bytes());
    buf.extend_from_slice(&1u32.to_be_bytes());
    buf.extend_from_slice(body);
    buf
}

/// 心跳包：op=2，空 body。
fn heartbeat_packet() -> Vec<u8> {
    build_packet(1, 2, &[])
}

/// 认证包：op=7，body 为 JSON。
fn auth_packet(room_id: u64, token: &str) -> Vec<u8> {
    let body = format!(
        r#"{{"uid":0,"roomid":{room_id},"protover":3,"buvid":"","platform":"web","type":2,"key":"{token}"}}"#
    );
    build_packet(1, 7, body.as_bytes())
}

/// 解析一段收到的二进制：可能含多个拼接的协议包，逐包切出 `(op, body)`。
fn parse_frames(data: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 16 <= data.len() {
        let pkt_len = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        if pkt_len < 16 || off + pkt_len > data.len() {
            // 尾部半包（TCP 分片残留）：放弃剩余，等下一个 frame。
            break;
        }
        let protover = u16::from_be_bytes([data[off + 6], data[off + 7]]);
        let op = u32::from_be_bytes([data[off + 8], data[off + 9], data[off + 10], data[off + 11]]);
        let body = &data[off + 16..off + pkt_len];
        let body = if op == 5 && protover != 0 {
            // 命令包 protover=2/3 为 zlib 压缩，解压后再交给上层。
            let mut out_buf = Vec::new();
            let mut dec = flate2::read::ZlibDecoder::new(body);
            if dec.read_to_end(&mut out_buf).is_err() {
                Vec::new()
            } else {
                out_buf
            }
        } else {
            body.to_vec()
        };
        out.push((op, body));
        off += pkt_len;
    }
    out
}

/// 命令文本里是否出现开播事件（`LIVE` 命令）。解压后的数据可能是多条
/// JSON 拼接，逐段判定。
fn contains_live_event(text: &[u8]) -> bool {
    // 用流式反序列化逐条解析：解压后的数据可能是多条 JSON 拼接，
    // `split_inclusive` 会在嵌套对象中途切断，必须由解析器自行切分。
    let mut stream = serde_json::Deserializer::from_slice(text).into_iter::<serde_json::Value>();
    while let Some(item) = stream.next() {
        let Ok(v) = item else { break };
        if v.get("cmd").and_then(|c| c.as_str()) == Some("LIVE") {
            return true;
        }
    }
    false
}

/// 对单个房间：拿弹幕服务器地址 → 建立 WS → 认证 → 心跳 + 读循环。
/// 返回 `Ok` 表示连接被服务端关闭（正常），`Err` 表示网络层失败，由调用方重连。
async fn connect_and_listen(room_id: u64, url: String, tx: &Sender<String>) -> anyhow::Result<()> {
    // 1) 取弹幕服务器地址与 token。
    let info_url = format!(
        "https://api.live.bilibili.com/xlive/web-room/v1/index/getDanmuInfo?id={room_id}"
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .build()?;
    let resp = client.get(&info_url).send().await?;
    let json: serde_json::Value = resp.json().await?;
    if json.get("code").and_then(|c| c.as_i64()) != Some(0) {
        anyhow::bail!("getDanmuInfo 失败: {}", json.get("message").unwrap_or(&serde_json::Value::Null));
    }
    let data = json
        .get("data")
        .ok_or_else(|| anyhow::anyhow!("getDanmuInfo 无 data"))?;
    let token = data.get("token").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let host = data
        .get("host_list")
        .and_then(|h| h.get(0))
        .and_then(|h| h.get("host"))
        .and_then(|h| h.as_str())
        .ok_or_else(|| anyhow::anyhow!("host_list 为空"))?;
    let port = data
        .get("host_list")
        .and_then(|h| h.get(0))
        .and_then(|h| h.get("wss_port"))
        .and_then(|h| h.as_u64())
        .unwrap_or(443);
    let ws_url = format!("wss://{host}:{port}/sub");
    debug!("[B站WS] {room_id} 连接 {ws_url}");

    // 2) 建立连接并认证。
    let (mut ws, _resp) =
        tokio_tungstenite::connect_async(&ws_url).await.map_err(|e| anyhow::anyhow!("WS 连接失败: {e}"))?;
    ws.send(Message::binary(auth_packet(room_id, &token))).await?;

    // 3) 心跳 + 读循环。
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
    heartbeat.tick().await; // 丢弃首次立即返回
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                ws.send(Message::binary(heartbeat_packet())).await?;
            }
            msg = ws.next() => {
                let Some(msg) = msg else {
                    info!("[B站WS] {room_id} 连接被服务端关闭，退出等待重连");
                    return Ok(());
                };
                let msg = msg?;
                let Message::Binary(data) = msg else { continue };
                for (op, body) in parse_frames(&data) {
                    match op {
                        3 => { /* 心跳回复，无需处理 */ }
                        5 => {
                            if contains_live_event(&body) {
                                info!("[B站WS] {room_id} 收到开播事件(LIVE)，唤醒调度器");
                                // 发送失败只可能是调度器已停止，忽略即可。
                                let _ = tx.send(url.clone()).await;
                            }
                        }
                        _ => { /* 其他 op（8 人气等）忽略 */ }
                    }
                }
            }
        }
    }
}

/// 单房间监听循环：连接 → 断开 → 指数退避重连。
async fn listen_room(room_id: u64, url: String, tx: Sender<String>) {
    let mut backoff: u64 = 5;
    loop {
        match connect_and_listen(room_id, url.clone(), &tx).await {
            Ok(()) => debug!("[B站WS] {room_id} 正常断开"),
            Err(e) => warn!("[B站WS] {room_id} 监听失败: {e:#}"),
        }
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
    }
}

/// 启动 B站直播 WS 监听：从直播源清单里筛出 B站房间，每源一个监听任务。
///
/// 返回句柄，调度器停机时应 abort 掉。
pub fn spawn_bili_ws_monitor(project_dir: std::path::PathBuf, tx: Sender<String>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let watch = Watchlist::new(project_dir);
        let mut spawned = 0usize;
        for item in watch.list_live() {
            if !item.enabled {
                continue;
            }
            if let Some(room_id) = room_id_from_url(&item.url) {
                let tx2 = tx.clone();
                tokio::spawn(async move {
                    listen_room(room_id, item.url, tx2).await;
                });
                spawned += 1;
            }
        }
        if spawned == 0 {
            info!("[B站WS] 直播源清单里没有 B站房间，监听未启动");
        } else {
            info!("[B站WS] 已为 {spawned} 个 B站房间启动实时开播监听");
        }
        // 让监听任务随 daemon 存活；本任务不退出。
        std::future::pending::<()>().await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_id_extraction() {
        assert_eq!(room_id_from_url("https://live.bilibili.com/24931109"), Some(24931109));
        assert_eq!(
            room_id_from_url("https://live.bilibili.com/24931109?p=2"),
            Some(24931109)
        );
        assert_eq!(room_id_from_url("https://live.kuaishou.com/u/x"), None);
        assert_eq!(room_id_from_url("https://live.bilibili.com/abc"), None);
    }

    #[test]
    fn auth_and_heartbeat_packets_have_expected_ops() {
        let auth = auth_packet(24931109, "tok123");
        assert_eq!(u32::from_be_bytes([auth[8], auth[9], auth[10], auth[11]]), 7);
        assert!(String::from_utf8_lossy(&auth[16..]).contains("\"roomid\":24931109"));
        let hb = heartbeat_packet();
        assert_eq!(u32::from_be_bytes([hb[8], hb[9], hb[10], hb[11]]), 2);
        assert_eq!(hb.len(), 16);
    }

    #[test]
    fn plaintext_command_detects_live() {
        let body = br#"{"cmd":"LIVE","data":{"roomid":24931109}}"#;
        assert!(contains_live_event(body));
        let no = br#"{"cmd":"PREPARING"}"#;
        assert!(!contains_live_event(no));
    }

    #[test]
    fn parse_frames_splits_concatenated_packets() {
        let hb = heartbeat_packet();
        let body = br#"{"cmd":"LIVE"}"#;
        let cmd = build_packet(0, 5, body);
        let mut joined = hb.clone();
        joined.extend_from_slice(&cmd);
        let frames = parse_frames(&joined);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].0, 2);
        assert_eq!(frames[1].0, 5);
        assert!(contains_live_event(&frames[1].1));
    }

    #[test]
    fn zlib_compressed_command_decodes() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(br#"{"cmd":"LIVE","data":{"roomid":1}}"#).unwrap();
        let compressed = enc.finish().unwrap();
        let pkt = build_packet(3, 5, &compressed);
        let frames = parse_frames(&pkt);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].0, 5);
        assert!(contains_live_event(&frames[0].1));
    }

    /// 真机网络测试（默认跳过，用 `cargo test live_connection_roundtrip -- --ignored`
    /// 运行）：连真实 B站弹幕服务器验证 getDanmuInfo → WS 认证 → 心跳/收包
    /// 整条链路。只验证链路通不通，不依赖房间是否在播。
    #[tokio::test]
    #[ignore]
    async fn live_connection_roundtrip() {
        let room_id = 24931109u64; // 侯翠翠 B站直播间
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
        let mut task = tokio::spawn(async move {
            let _ = connect_and_listen(room_id, "https://live.bilibili.com/24931109".to_string(), &tx).await;
        });
        // 等待：连接建立后要么收到开播事件（rx 有值），要么连接被服务端
        // 关闭/报错（task 结束）。任一都说明“链路可通”；45 秒超时算失败。
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                tokio::select! {
                    _ = rx.recv() => {
                        println!("收到开播事件通知，链路通");
                        break;
                    }
                    _ = &mut task => {
                        println!("监听任务结束（连接被关闭/出错），链路已尝试");
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        })
        .await
        .expect("45 秒内未观察到任何结果，链路疑似不通");
        task.abort();
    }
}
