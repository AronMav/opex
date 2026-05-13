use hydeclaw_embedding::{RetryPolicy, ToolgateClient};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn embed_one_returns_vector() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"object": "embedding", "index": 0, "embedding": [0.1, 0.2, 0.3]}],
            "model": ""
        })))
        .mount(&mock)
        .await;

    let client = ToolgateClient::new(mock.uri(), 0).with_retry(RetryPolicy::NONE);
    let v = client.embed_one("hello").await.unwrap();
    assert_eq!(v, vec![0.1, 0.2, 0.3]);
}

#[tokio::test]
async fn embed_batch_preserves_order() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [
                {"object": "embedding", "index": 0, "embedding": [1.0]},
                {"object": "embedding", "index": 1, "embedding": [2.0]},
                {"object": "embedding", "index": 2, "embedding": [3.0]}
            ],
            "model": ""
        })))
        .mount(&mock)
        .await;

    let client = ToolgateClient::new(mock.uri(), 0).with_retry(RetryPolicy::NONE);
    let v = client.embed_batch(&["a", "b", "c"]).await.unwrap();
    assert_eq!(v, vec![vec![1.0], vec![2.0], vec![3.0]]);
}

#[tokio::test]
async fn embed_one_does_not_send_model_field() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(body_partial_json(json!({"input": "x"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"object": "embedding", "index": 0, "embedding": [0.1]}],
            "model": ""
        })))
        .mount(&mock)
        .await;

    let client = ToolgateClient::new(mock.uri(), 0).with_retry(RetryPolicy::NONE);
    client.embed_one("x").await.unwrap();

    let received = mock.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert!(body.get("model").is_none(), "model field must NOT be sent (Toolgate resolves internally)");
}
