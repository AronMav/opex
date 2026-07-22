use opex_embedding::{RetryPolicy, ToolgateClient};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn probe_dim_returns_vector_length() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"object": "embedding", "index": 0, "embedding": vec![0.1f32; 1536]}],
            "model": ""
        })))
        .mount(&mock)
        .await;

    let client = ToolgateClient::new(mock.uri(), 0)
        .with_retry(RetryPolicy::NONE);
    let dim = client.probe_dim().await.unwrap();
    assert_eq!(dim, 1536);
}

#[tokio::test]
async fn probe_dim_retries_5xx_then_succeeds() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"object": "embedding", "index": 0, "embedding": vec![0.5f32; 384]}],
            "model": ""
        })))
        .mount(&mock)
        .await;

    let client = ToolgateClient::new(mock.uri(), 0);
    let dim = client.probe_dim().await.unwrap();
    assert_eq!(dim, 384);
}

#[tokio::test]
async fn probe_dim_does_not_retry_4xx() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(400))
        .expect(1) // ровно одна попытка
        .mount(&mock)
        .await;

    let client = ToolgateClient::new(mock.uri(), 0);
    assert!(client.probe_dim().await.is_err());
}
