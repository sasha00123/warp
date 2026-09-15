//! Task-authenticated publication of the server-supported subset of native harness usage.
//!
//! These wire types intentionally exclude producer-local scope, session identity, and diagnostics
//! so extractor changes do not implicitly change the publication contract.
use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Result, ensure};
use chrono::{DateTime, Utc};
use http::header::{CONTENT_TYPE, RETRY_AFTER};
use http_client::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use warp_harness_usage::{
    AttributedUsage, CacheCreation, ClaudeUsage, CodexUsage, CoverageStatus, NativePayload,
    ToolCalls, UsagePayload, UsageSnapshot,
};

use super::super::ServerApi;
use crate::ai::ambient_agents::AmbientAgentTaskId;

const MAX_BODY_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One cumulative capture, retained unchanged across publication retries.
#[derive(Clone, Serialize)]
pub struct HarnessUsageReport {
    pub execution_id: i64,
    pub capture_sequence: i64,
    pub captured_at: DateTime<Utc>,
    #[serde(flatten)]
    snapshot: HarnessSnapshotWire,
}

impl HarnessUsageReport {
    /// Copies a native snapshot into the stable contract, deriving its harness from the payload.
    ///
    /// Publication validation is deferred to [`Self::encode`].
    pub fn new(
        execution_id: i64,
        capture_sequence: i64,
        captured_at: DateTime<Utc>,
        snapshot: &UsageSnapshot,
    ) -> Self {
        let coverage = UsageCoverageWire {
            token_status: snapshot.coverage.token_status.into(),
            tool_status: snapshot.coverage.tool_status.into(),
        };
        let snapshot = match &snapshot.payload {
            NativePayload::Claude(payload) => HarnessSnapshotWire::ClaudeCode(UsageSnapshotWire {
                coverage,
                payload: payload.into(),
            }),
            NativePayload::Codex(payload) => HarnessSnapshotWire::Codex(UsageSnapshotWire {
                coverage,
                payload: payload.into(),
            }),
        };
        Self {
            execution_id,
            capture_sequence,
            captured_at,
            snapshot,
        }
    }

    /// Encodes a positive, usable capture within the publication body limit.
    pub(super) fn encode(&self) -> Result<Vec<u8>> {
        ensure!(
            self.execution_id > 0 && self.capture_sequence > 0,
            "Invalid harness capture identity"
        );
        ensure!(
            self.snapshot.has_usable_category(),
            "No usable harness usage category"
        );
        let body = serde_json::to_vec(self)?;
        ensure!(
            body.len() <= MAX_BODY_BYTES,
            "Harness usage body exceeds limit"
        );
        Ok(body)
    }
}

#[derive(Clone, Serialize)]
#[serde(
    tag = "harness",
    content = "snapshot",
    rename_all = "SCREAMING_SNAKE_CASE"
)]
enum HarnessSnapshotWire {
    ClaudeCode(UsageSnapshotWire<ClaudeTokenUsageWire>),
    Codex(UsageSnapshotWire<CodexTokenUsageWire>),
}

impl HarnessSnapshotWire {
    fn has_usable_category(&self) -> bool {
        let coverage = match self {
            Self::ClaudeCode(snapshot) => &snapshot.coverage,
            Self::Codex(snapshot) => &snapshot.coverage,
        };
        coverage.token_status != CoverageStatusWire::Unavailable
            || coverage.tool_status != CoverageStatusWire::Unavailable
    }
}

#[derive(Clone, Serialize)]
struct UsageSnapshotWire<T> {
    coverage: UsageCoverageWire,
    payload: UsagePayloadWire<T>,
}

#[derive(Clone, Serialize)]
struct UsageCoverageWire {
    token_status: CoverageStatusWire,
    tool_status: CoverageStatusWire,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CoverageStatusWire {
    Known,
    Partial,
    Unavailable,
}

impl From<CoverageStatus> for CoverageStatusWire {
    fn from(status: CoverageStatus) -> Self {
        match status {
            CoverageStatus::Known => Self::Known,
            CoverageStatus::Partial => Self::Partial,
            CoverageStatus::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Clone, Serialize)]
struct UsagePayloadWire<T> {
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<T>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attribution: Vec<AttributedUsageWire<T>>,
    #[serde(rename = "toolCalls", skip_serializing_if = "Option::is_none")]
    tool_calls: Option<ToolCallsWire>,
}

impl<T, U> From<&UsagePayload<T>> for UsagePayloadWire<U>
where
    for<'a> U: From<&'a T>,
{
    fn from(payload: &UsagePayload<T>) -> Self {
        Self {
            usage: payload.usage.as_ref().map(U::from),
            attribution: payload.attribution.iter().map(Into::into).collect(),
            tool_calls: payload.tool_calls.as_ref().map(Into::into),
        }
    }
}

#[derive(Clone, Serialize)]
struct AttributedUsageWire<T> {
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inference_geo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<String>,
    usage: T,
}

impl<T, U> From<&AttributedUsage<T>> for AttributedUsageWire<U>
where
    for<'a> U: From<&'a T>,
{
    fn from(usage: &AttributedUsage<T>) -> Self {
        Self {
            model: usage.attribution.model.clone(),
            service_tier: usage.attribution.service_tier.clone(),
            inference_geo: usage.attribution.inference_geo.clone(),
            speed: usage.attribution.speed.clone(),
            usage: (&usage.usage).into(),
        }
    }
}

#[derive(Clone, Serialize)]
struct ClaudeTokenUsageWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_read_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_creation_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_creation: Option<CacheCreationWire>,
}

impl From<&ClaudeUsage> for ClaudeTokenUsageWire {
    fn from(usage: &ClaudeUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_creation: usage.cache_creation.as_ref().map(Into::into),
        }
    }
}

#[derive(Clone, Serialize)]
struct CacheCreationWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    ephemeral_5m_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ephemeral_1h_input_tokens: Option<i64>,
}

impl From<&CacheCreation> for CacheCreationWire {
    fn from(usage: &CacheCreation) -> Self {
        Self {
            ephemeral_5m_input_tokens: usage.ephemeral_5m_input_tokens,
            ephemeral_1h_input_tokens: usage.ephemeral_1h_input_tokens,
        }
    }
}

#[derive(Clone, Serialize)]
struct CodexTokenUsageWire {
    #[serde(skip_serializing_if = "Option::is_none")]
    input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cached_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_tokens: Option<i64>,
}

impl From<&CodexUsage> for CodexTokenUsageWire {
    fn from(usage: &CodexUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_output_tokens: usage.reasoning_output_tokens,
            total_tokens: usage.total_tokens,
        }
    }
}

#[derive(Clone, Serialize)]
struct ToolCallsWire {
    total: i64,
    #[serde(rename = "byName")]
    by_name: BTreeMap<String, i64>,
}

impl From<&ToolCalls> for ToolCallsWire {
    fn from(tool_calls: &ToolCalls) -> Self {
        Self {
            total: tool_calls.total,
            by_name: tool_calls.by_name.clone(),
        }
    }
}

/// Execution ownership supplied only by authenticated, reporting-enabled startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct HarnessUsageContext {
    pub execution_id: i64,
}

pub(super) fn deserialize_harness_usage_context<'de, D>(
    deserializer: D,
) -> Result<Option<HarnessUsageContext>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    // Malformed capability data must not break raw transcript persistence or resume.
    Ok(serde_json::from_value::<HarnessUsageContext>(value)
        .ok()
        .filter(|context| context.execution_id > 0))
}

/// Server disposition for one cumulative usage capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessUsagePublicationStatus {
    /// The server retained this capture as the newest cumulative value.
    Accepted,
    /// The server had already retained a newer capture.
    IgnoredOlderCapture,
    /// The server had already retained this exact capture.
    Idempotent,
}

/// Acknowledgment of the retained capture identity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HarnessUsagePublication {
    pub status: HarnessUsagePublicationStatus,
    pub execution_id: i64,
    pub capture_sequence: i64,
}

/// Determines whether publication may retry or should stop for this execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessUsageErrorKind {
    /// A transient failure eligible for bounded retry.
    Retryable,
    /// The server does not support or has disabled publication.
    Disabled,
    /// The server rejected the task authentication.
    Unauthorized,
    /// The capture conflicts with the server's execution state.
    Conflict,
    /// The request failed local validation or server validation.
    InvalidReport,
    /// A successful response did not acknowledge a valid capture identity.
    InvalidResponse,
}

/// Safe publication diagnostics that never retain response bodies or credential errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Harness usage publication failed: {kind:?} (HTTP {status:?})")]
pub struct HarnessUsageError {
    pub kind: HarnessUsageErrorKind,
    pub status: Option<StatusCode>,
    pub retry_after: Option<Duration>,
}

impl HarnessUsageError {
    pub fn new(kind: HarnessUsageErrorKind) -> Self {
        Self {
            kind,
            status: None,
            retry_after: None,
        }
    }

    pub(super) fn from_request_preparation_error(_: anyhow::Error) -> Self {
        Self::new(HarnessUsageErrorKind::Retryable)
    }

    async fn from_response(response: http_client::Response) -> Self {
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| parse_harness_usage_retry_after(value, Utc::now()));
        let problem = response.json::<HarnessUsageProblem>().await.ok();
        let problem_type = problem.as_ref().map(|problem| problem.problem_type);
        let kind = if matches!(
            problem_type,
            Some(HarnessUsageProblemType::Disabled | HarnessUsageProblemType::Unsupported)
        ) || matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
        ) {
            HarnessUsageErrorKind::Disabled
        } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            HarnessUsageErrorKind::Unauthorized
        } else if matches!(
            status,
            StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED
        ) {
            HarnessUsageErrorKind::Conflict
        } else if matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
        ) || (status.is_server_error()
            && problem.and_then(|problem| problem.retryable) != Some(false))
        {
            HarnessUsageErrorKind::Retryable
        } else {
            HarnessUsageErrorKind::InvalidReport
        };
        Self {
            kind,
            status: Some(status),
            retry_after,
        }
    }
}

#[derive(Deserialize)]
struct HarnessUsageProblem {
    #[serde(rename = "type", default)]
    problem_type: HarnessUsageProblemType,
    retryable: Option<bool>,
}

#[derive(Clone, Copy, Default, Deserialize)]
enum HarnessUsageProblemType {
    #[serde(rename = "https://docs.warp.dev/errors/feature_not_available")]
    Disabled,
    #[serde(rename = "https://docs.warp.dev/errors/operation_not_supported")]
    Unsupported,
    #[default]
    #[serde(other)]
    Unknown,
}

pub(super) fn parse_harness_usage_retry_after(value: &str, now: DateTime<Utc>) -> Option<Duration> {
    value
        .trim()
        .parse::<u64>()
        .map(Duration::from_secs)
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc2822(value).ok().map(|date| {
                (date.with_timezone(&Utc) - now)
                    .to_std()
                    .unwrap_or_default()
            })
        })
}

impl ServerApi {
    /// Makes one task-authenticated publication attempt and validates its acknowledgment.
    pub async fn publish_harness_usage_for_task(
        &self,
        task_id: &AmbientAgentTaskId,
        report: &HarnessUsageReport,
    ) -> Result<HarnessUsagePublication, HarnessUsageError> {
        let body = report
            .encode()
            .map_err(|_| HarnessUsageError::new(HarnessUsageErrorKind::InvalidReport))?;
        let auth_token = self
            .get_or_refresh_access_token()
            .await
            .map_err(HarnessUsageError::from_request_preparation_error)?;
        let url = format!(
            "{}/api/v1/harness-support/harness-usage",
            crate::ChannelState::server_root_url()
        );
        let mut request = self
            .base_client
            .http_client()
            .post(&url)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .timeout(REQUEST_TIMEOUT);
        if let Some(token) = auth_token.as_bearer_token() {
            request = request.bearer_auth(token);
        }
        for (name, value) in self
            .ambient_agent_headers_for_task(task_id)
            .await
            .map_err(HarnessUsageError::from_request_preparation_error)?
        {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|_| HarnessUsageError::new(HarnessUsageErrorKind::Retryable))?;
        if !response.status().is_success() {
            self.observe_iap_challenge(&response);
            return Err(HarnessUsageError::from_response(response).await);
        }
        let publication = response
            .json::<HarnessUsagePublication>()
            .await
            .map_err(|error| {
                let kind = if error.is_decode() {
                    HarnessUsageErrorKind::InvalidResponse
                } else {
                    HarnessUsageErrorKind::Retryable
                };
                HarnessUsageError::new(kind)
            })?;
        if publication.execution_id <= 0
            || publication.capture_sequence <= 0
            || (publication.status != HarnessUsagePublicationStatus::IgnoredOlderCapture
                && (publication.execution_id != report.execution_id
                    || publication.capture_sequence != report.capture_sequence))
        {
            return Err(HarnessUsageError::new(
                HarnessUsageErrorKind::InvalidResponse,
            ));
        }
        Ok(publication)
    }
}

#[cfg(test)]
#[path = "harness_usage_wire_tests.rs"]
mod tests;
