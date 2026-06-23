use opex_embedding::ToolgateClient;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn fetch_health_parses_active_embedding_provider() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ok",
            "active_providers": {"embedding": "OpenAI Embedding", "tts": null}
        })))
        .mount(&mock)
        .await;

    let client = ToolgateClient::new(mock.uri(), 0);
    let h = client.fetch_health().await.unwrap();
    assert_eq!(h.active_embedding_provider.as_deref(), Some("OpenAI Embedding"));
}

#[tokio::test]
async fn fetch_health_does_not_retry() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1) // EXACTLY один запрос, без retry
        .mount(&mock)
        .await;

    let client = ToolgateClient::new(mock.uri(), 0);
    let result = client.fetch_health().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn fetch_health_returns_none_when_provider_field_missing() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "ok"})))
        .mount(&mock)
        .await;

    let client = ToolgateClient::new(mock.uri(), 0);
    let h = client.fetch_health().await.unwrap();
    assert_eq!(h.active_embedding_provider, None);
}
