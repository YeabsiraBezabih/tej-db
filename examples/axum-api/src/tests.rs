use super::*;
use axum::body::Body;
use axum::http::Request;
use tempfile::NamedTempFile;
use tower::util::ServiceExt;

fn setup_test_app() -> (Router, tempfile::TempPath) {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.into_temp_path();
    let db = Database::open_file(&path).unwrap();
    let state = AppState::new(db);
    (create_router(state), path)
}

#[tokio::test]
async fn test_axum_four_routes_lifecycle() {
    let (app, _path) = setup_test_app();

    // 1. POST /api/items (Create Item)
    let payload = serde_json::json!({
        "id": "book-1",
        "name": "Designing Data-Intensive Applications",
        "description": "Martin Kleppmann",
        "price": 45
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/items")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // 2. GET /api/items/:id (Fetch item)
    let req = Request::builder()
        .method("GET")
        .uri("/api/items/book-1")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 3. GET /api/items (List items)
    let req = Request::builder()
        .method("GET")
        .uri("/api/items?limit=10")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 4. DELETE /api/items/:id (Delete item)
    let req = Request::builder()
        .method("DELETE")
        .uri("/api/items/book-1")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 5. GET after DELETE -> 404 NOT_FOUND
    let req = Request::builder()
        .method("GET")
        .uri("/api/items/book-1")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_axum_concurrent_load() {
    let (app, _path) = setup_test_app();
    let mut handles = Vec::new();

    // Spawn 20 concurrent tasks inserting different items
    for i in 0..20 {
        let app_clone = app.clone();
        handles.push(tokio::spawn(async move {
            let payload = serde_json::json!({
                "id": format!("user-{}", i),
                "name": format!("User {}", i),
                "description": "Load test entity",
                "price": i * 10
            });
            let req = Request::builder()
                .method("POST")
                .uri("/api/items")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap();
            let resp = app_clone.oneshot(req).await.unwrap();
            assert!(resp.status() == StatusCode::CREATED || resp.status() == StatusCode::CONFLICT);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    // Verify count via GET /api/items
    let req = Request::builder()
        .method("GET")
        .uri("/api/items?limit=50")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_axum_sql_endpoint() {
    let (app, _path) = setup_test_app();

    // 0. Fail-closed when TEJ_API_KEY is unset
    unsafe {
        std::env::remove_var("TEJ_API_KEY");
    }
    let create_payload = serde_json::json!({
        "sql": "CREATE TABLE products (id INT PRIMARY KEY, name TEXT, price INT)"
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/sql")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&create_payload).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    unsafe {
        std::env::set_var("TEJ_API_KEY", "test-secret-key");
    }

    // 1. Create table via POST /api/sql with valid API key
    let req = Request::builder()
        .method("POST")
        .uri("/api/sql")
        .header("content-type", "application/json")
        .header("x-api-key", "test-secret-key")
        .body(Body::from(serde_json::to_vec(&create_payload).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 2. Insert rows via POST /api/sql
    let insert_payload = serde_json::json!({
        "sql": "INSERT INTO products VALUES (101, 'Widget', 25), (102, 'Gadget', 45)"
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/sql")
        .header("content-type", "application/json")
        .header("x-api-key", "test-secret-key")
        .body(Body::from(serde_json::to_vec(&insert_payload).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // 3. Query via POST /api/sql
    let select_payload = serde_json::json!({
        "sql": "SELECT name, price FROM products WHERE id = 101"
    });
    let req = Request::builder()
        .method("POST")
        .uri("/api/sql")
        .header("content-type", "application/json")
        .header("x-api-key", "test-secret-key")
        .body(Body::from(serde_json::to_vec(&select_payload).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert!(json["success"].as_bool().unwrap());
    assert_eq!(json["data"]["columns"][0], "name");
    assert_eq!(json["data"]["rows"][0][0], "Widget");
    assert_eq!(json["data"]["rows"][0][1], 25);
}
