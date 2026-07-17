//! SQLite orchestration for the Responses protocol continuation adapter.
//!
//! Protocol parsing and transcript merging live in `conduit-codec`; this module
//! only applies gateway storage, tenancy, expiry, and HTTP-facing errors.

use conduit_codec::{
    can_reset_responses_continuation, merge_responses_continuation, prepare_responses_continuation,
    reset_responses_continuation, ResponsesContinuation, ResponsesContinuationRequest,
};
use conduit_store::{ResponseContinuationRepo, StoreError};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ContinuationError {
    #[error("responses continuation store unavailable: {0}")]
    Store(#[from] StoreError),

    #[error("previous_response_id `{0}` is unknown or expired; resend the full input transcript")]
    Missing(String),

    #[error("previous_response_id `{0}` has invalid stored transcript data")]
    Corrupt(String),
}

/// Hash the raw downstream credential before it reaches SQLite. The hash is a
/// tenant boundary for continuations; it is not used for authentication.
pub fn continuation_key_scope(key: Option<&str>) -> String {
    match key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => hex::encode(blake3::hash(key.as_bytes()).as_bytes()),
        None => "anonymous".to_string(),
    }
}

pub async fn apply_continuation(
    repo: &ResponseContinuationRepo<'_>,
    body: Value,
    key_scope: &str,
) -> Result<Value, ContinuationError> {
    match prepare_responses_continuation(body) {
        ResponsesContinuationRequest::Ready(body) => Ok(body),
        ResponsesContinuationRequest::Incremental {
            previous_response_id,
            body,
        } => {
            let Some(stored) = repo.get(&previous_response_id, key_scope).await? else {
                if can_reset_responses_continuation(&body) {
                    return Ok(reset_responses_continuation(body));
                }
                return Err(ContinuationError::Missing(previous_response_id));
            };
            let continuation = ResponsesContinuation::from_json(
                &stored.input_items_json,
                &stored.output_items_json,
            )
            .map_err(|_| ContinuationError::Corrupt(previous_response_id))?;
            Ok(merge_responses_continuation(body, &continuation))
        }
    }
}

pub async fn persist_continuation(
    repo: &ResponseContinuationRepo<'_>,
    response_id: &str,
    key_scope: &str,
    input: Value,
    output: Vec<Value>,
) -> Result<(), StoreError> {
    if response_id.trim().is_empty() {
        return Ok(());
    }
    let continuation = ResponsesContinuation::new(input, output);
    let input_items_json = continuation
        .input_items_json()
        .map_err(|error| StoreError::Serialization(error.to_string()))?;
    let output_items_json = continuation
        .output_items_json()
        .map_err(|error| StoreError::Serialization(error.to_string()))?;
    repo.put(
        response_id,
        key_scope,
        &input_items_json,
        &output_items_json,
    )
    .await
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use conduit_store::open_db;

    #[tokio::test]
    async fn resolves_persisted_continuation_for_the_same_key_scope() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = ResponseContinuationRepo::new(&pool);
        persist_continuation(
            &repo,
            "resp_1",
            "key-a",
            json!([{"type": "message", "role": "user", "content": "first"}]),
            vec![json!({"type": "message", "role": "assistant", "content": "answer"})],
        )
        .await
        .unwrap();

        let merged = apply_continuation(
            &repo,
            json!({
                "previous_response_id": "resp_1",
                "input": [{"type": "message", "role": "user", "content": "continue"}]
            }),
            "key-a",
        )
        .await
        .unwrap();
        assert_eq!(merged["input"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn does_not_load_a_different_key_scope() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = ResponseContinuationRepo::new(&pool);
        persist_continuation(&repo, "resp_1", "key-a", json!([]), vec![])
            .await
            .unwrap();
        assert!(matches!(
            apply_continuation(
                &repo,
                json!({
                    "previous_response_id": "resp_1",
                    "input": [{"type": "function_call_output", "call_id": "call_1", "output": "x"}]
                }),
                "key-b",
            )
            .await,
            Err(ContinuationError::Missing(_))
        ));
    }

    #[tokio::test]
    async fn missing_plain_text_continuation_starts_a_new_turn() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let repo = ResponseContinuationRepo::new(&pool);
        let body = apply_continuation(
            &repo,
            json!({"previous_response_id": "resp_old", "input": "continue"}),
            "key",
        )
        .await
        .unwrap();
        assert!(body.get("previous_response_id").is_none());
        assert_eq!(body["input"], "continue");
    }
}
