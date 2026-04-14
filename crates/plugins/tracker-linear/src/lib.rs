//! Linear issue tracker plugin — HTTP to Linear GraphQL API.
//!
//! Auth via env var `LINEAR_API_TOKEN` (personal API key) and requests sent to:
//! `https://api.linear.app/graphql`.

use ao_core::{AoError, Issue, IssueState, Result, Tracker};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

const DEFAULT_ENDPOINT: &str = "https://api.linear.app/graphql";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct LinearTracker {
    endpoint: String,
    token: String,
    http: Client,
}

impl LinearTracker {
    pub fn new(token: impl Into<String>) -> Result<Self> {
        let http = Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| AoError::Other(format!("failed to build http client: {e}")))?;
        Ok(Self {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            token: token.into(),
            http,
        })
    }

    pub fn from_env() -> Result<Self> {
        let token = std::env::var("LINEAR_API_TOKEN")
            .or_else(|_| std::env::var("LINEAR_API_KEY"))
            .map_err(|_| {
                AoError::Other(
                    "missing Linear API token: set LINEAR_API_TOKEN (or LINEAR_API_KEY)".into(),
                )
            })?;
        Self::new(token)
    }

    fn normalize_identifier(id: &str) -> String {
        let trimmed = id.trim();
        if let Some(rest) = trimmed.strip_prefix("https://linear.app/") {
            // URL form: https://linear.app/<workspace>/issue/LIN-123/some-title
            // Also accept https://linear.app/issue/LIN-123/... (rare).
            let parts: Vec<&str> = rest.split('/').collect();
            if let Some(pos) = parts.iter().position(|p| *p == "issue") {
                if let Some(key) = parts.get(pos + 1) {
                    if !key.is_empty() {
                        return key.to_string();
                    }
                }
            }
        }
        trimmed.to_string()
    }

    async fn graphql<T: for<'de> Deserialize<'de>>(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<T> {
        let body = json!({ "query": query, "variables": variables });
        let resp = self
            .http
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", self.token.clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| AoError::Other(format!("Linear request failed: {e}")))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AoError::Other(format!("Linear response read failed: {e}")))?;
        if !status.is_success() {
            return Err(AoError::Other(format!(
                "Linear API error (HTTP {status}): {text}"
            )));
        }

        let parsed: GraphqlResponse<T> = serde_json::from_str(&text)
            .map_err(|e| AoError::Other(format!("Linear response parse failed: {e}")))?;
        if let Some(errors) = parsed.errors {
            let msg = errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(AoError::Other(format!("Linear GraphQL error: {msg}")));
        }
        parsed
            .data
            .ok_or_else(|| AoError::Other("Linear GraphQL response missing data".into()))
    }
}

#[async_trait]
impl Tracker for LinearTracker {
    fn name(&self) -> &str {
        "linear"
    }

    async fn get_issue(&self, identifier: &str) -> Result<Issue> {
        let id = Self::normalize_identifier(identifier);

        // Linear docs: issue(id: "LIN-123") works with the human identifier.
        let q = r#"
          query Issue($id: String!) {
            issue(id: $id) {
              id
              identifier
              title
              description
              url
              state { type name }
              labels { nodes { name } }
              assignee { name }
              team { key name }
              project { name }
              cycle { name startsAt endsAt }
            }
          }
        "#;

        let data: IssueQueryData = self.graphql(q, json!({ "id": id })).await?;
        let issue = data
            .issue
            .ok_or_else(|| AoError::Other("Linear issue not found".into()))?;

        Ok(Issue {
            id: issue.identifier.clone(),
            title: issue.title.unwrap_or_default(),
            description: issue.description.unwrap_or_default(),
            url: issue
                .url
                .unwrap_or_else(|| self.issue_url(&issue.identifier)),
            state: map_state(issue.state.as_ref().map(|s| s.r#type.as_str())),
            labels: issue
                .labels
                .as_ref()
                .map(|c| {
                    c.nodes
                        .iter()
                        .map(|n| n.name.clone())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            assignee: issue
                .assignee
                .as_ref()
                .and_then(|a| a.name.clone())
                .filter(|s| !s.is_empty()),
            milestone: issue
                .project
                .as_ref()
                .and_then(|p| p.name.clone())
                .filter(|s| !s.trim().is_empty()),
        })
    }

    async fn is_completed(&self, identifier: &str) -> Result<bool> {
        let issue = self.get_issue(identifier).await?;
        Ok(matches!(
            issue.state,
            IssueState::Closed | IssueState::Cancelled
        ))
    }

    fn issue_url(&self, identifier: &str) -> String {
        let id = Self::normalize_identifier(identifier);
        format!("https://linear.app/issue/{id}")
    }

    fn branch_name(&self, identifier: &str) -> String {
        let id = Self::normalize_identifier(identifier);
        format!("feat/linear-{id}")
    }

    fn generate_prompt(&self, issue: &Issue) -> String {
        // Keep it structured and “issue-first”: identifiers + URL + description.
        // We intentionally do not include token/config hints here.
        let labels = if issue.labels.is_empty() {
            "none".to_string()
        } else {
            issue.labels.join(", ")
        };
        let assignee = issue.assignee.as_deref().unwrap_or("unassigned");
        format!(
            "## Issue (Linear)\n\
**ID**: {id}\n\
**Title**: {title}\n\
**URL**: {url}\n\
**State**: {state}\n\
**Assignee**: {assignee}\n\
**Labels**: {labels}\n\
\n\
## Description\n\
{desc}\n",
            id = issue.id,
            title = issue.title,
            url = issue.url,
            state = format_issue_state(issue.state),
            assignee = assignee,
            labels = labels,
            desc = if issue.description.trim().is_empty() {
                "(no description)".to_string()
            } else {
                issue.description.clone()
            }
        )
    }
}

fn format_issue_state(s: IssueState) -> &'static str {
    match s {
        IssueState::Open => "open",
        IssueState::InProgress => "in_progress",
        IssueState::Closed => "closed",
        IssueState::Cancelled => "cancelled",
    }
}

fn map_state(state_type: Option<&str>) -> IssueState {
    match state_type
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase()
        .as_str()
    {
        "COMPLETED" => IssueState::Closed,
        "CANCELED" | "CANCELLED" => IssueState::Cancelled,
        "STARTED" => IssueState::InProgress,
        // Backlog, Triage, Unstarted, etc.
        _ => IssueState::Open,
    }
}

#[derive(Debug, Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct IssueQueryData {
    #[serde(default)]
    issue: Option<LinearIssue>,
}

#[derive(Debug, Deserialize)]
struct LinearIssue {
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    state: Option<LinearState>,
    #[serde(default)]
    labels: Option<LinearLabelConnection>,
    #[serde(default)]
    assignee: Option<LinearAssignee>,
    // Extra fields fetched for future prompt richness (kept for forward compat)
    #[allow(dead_code)]
    #[serde(default)]
    team: Option<LinearTeam>,
    #[allow(dead_code)]
    #[serde(default)]
    project: Option<LinearProject>,
    #[allow(dead_code)]
    #[serde(default)]
    cycle: Option<LinearCycle>,
}

#[derive(Debug, Deserialize)]
struct LinearState {
    #[serde(rename = "type")]
    r#type: String,
    #[allow(dead_code)]
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinearLabelConnection {
    #[serde(default)]
    nodes: Vec<LinearLabel>,
}

#[derive(Debug, Deserialize)]
struct LinearLabel {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct LinearAssignee {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinearTeam {
    #[allow(dead_code)]
    #[serde(default)]
    key: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinearProject {
    #[allow(dead_code)]
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinearCycle {
    #[allow(dead_code)]
    #[serde(default)]
    name: Option<String>,
    #[allow(dead_code)]
    #[serde(default, rename = "startsAt")]
    starts_at: Option<String>,
    #[allow(dead_code)]
    #[serde(default, rename = "endsAt")]
    ends_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_identifier_passthrough() {
        assert_eq!(LinearTracker::normalize_identifier("LIN-123"), "LIN-123");
        assert_eq!(LinearTracker::normalize_identifier("  ENG-7 "), "ENG-7");
    }

    #[test]
    fn normalize_identifier_extracts_from_url() {
        assert_eq!(
            LinearTracker::normalize_identifier("https://linear.app/acme/issue/LIN-123/some-title"),
            "LIN-123"
        );
        assert_eq!(
            LinearTracker::normalize_identifier("https://linear.app/issue/ENG-7/another"),
            "ENG-7"
        );
    }

    #[test]
    fn map_state_covers_common_types() {
        assert_eq!(map_state(Some("COMPLETED")), IssueState::Closed);
        assert_eq!(map_state(Some("canceled")), IssueState::Cancelled);
        assert_eq!(map_state(Some("STARTED")), IssueState::InProgress);
        assert_eq!(map_state(Some("BACKLOG")), IssueState::Open);
        assert_eq!(map_state(None), IssueState::Open);
    }
}
