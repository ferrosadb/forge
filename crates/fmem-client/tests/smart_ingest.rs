use forge_fmem_client::transport::mock::{MockTransport, ScriptedResponse};
use forge_fmem_client::{smart_ingest, SmartIngestArgs};
use serde_json::json;

#[test]
fn smart_ingest_calls_fmem_tool_and_parses_created_response() {
    let transport = MockTransport::new();
    transport.expect_call_with(
        "tools/call",
        |params| {
            params.get("name").and_then(|v| v.as_str()) == Some("smart_ingest")
                && params
                    .get("arguments")
                    .and_then(|v| v.get("content"))
                    .and_then(|v| v.as_str())
                    == Some("Paper summary")
                && params
                    .get("arguments")
                    .and_then(|v| v.get("entity_name"))
                    .and_then(|v| v.as_str())
                    == Some("Example Paper")
        },
        ScriptedResponse::Ok(json!({
            "action": "Created",
            "entity_id": "11111111-1111-1111-1111-111111111111",
            "hint": "next"
        })),
    );

    let response = smart_ingest(
        &transport,
        SmartIngestArgs {
            session_id: Some("00000000-0000-0000-0000-000000000000".into()),
            content: "Paper summary".into(),
            entity_type: "document".into(),
            entity_name: Some("Example Paper".into()),
            embedding: None,
            source_fold_id: None,
        },
    )
    .expect("smart ingest succeeds");

    assert_eq!(response.action, "Created");
    assert_eq!(
        response.resolved_entity_id().as_deref(),
        Some("11111111-1111-1111-1111-111111111111")
    );
    transport.assert_done();
}

#[test]
fn smart_ingest_resolves_skipped_entity_id() {
    let transport = MockTransport::new();
    transport.expect_call(
        "tools/call",
        ScriptedResponse::Ok(json!({
            "action": "Skipped",
            "existing_entity_id": "22222222-2222-2222-2222-222222222222",
            "similarity": 0.98,
            "reason": "redundant"
        })),
    );

    let response = smart_ingest(
        &transport,
        SmartIngestArgs {
            session_id: None,
            content: "duplicate".into(),
            entity_type: "concept".into(),
            entity_name: None,
            embedding: None,
            source_fold_id: None,
        },
    )
    .expect("smart ingest succeeds");

    assert_eq!(response.action, "Skipped");
    assert_eq!(
        response.resolved_entity_id().as_deref(),
        Some("22222222-2222-2222-2222-222222222222")
    );
}
