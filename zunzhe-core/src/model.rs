use serde::{Deserialize, Serialize};

/// 平台无关的视频元数据。各平台适配器只输出这一种结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoMeta {
    pub id: String,
    pub platform: String,
    pub url: String,
    pub title: String,
    pub cover: String,
    pub published_at: String,
    pub duration: i64,
}

/// 关注的主播/直播间（SQLite anchors 行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Anchor {
    pub id: i64,
    pub url: String,
    pub name: String,
    pub platform: String,
    pub enabled: bool,
    pub live: bool,
    pub notify_channel: String,
}

/// 视频库记录（SQLite videos 行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Video {
    pub id: String,
    pub platform: String,
    pub url: String,
    pub title: String,
    pub cover: String,
    pub cover_cache: String,
    pub published_at: String,
    pub duration: i64,
    pub anchor: String,
    pub source_url: String,
    pub downloaded: bool,
    pub downloaded_at: String,
}

/// 后台任务（SQLite tasks 行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: i64,
    pub kind: String,
    pub url: String,
    pub name: String,
    pub status: String,
    pub attempts: i64,
    pub payload: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 通知事件（SQLite notify 行）。
#[allow(dead_code)] // 通知审计/重发使用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyEvent {
    pub id: i64,
    pub event: String,
    pub anchor: String,
    pub title: String,
    pub content: String,
    pub channel: String,
    pub status: String,
    pub created_at: String,
}

/// 转写片段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// 转写结果（对应 Python 的 转写.json）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub source: String,
    pub language: String,
    pub duration: f64,
    pub segments: Vec<TranscriptSegment>,
}

/// 提炼出的知识点（对应 Python 的 知识点.json 元素）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgePoint {
    pub title: String,
    pub content: String,
    pub start_sec: f64,
    pub end_sec: f64,
    pub value: i64,
}

/// 训练数据 JSONL 记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TrainingRecord {
    Knowledge {
        anchor: String,
        session: String,
        date: String,
        output_dir: String,
        title: String,
        content: String,
        start_sec: f64,
        end_sec: f64,
        value: i64,
    },
    Transcript {
        anchor: String,
        session: String,
        date: String,
        output_dir: String,
        source: String,
        language: String,
        text: String,
        start_sec: f64,
        end_sec: f64,
    },
}
