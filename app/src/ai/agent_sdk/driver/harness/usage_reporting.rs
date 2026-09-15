use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use instant::Instant;
use parking_lot::Mutex;
use warp_harness_usage::ExtractionOutcome;
use warpui::r#async::{FutureExt as _, Timer};
use warpui::duration_with_jitter;

use crate::ai::ambient_agents::AmbientAgentTaskId;
use crate::server::server_api::ServerApi;
use crate::server::server_api::harness_support::{
    HarnessUsageContext, HarnessUsageError, HarnessUsageErrorKind, HarnessUsagePublication,
    HarnessUsageReport,
};

const MAX_PUBLICATION_ATTEMPTS: usize = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct ReportingContext {
    task_id: AmbientAgentTaskId,
    execution_id: i64,
    client: Arc<ServerApi>,
}

/// Starts uninitialized, initializes once as active or disabled, and only transitions from active
/// to disabled after a permanent publication failure or sequence exhaustion.
#[derive(Default)]
enum ReportingLifecycle {
    #[default]
    Uninitialized,
    Disabled,
    Active(ReportingContext),
}

#[derive(Default)]
struct ReportingState {
    lifecycle: ReportingLifecycle,
    sequence: i64,
    pending: Option<HarnessUsageReport>,
}

#[derive(Clone, Copy)]
pub(super) struct CaptureIdentity {
    execution_id: i64,
    sequence: i64,
}

impl CaptureIdentity {
    pub(super) fn build_usage_report(
        self,
        captured_at: DateTime<Utc>,
        outcome: ExtractionOutcome,
    ) -> Option<HarnessUsageReport> {
        match outcome {
            ExtractionOutcome::Usable(snapshot) => Some(HarnessUsageReport::new(
                self.execution_id,
                self.sequence,
                captured_at,
                &snapshot,
            )),
            ExtractionOutcome::Unavailable(reasons) => {
                log::debug!("Harness usage unavailable: reasons={reasons:?}");
                None
            }
        }
    }
}

/// Per-execution usage state advanced only by the runner's ordered save operations.
#[derive(Default)]
pub(crate) struct UsageReporter {
    state: Mutex<ReportingState>,
}

impl UsageReporter {
    pub(crate) fn initialize(
        &self,
        client: Arc<ServerApi>,
        task_id: Option<AmbientAgentTaskId>,
        context: Option<HarnessUsageContext>,
    ) {
        let lifecycle = task_id.zip(context).and_then(|(task_id, context)| {
            (context.execution_id > 0).then_some(ReportingLifecycle::Active(ReportingContext {
                task_id,
                execution_id: context.execution_id,
                client,
            }))
        });
        if lifecycle.is_none() {
            log::debug!("Harness usage disabled: no supported execution-bound startup context");
        }
        let mut state = self.state.lock();
        if matches!(&state.lifecycle, ReportingLifecycle::Uninitialized) {
            state.lifecycle = lifecycle.unwrap_or(ReportingLifecycle::Disabled);
        }
    }

    pub(super) fn is_enabled(&self) -> bool {
        matches!(&self.state.lock().lifecycle, ReportingLifecycle::Active(_))
    }

    pub(super) fn begin_capture(&self) -> Option<CaptureIdentity> {
        let mut state = self.state.lock();
        state.pending = None;
        let execution_id = match &state.lifecycle {
            ReportingLifecycle::Active(context) => context.execution_id,
            ReportingLifecycle::Uninitialized | ReportingLifecycle::Disabled => return None,
        };
        let Some(next) = state.sequence.checked_add(1) else {
            state.lifecycle = ReportingLifecycle::Disabled;
            log::warn!("Harness usage disabled: capture sequence exhausted");
            return None;
        };
        state.sequence = next;
        Some(CaptureIdentity {
            execution_id,
            sequence: next,
        })
    }

    pub(super) fn stage_uploaded_report(&self, report: Option<HarnessUsageReport>) {
        self.state.lock().pending = report;
    }

    /// Publishes the report staged by a successful raw transcript upload.
    pub(super) async fn publish_staged(&self) {
        let (report, context) = {
            let mut state = self.state.lock();
            let context = match &state.lifecycle {
                ReportingLifecycle::Active(context) => context.clone(),
                ReportingLifecycle::Uninitialized | ReportingLifecycle::Disabled => return,
            };
            let Some(report) = state.pending.take() else {
                return;
            };
            (report, context)
        };
        let started = Instant::now();
        let result = publish_with_retry(
            &report,
            |report| {
                let context = context.clone();
                async move {
                    context
                        .client
                        .publish_harness_usage_for_task(&context.task_id, report)
                        .with_timeout(REQUEST_TIMEOUT)
                        .await
                        .unwrap_or_else(|_| {
                            Err(HarnessUsageError::new(HarnessUsageErrorKind::Retryable))
                        })
                }
            },
            wait_before_retry,
        )
        .await;
        match result {
            Ok(publication) => log::debug!(
                "Harness usage published: status={:?} elapsed_ms={}",
                publication.status,
                started.elapsed().as_millis()
            ),
            Err(error) => {
                if matches!(
                    error.kind,
                    HarnessUsageErrorKind::Disabled
                        | HarnessUsageErrorKind::Unauthorized
                        | HarnessUsageErrorKind::Conflict
                        | HarnessUsageErrorKind::InvalidResponse
                ) {
                    let mut state = self.state.lock();
                    state.lifecycle = ReportingLifecycle::Disabled;
                    state.pending = None;
                }
                log::warn!("Harness usage not published: {error}");
            }
        }
    }
}

async fn publish_with_retry<'a, F, Fut, S, Sleep>(
    report: &'a HarnessUsageReport,
    mut send: F,
    mut sleep: S,
) -> Result<HarnessUsagePublication, HarnessUsageError>
where
    F: FnMut(&'a HarnessUsageReport) -> Fut,
    Fut: Future<Output = Result<HarnessUsagePublication, HarnessUsageError>>,
    S: FnMut(Duration) -> Sleep,
    Sleep: Future<Output = ()>,
{
    for attempt in 1..=MAX_PUBLICATION_ATTEMPTS {
        match send(report).await {
            Err(error)
                if error.kind == HarnessUsageErrorKind::Retryable
                    && attempt < MAX_PUBLICATION_ATTEMPTS =>
            {
                let delay = error
                    .retry_after
                    .unwrap_or_default()
                    .max(publication_backoff(attempt));
                // Retry-After cannot extend idle lifetime indefinitely.
                if delay > REQUEST_TIMEOUT {
                    return Err(error);
                }
                sleep(delay).await;
            }
            result => return result,
        }
    }
    unreachable!()
}

fn publication_backoff(attempt: usize) -> Duration {
    duration_with_jitter(Duration::from_secs(if attempt == 1 { 1 } else { 2 }), 0.2)
}

async fn wait_before_retry(delay: Duration) {
    Timer::after(delay).await;
}

#[cfg(test)]
#[path = "usage_reporting_tests.rs"]
mod tests;
