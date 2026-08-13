use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::{KnowledgePoint, TrainingRecord, Transcript, TranscriptSegment};

/// 扫描 fupan_output/<主播>/<场次>/，把转写与知识点合并导出 JSONL。
pub fn export(fupan_dir: &Path, anchor_filter: &str, output: &str) -> Result<ExportSummary> {
    let out_path = if output.is_empty() {
        fupan_dir.join("training_data.jsonl")
    } else {
        PathBuf::from(output)
    };
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut records: Vec<TrainingRecord> = Vec::new();
    let mut sessions = 0usize;
    let mut knowledge = 0usize;
    let mut transcript_segments = 0usize;
    let mut errors: Vec<String> = Vec::new();

    if fupan_dir.exists() {
        let mut anchor_dirs: Vec<PathBuf> = fs::read_dir(fupan_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        anchor_dirs.sort();
        for anchor_dir in anchor_dirs {
            let anchor = anchor_dir
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if anchor == "run"
                || anchor == "logs"
                || anchor == "covers"
                || anchor == "training_data"
            {
                continue;
            }
            if !anchor_filter.is_empty() && anchor != anchor_filter {
                continue;
            }
            let mut session_dirs: Vec<PathBuf> = fs::read_dir(&anchor_dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            session_dirs.sort();
            for session_dir in session_dirs {
                if session_dir
                    .file_name()
                    .map(|s| s == "视频下载")
                    .unwrap_or(false)
                {
                    continue;
                }
                let session = session_dir
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let points_file = first_glob(&session_dir, "*.知识点.json");
                let transcript_file = first_glob(&session_dir, "*.转写.json");
                if points_file.is_none() && transcript_file.is_none() {
                    continue;
                }
                sessions += 1;
                let date = session_dir
                    .metadata()
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|modified| {
                        chrono::DateTime::from_timestamp(
                            modified
                                .duration_since(std::time::UNIX_EPOCH)
                                .ok()?
                                .as_secs() as i64,
                            0,
                        )
                        .map(|dt| {
                            dt.with_timezone(&chrono::Local)
                                .format("%Y-%m-%d")
                                .to_string()
                        })
                    })
                    .unwrap_or_default();
                let output_dir = session_dir.display().to_string();
                let base = (anchor.clone(), session.clone(), date, output_dir);

                if let Some(pf) = points_file {
                    match read_points(&pf) {
                        Ok(points) => {
                            for p in points {
                                knowledge += 1;
                                records.push(TrainingRecord::Knowledge {
                                    anchor: base.0.clone(),
                                    session: base.1.clone(),
                                    date: base.2.clone(),
                                    output_dir: base.3.clone(),
                                    title: p.title,
                                    content: p.content,
                                    start_sec: p.start_sec,
                                    end_sec: p.end_sec,
                                    value: p.value,
                                });
                            }
                        }
                        Err(e) => errors.push(format!("{}: {}", pf.display(), e)),
                    }
                }

                if let Some(tf) = transcript_file
                    && let Ok(transcript) = read_transcript(&tf)
                {
                    for seg in transcript.segments {
                        transcript_segments += 1;
                        records.push(TrainingRecord::Transcript {
                            anchor: base.0.clone(),
                            session: base.1.clone(),
                            date: base.2.clone(),
                            output_dir: base.3.clone(),
                            source: transcript.source.clone(),
                            language: transcript.language.clone(),
                            text: seg.text,
                            start_sec: seg.start,
                            end_sec: seg.end,
                        });
                    }
                }
            }
        }
    }

    let mut content = String::new();
    for record in &records {
        content.push_str(&serde_json::to_string(record)?);
        content.push('\n');
    }
    fs::write(&out_path, content)
        .with_context(|| format!("写 JSONL 失败: {}", out_path.display()))?;

    Ok(ExportSummary {
        ok: true,
        output: out_path.display().to_string(),
        records: records.len(),
        sessions,
        knowledge,
        transcript_segments,
        errors,
    })
}

fn first_glob(dir: &Path, pattern: &str) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|s| s.to_string_lossy().ends_with(&pattern[1..]))
                .unwrap_or(false)
        })
}

fn read_points(path: &Path) -> Result<Vec<KnowledgePoint>> {
    let text = fs::read_to_string(path)?;
    let raw: Vec<serde_json::Value> = serde_json::from_str(&text)?;
    let mut out = Vec::new();
    for v in raw {
        let title = v
            .get("标题")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if title.is_empty() {
            continue;
        }
        out.push(KnowledgePoint {
            title,
            content: v
                .get("内容")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            start_sec: v.get("开始秒").and_then(|x| x.as_f64()).unwrap_or(0.0),
            end_sec: v.get("结束秒").and_then(|x| x.as_f64()).unwrap_or(0.0),
            value: v.get("价值").and_then(|x| x.as_i64()).unwrap_or(3),
        });
    }
    Ok(out)
}

fn read_transcript(path: &Path) -> Result<Transcript> {
    let text = fs::read_to_string(path)?;
    let raw: serde_json::Value = serde_json::from_str(&text)?;
    let mut segments = Vec::new();
    if let Some(list) = raw.get("segments").and_then(|s| s.as_array()) {
        for seg in list {
            let text = seg
                .get("text")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                continue;
            }
            segments.push(TranscriptSegment {
                start: seg.get("start").and_then(|x| x.as_f64()).unwrap_or(0.0),
                end: seg.get("end").and_then(|x| x.as_f64()).unwrap_or(0.0),
                text,
            });
        }
    }
    Ok(Transcript {
        source: raw
            .get("source")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        language: raw
            .get("language")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        duration: raw.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0),
        segments,
    })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ExportSummary {
    pub ok: bool,
    pub output: String,
    pub records: usize,
    pub sessions: usize,
    pub knowledge: usize,
    pub transcript_segments: usize,
    pub errors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_jsonl_with_metadata() {
        let root = std::env::temp_dir().join(format!("zunzhe-training-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let session = root.join("主播甲").join("20260812_1015");
        fs::create_dir_all(&session).unwrap();
        let stem = "20260812_1015";
        fs::write(
            session.join(format!("{stem}.转写.json")),
            r#"{"source":"/tmp/a.mp4","language":"zh","duration":8.0,"segments":[
                {"start":0.0,"end":3.2,"text":"大家好"},
                {"start":3.2,"end":8.0,"text":"递归是函数调用自己"}]}"#,
        )
        .unwrap();
        fs::write(
            session.join(format!("{stem}.知识点.json")),
            r#"[{"标题":"递归定义","内容":"函数调用自己","开始秒":3.2,"结束秒":8.0,"价值":4}]"#,
        )
        .unwrap();

        let out = root.join("out.jsonl");
        let summary = export(&root, "", &out.display().to_string()).unwrap();
        assert_eq!(summary.sessions, 1);
        assert_eq!(summary.knowledge, 1);
        assert_eq!(summary.transcript_segments, 2);
        let text = fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"anchor\":\"主播甲\""));
        assert!(lines[0].contains("\"date\":\""));
        let _ = fs::remove_dir_all(&root);
    }
}
