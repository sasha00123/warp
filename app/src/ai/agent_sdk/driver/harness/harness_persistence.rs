use std::future::Future;
use std::sync::{Arc, Weak};

use anyhow::{Context, Result, anyhow};
use warp_errors::report_if_error;
use warpui::ModelSpawner;
use warpui::r#async::executor::Background;

use super::save_coordinator::{
    SaveCoordinator, SaveOperation, final_save_budget,
};
use super::transcript_persistence::UploadedTranscriptUsage;
use super::usage_reporting::{CaptureIdentity, UsageReporter};
use super::{AgentDriver, HarnessRunner, SavePoint};
use crate::ai::ambient_agents::AmbientAgentTaskId;
use crate::server::server_api::ServerApi;
use crate::server::server_api::harness_support::HarnessUsageCapability;

pub(crate) struct PersistenceOutcome {
    result: Result<()>,
    uploaded_usage: Option<UploadedTranscriptUsage>,
}

impl PersistenceOutcome {
    pub(super) fn block_only(result: Result<()>) -> Self {
        Self {
            result,
            uploaded_usage: None,
        }
    }

    #[cfg(test)]
    pub(super) fn into_result(self) -> Result<()> {
        self.result
    }
}


#[derive(Default)]
pub(crate) struct HarnessPersistence {
    saves: SaveCoordinator,
    usage: UsageReporter,
}

impl HarnessPersistence {
    pub(crate) fn initialize(
        &self,
        client: Arc<ServerApi>,
        task_id: Option<AmbientAgentTaskId>,
        capability: Option<HarnessUsageCapability>,
    ) {
        self.usage.initialize(client, task_id, capability);
    }

    pub(super) fn is_reporting_enabled(&self) -> bool {
        self.usage.is_enabled()
    }

    pub(super) fn begin_capture(&self) -> Option<CaptureIdentity> {
        self.usage.begin_capture()
    }

    pub(super) fn enqueue<R>(
        &self,
        runner: Weak<R>,
        save_point: SavePoint,
        foreground: ModelSpawner<AgentDriver>,
        background: Arc<Background>,
    )
    where
        R: HarnessRunner + ?Sized,
    {
        let publisher = background.clone();
        let operation: SaveOperation = Arc::new(move |save_point| {
            let runner = runner.clone();
            let foreground = foreground.clone();
            let publisher = publisher.clone();
            Box::pin(async move {
                let Some(runner) = runner.upgrade() else {
                    return Ok(());
                };
                if matches!(save_point, SavePoint::PostTurn) {
                    report_if_error!(
                        runner
                            .handle_session_update(&foreground)
                            .await
                            .context("Failed to handle harness session update before save")
                    );
                }
                let outcome = runner.save_conversation(save_point, &foreground).await;
                runner.persistence().complete(outcome, &publisher)
            })
        });
        self.saves.set_worker_operation(operation);
        self.saves.enqueue(save_point, &background);
    }

    pub(super) async fn finalize<R>(
        &self,
        runner: &R,
        foreground: &ModelSpawner<AgentDriver>,
        background: &Background,
    ) -> Result<()>
    where
        R: HarnessRunner + ?Sized,
    {
        let budget = final_save_budget();
        self.saves
            .finalize(
                async {
                    report_if_error!(
                        runner
                            .handle_session_update(foreground)
                            .await
                            .context("Failed to handle harness session update before final save")
                    );
                    let outcome = runner.save_conversation(SavePoint::Final, foreground).await;
                    self.complete(outcome, background)
                },
                self.usage.close_and_drain(budget),
                budget,
            )
            .await
    }

    fn complete(&self, outcome: PersistenceOutcome, background: &Background) -> Result<()> {
        if let Some(uploaded_usage) = outcome.uploaded_usage {
            self.usage.stage_uploaded(uploaded_usage, background);
        }
        outcome.result
    }
}

pub(super) async fn save_transcript_and_block(
    transcript: impl Future<Output = Result<UploadedTranscriptUsage>>,
    block: impl Future<Output = Result<()>>,
) -> PersistenceOutcome {
    let (transcript, block) = futures::join!(transcript, block);
    match (transcript, block) {
        (Ok(uploaded_usage), Ok(())) => PersistenceOutcome {
            result: Ok(()),
            uploaded_usage: Some(uploaded_usage),
        },
        (Ok(uploaded_usage), Err(error)) => PersistenceOutcome {
            result: Err(error.context("Harness block snapshot save failed")),
            uploaded_usage: Some(uploaded_usage),
        },
        (Err(error), Ok(())) => PersistenceOutcome {
            result: Err(error.context("Harness transcript save failed")),
            uploaded_usage: None,
        },
        (Err(transcript), Err(block)) => PersistenceOutcome {
            result: Err(anyhow!(
                "Harness transcript and block snapshot saves failed: \
                 transcript={transcript:#}; block={block:#}"
            )),
            uploaded_usage: None,
        },
    }
}

#[cfg(test)]
#[path = "harness_persistence_tests.rs"]
mod tests;
