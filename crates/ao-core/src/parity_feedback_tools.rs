use crate::parity_metadata::{atomic_write_file, parse_key_value_content};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

pub const FEEDBACK_TOOL_BUG_REPORT: &str = "bug_report";
pub const FEEDBACK_TOOL_IMPROVEMENT_SUGGESTION: &str = "improvement_suggestion";

fn normalize_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeedbackInput {
    pub title: String,
    pub body: String,
    pub evidence: Vec<String>,
    pub session: String,
    pub source: String,
    pub confidence: f64,
}

pub fn validate_feedback_tool_input(tool: &str, input: &FeedbackInput) -> Result<(), String> {
    if tool != FEEDBACK_TOOL_BUG_REPORT && tool != FEEDBACK_TOOL_IMPROVEMENT_SUGGESTION {
        return Err(format!("Unknown feedback tool: {tool}"));
    }
    let title = normalize_text(&input.title);
    let body = normalize_text(&input.body);
    let session = normalize_text(&input.session);
    let source = normalize_text(&input.source);
    if title.is_empty()
        || body.is_empty()
        || session.is_empty()
        || source.is_empty()
        || input.evidence.is_empty()
    {
        return Err("Missing required fields".into());
    }
    if !input.confidence.is_finite() || input.confidence < 0.0 || input.confidence > 1.0 {
        return Err("Invalid confidence".into());
    }
    Ok(())
}

pub fn generate_feedback_dedupe_key(tool: &str, input: &FeedbackInput) -> String {
    let mut evidence: Vec<String> = input
        .evidence
        .iter()
        .map(|e| normalize_text(e).to_lowercase())
        .collect();
    evidence.sort();

    let canonical = format!(
        "{}|{}|{}|{}|{}|{}",
        tool,
        normalize_text(&input.title).to_lowercase(),
        normalize_text(&input.body).to_lowercase(),
        normalize_text(&input.session).to_lowercase(),
        normalize_text(&input.source).to_lowercase(),
        evidence.join("|")
    );

    let mut h = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[derive(Debug, Clone, PartialEq)]
pub struct PersistedFeedbackReport {
    pub id: String,
    pub tool: String,
    pub created_at: String,
    pub dedupe_key: String,
    pub input: FeedbackInput,
}

fn serialize_report(report: &PersistedFeedbackReport) -> String {
    let mut lines: Vec<String> = vec![
        "version=1".into(),
        format!("id={}", report.id),
        format!("tool={}", report.tool),
        format!("createdAt={}", report.created_at),
        format!("dedupeKey={}", report.dedupe_key),
        format!("title={}", report.input.title),
        format!("body={}", report.input.body),
        format!("session={}", report.input.session),
        format!("source={}", report.input.source),
        format!("confidence={}", report.input.confidence),
    ];
    for (i, ev) in report.input.evidence.iter().enumerate() {
        lines.push(format!("evidence.{i}={ev}"));
    }
    lines.join("\n") + "\n"
}

fn is_report_file_name(name: &str) -> bool {
    name.starts_with("report_") && name.ends_with(".kv")
}

pub struct FeedbackReportStore {
    reports_dir: PathBuf,
}

impl FeedbackReportStore {
    pub fn new(reports_dir: impl Into<PathBuf>) -> Self {
        Self {
            reports_dir: reports_dir.into(),
        }
    }

    pub fn persist(
        &self,
        tool: &str,
        input: FeedbackInput,
    ) -> Result<PersistedFeedbackReport, String> {
        validate_feedback_tool_input(tool, &input)?;
        let created_at = iso_now();
        let dedupe_key = generate_feedback_dedupe_key(tool, &input);
        let id = format!("report_{}_{}", created_at.replace([':', '.'], "-"), short_id());
        let report = PersistedFeedbackReport {
            id: id.clone(),
            tool: tool.to_string(),
            created_at,
            dedupe_key,
            input,
        };
        std::fs::create_dir_all(&self.reports_dir).map_err(|e| e.to_string())?;
        let path = self.reports_dir.join(format!("{id}.kv"));
        atomic_write_file(&path, &serialize_report(&report)).map_err(|e| e.to_string())?;
        Ok(report)
    }

    pub fn list(&self) -> Vec<PersistedFeedbackReport> {
        let Ok(rd) = std::fs::read_dir(&self.reports_dir) else {
            return vec![];
        };
        let mut out = vec![];
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if !is_report_file_name(&name) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(ent.path()) else {
                continue;
            };
            let Ok(report) = parse_report_file(&content) else {
                continue;
            };
            out.push(report);
        }
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        out
    }
}

fn parse_report_file(content: &str) -> Result<PersistedFeedbackReport, String> {
    let raw = parse_key_value_content(content);
    let tool = raw.get("tool").cloned().unwrap_or_default();
    if tool != FEEDBACK_TOOL_BUG_REPORT && tool != FEEDBACK_TOOL_IMPROVEMENT_SUGGESTION {
        return Err("Invalid tool".into());
    }
    let mut evidence: Vec<(usize, String)> = raw
        .iter()
        .filter_map(|(k, v)| {
            if let Some(idx) = k.strip_prefix("evidence.") {
                Some((idx.parse::<usize>().unwrap_or(0), v.clone()))
            } else {
                None
            }
        })
        .collect();
    evidence.sort_by_key(|(i, _)| *i);
    let evidence = evidence.into_iter().map(|(_, v)| v).collect::<Vec<_>>();
    let input = FeedbackInput {
        title: raw.get("title").cloned().unwrap_or_default(),
        body: raw.get("body").cloned().unwrap_or_default(),
        evidence,
        session: raw.get("session").cloned().unwrap_or_default(),
        source: raw.get("source").cloned().unwrap_or_default(),
        confidence: raw
            .get("confidence")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(f64::NAN),
    };
    validate_feedback_tool_input(&tool, &input)?;
    Ok(PersistedFeedbackReport {
        id: raw.get("id").cloned().unwrap_or_default(),
        tool,
        created_at: raw.get("createdAt").cloned().unwrap_or_default(),
        dedupe_key: raw.get("dedupeKey").cloned().unwrap_or_default(),
        input,
    })
}

fn iso_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{ms}Z")
}

fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:08x}", n)
}

