use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use warp_harness_usage::api::{
    CodexUsage, Coverage, CoverageStatus, HarnessUsageRequest, HarnessUsageSnapshot, ToolCalls,
    UsagePayload, UsageSnapshot,
};

use super::{MAX_BODY_BYTES, encode_request};

fn request() -> HarnessUsageRequest {
    HarnessUsageRequest::new(
        7,
        3,
        Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap(),
        HarnessUsageSnapshot::Codex(UsageSnapshot {
            coverage: Coverage {
                token_status: CoverageStatus::Known,
                tool_status: CoverageStatus::Known,
            },
            payload: UsagePayload {
                usage: Some(CodexUsage {
                    input_tokens: Some(10),
                    cached_input_tokens: None,
                    output_tokens: Some(4),
                    reasoning_output_tokens: None,
                    total_tokens: Some(14),
                }),
                attribution: Vec::new(),
                tool_calls: Some(ToolCalls {
                    total: 0,
                    by_name: BTreeMap::new(),
                }),
            },
        }),
    )
}

#[test]
fn rejects_invalid_unusable_and_oversized_requests() {
    let mut request = request();
    request.execution_id = 0;
    assert!(encode_request(&request).is_err());

    let mut request = request();
    let HarnessUsageSnapshot::Codex(snapshot) = &mut request.snapshot else {
        unreachable!()
    };
    snapshot.coverage.token_status = CoverageStatus::Unavailable;
    snapshot.coverage.tool_status = CoverageStatus::Unavailable;
    assert!(encode_request(&request).is_err());

    snapshot.coverage.tool_status = CoverageStatus::Known;
    snapshot.payload.tool_calls.as_mut().unwrap().by_name =
        BTreeMap::from([("a".repeat(MAX_BODY_BYTES), 1)]);
    assert!(encode_request(&request).is_err());
}
