#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_public_ip_classification() {
        assert!(is_public_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_public_ip("10.0.0.5".parse().unwrap()));
        assert!(!is_public_ip("100.64.0.1".parse().unwrap()));
        assert!(!is_public_ip("198.18.0.1".parse().unwrap()));
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn test_redact_headers() {
        let integration = HttpIntegration::new(vec![]);
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", HeaderValue::from_static("Bearer secret"));
        headers.insert("X-Api-Key", HeaderValue::from_static("12345"));
        headers.insert("Cookie", HeaderValue::from_static("session=abc"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));

        let redacted = integration.redact_headers(&headers);
        assert!(redacted.contains("authorization"));
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("Bearer secret"));

        assert!(redacted.contains("x-api-key"));
        assert!(!redacted.contains("12345"));

        assert!(redacted.contains("cookie"));
        assert!(!redacted.contains("session=abc"));
        assert!(redacted.contains("content-type"));
        assert!(redacted.contains("application/json"));
    }

    // --- Multipart Integration Tests ---

    use axum::Router;
    use axum::extract::Multipart;
    use axum::routing::post;
    use base64::Engine;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_multipart_upload() {
        // Start a local axum server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let app = Router::new().route("/upload", post(|mut multipart: Multipart| async move {
            let mut fields: HashMap<String, String> = HashMap::new();
            let mut files: Vec<(String, String, Option<String>, usize)> = Vec::new();

            while let Some(field) = multipart.next_field().await.unwrap() {
                let name = field.name().unwrap().to_string();
                if let Some(filename) = field.file_name() {
                    let filename = filename.to_string();
                    let content_type = field.content_type().map(|s| s.to_string());
                    let data = field.bytes().await.unwrap();
                    files.push((name, filename, content_type, data.len()));
                } else {
                    let data = field.text().await.unwrap();
                    fields.insert(name, data);
                }
            }

            // Return validation result as JSON
            serde_json::json!({
                "fields": fields,
                "files_count": files.len(),
                "file_info": files
            }).to_string()
        }));

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Construct client and make request
        let integration = HttpIntegration::new(vec![]);

        // Prepare base64 image
        let dummy_data = b"Hello World Image";
        let engine = base64::engine::general_purpose::STANDARD;
        let b64_data = engine.encode(dummy_data);

        let body_json = serde_json::json!({
            "type": "multipart",
            "fields": [
                {"name": "field1", "value": "value1"},
                {"name": "mode", "value": "test"}
            ],
            "files": [
                {
                    "name": "upfile",
                    "filename": "test.png",
                    "content_type": "image/png",
                    "data_base64": b64_data
                }
            ]
        });

        let args = serde_json::json!({
            "method": "POST",
            "url": format!("http://{}/upload", addr),
            "headers": null,
            "query": null,
            "body": body_json.to_string(),  // Stringify the body
            "timeout_ms": 5000,
            "follow_redirects": false
        });

        let result = integration
            .execute("http_request", &args.to_string())
            .await
            .expect("Request failed");

        let output: Value = serde_json::from_str(&result).unwrap();
        let resp_body: Value = serde_json::from_str(output["body_text"].as_str().unwrap()).unwrap();

        assert_eq!(resp_body["fields"]["field1"], "value1");
        assert_eq!(resp_body["fields"]["mode"], "test");
        assert_eq!(resp_body["files_count"], 1);

        let file_info = resp_body["file_info"].as_array().unwrap()[0]
            .as_array()
            .unwrap();
        assert_eq!(file_info[0], "upfile");
        assert_eq!(file_info[1], "test.png");
        assert_eq!(file_info[2], "image/png");
        assert_eq!(file_info[3], dummy_data.len());
    }

    #[tokio::test]
    async fn test_multipart_size_limit() {
        let integration = HttpIntegration::new(vec![]);

        // Create 600KB dummy data
        let large_data = vec![0u8; 600 * 1024];
        let engine = base64::engine::general_purpose::STANDARD;
        let b64_data = engine.encode(large_data);

        let body_json = serde_json::json!({
            "type": "multipart",
            "fields": [],
            "files": [
                {
                    "name": "too_big",
                    "filename": "big.bin",
                    "data_base64": b64_data
                }
            ]
        });

        let args = serde_json::json!({
            "method": "POST",
            "url": "http://127.0.0.1:1234/upload",
            "headers": null,
            "query": null,
            "body": body_json.to_string(),  // Stringify the body
            "timeout_ms": 1000,
            "follow_redirects": false
        });

        let result = integration.execute("http_request", &args.to_string()).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("exceeds 500KB limit")
        );
    }
}
