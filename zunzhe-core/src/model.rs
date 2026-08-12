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

/// 直播录制会话（SQLite live_sessions 行）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSession {
    pub id: i64,
    pub url: String,
    pub anchor: String,
    pub title: String,
    pub platform: String,
    pub stream_url: String,
    pub status: String,
    pub pid: i64,
    pub file_path: String,
    pub started_at: String,
    pub ended_at: String,
    pub error: String,
    pub updated_at: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn video_meta_round_trips_json() {
        let meta = VideoMeta {
            id: "BV1xx411c7mD".into(),
            platform: "bilibili".into(),
            url: "https://www.bilibili.com/video/BV1xx411c7mD".into(),
            title: "测试视频".into(),
            cover: "https://example.com/cover.jpg".into(),
            published_at: "2026-08-01 12:00:00".into(),
            duration: 120,
        };
        let json = serde_json::to_value(&meta).unwrap();
        let back: VideoMeta = serde_json::from_value(json).unwrap();
        assert_eq!(back.id, meta.id);
        assert_eq!(back.url, meta.url);
        assert_eq!(back.duration, 120);
    }

    #[test]
    fn training_record_serializes_with_type_tag() {
        let rec = TrainingRecord::Knowledge {
            anchor: "主播甲".into(),
            session: "20260801-120000".into(),
            date: "2026-08-01".into(),
            output_dir: "/tmp/session".into(),
            title: "知识点".into(),
            content: "内容".into(),
            start_sec: 1.5,
            end_sec: 3.0,
            value: 3,
        };
        let value = serde_json::to_value(rec).unwrap();
        assert_eq!(value["type"], json!("knowledge"));
        assert_eq!(value["anchor"], json!("主播甲"));
    }
}
