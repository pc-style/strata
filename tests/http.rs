#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn raw(app: &Router, method: &str, path: &str, body: String) -> (u16, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = to_bytes(response.into_body(), 20 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| json!({"text":String::from_utf8_lossy(&body)})),
    )
}
async fn request(app: &Router, method: &str, path: &str, body: Value) -> Value {
    let (status, value) = raw(app, method, path, body.to_string()).await;
    assert_eq!(status, 200, "{method} {path}: {value}");
    value
}
fn mapping() -> Value {
    json!({"mappings":{"properties":{"title":{"type":"text"},"price":{"type":"double"},"date":{"type":"date"},"tag":{"type":"keyword"},"count":{"type":"long"}}}})
}

#[tokio::test]
async fn durable_crud_refresh_cursors_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let app = strata_search::router(dir.path()).unwrap();
    assert!(strata_search::router(dir.path()).is_err());
    request(&app, "PUT", "/books", mapping()).await;
    request(
        &app,
        "PUT",
        "/books/_doc/a",
        json!({"title":"Rust search","price":20.5,"date":1000,"count":9_007_199_254_740_993i64}),
    )
    .await;
    let page = request(&app, "POST", "/books/_search", json!({})).await;
    assert_eq!(page["hits"]["total"]["value"], 0);
    let get = request(&app, "GET", "/books/_doc/a", Value::Null).await;
    assert_eq!(get["_source"]["count"], 9_007_199_254_740_993i64);
    request(
        &app,
        "PUT",
        "/books/_doc/b?refresh=true",
        json!({"title":"Rust guide","price":3.5,"date":2000}),
    )
    .await;
    let search = json!({"query":{"bool":{"must":[{"match":{"title":"RUST"}}],"filter":[{"range":{"date":{"gte":1000,"lt":3000}}}]}},"sort":[{"field":"price","order":"asc"}],"size":1});
    let page = request(&app, "POST", "/books/_search", search.clone()).await;
    assert_eq!(page["hits"]["total"]["value"], 2);
    assert_eq!(page["hits"]["hits"][0]["_id"], "b");
    let mut next = search.clone();
    next["search_after"] = page["search_after"].clone();
    let last = request(&app, "POST", "/books/_search", next.clone()).await;
    assert_eq!(last["hits"]["total"]["value"], 2);
    assert_eq!(last["hits"]["hits"][0]["_id"], "a");
    assert!(last["search_after"].is_null());
    let mut changed = next.clone();
    changed["query"] = json!({"match_all":{}});
    assert_eq!(
        raw(&app, "POST", "/books/_search", changed.to_string())
            .await
            .0,
        400
    );
    let mut changed = next.clone();
    changed["sort"] = json!([{"field":"date"}]);
    assert_eq!(
        raw(&app, "POST", "/books/_search", changed.to_string())
            .await
            .0,
        400
    );
    request(
        &app,
        "PUT",
        "/books/_doc/a",
        json!({"title":"Rust revised","price":1.0,"date":1500}),
    )
    .await;
    assert_eq!(
        request(&app, "POST", "/books/_search", search.clone()).await["hits"]["hits"][0]["_id"],
        "b"
    );
    request(&app, "POST", "/books/_refresh", json!({})).await;
    assert_eq!(
        raw(&app, "POST", "/books/_search", next.to_string())
            .await
            .0,
        409
    );
    assert_eq!(
        request(&app, "POST", "/books/_search", search).await["hits"]["hits"][0]["_id"],
        "a"
    );
    request(&app, "DELETE", "/books/_doc/b?refresh=true", Value::Null).await;
    assert_eq!(
        raw(&app, "GET", "/books/_doc/b", String::new()).await.0,
        404
    );
    drop(app);
    let app = strata_search::router(dir.path()).unwrap();
    let page = request(&app, "POST", "/books/_search", json!({})).await;
    assert_eq!(page["hits"]["total"]["value"], 1);
    assert_eq!(page["hits"]["hits"][0]["_source"]["title"], "Rust revised");
}

#[tokio::test]
async fn bulk_partial_validation_and_same_id_ordering() {
    let dir = tempfile::tempdir().unwrap();
    let app = strata_search::router(dir.path()).unwrap();
    request(&app, "PUT", "/books", mapping()).await;
    let bulk = [
        json!({"index":{"_id":"a"}}),
        json!({"title":"first","price":2}),
        json!({"index":{"_id":"bad"}}),
        json!({"price":"wrong"}),
        json!({"delete":{"_id":"a"}}),
        json!({"index":{"_id":"a"}}),
        json!({"title":"last","price":5}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    let (status, result) = raw(&app, "POST", "/books/_bulk?refresh=true", bulk).await;
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["errors"], true);
    assert_eq!(result["items"][1]["index"]["status"], 400);
    let page = request(&app, "POST", "/books/_search", json!({})).await;
    assert_eq!(page["hits"]["total"]["value"], 1);
    assert_eq!(page["hits"]["hits"][0]["_source"]["title"], "last");
    assert_eq!(
        raw(
            &app,
            "PUT",
            "/books/_doc/a",
            json!({"price":"invalid"}).to_string()
        )
        .await
        .0,
        400
    );
    assert_eq!(
        request(&app, "GET", "/books/_doc/a", Value::Null).await["_source"]["price"],
        5
    );
    let malformed = "{\"delete\":{\"_id\":\"a\"}}\n{\"index\":{\"_id\":\"b\"}}\n";
    assert_eq!(
        raw(&app, "POST", "/books/_bulk", malformed.into()).await.0,
        400
    );
    assert_eq!(
        request(&app, "GET", "/books/_doc/a", Value::Null).await["_source"]["price"],
        5
    );
}

#[tokio::test]
async fn invalid_inputs_do_not_mutate_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let app = strata_search::router(dir.path()).unwrap();
    request(&app, "PUT", "/books", mapping()).await;
    assert_eq!(
        raw(&app, "PUT", "/books", mapping().to_string()).await.0,
        409
    );
    assert_eq!(
        raw(&app, "PUT", "/..%2Fescape", mapping().to_string())
            .await
            .0,
        400
    );
    assert_eq!(
        raw(
            &app,
            "PUT",
            "/books/_doc/a",
            json!({"unknown":1}).to_string()
        )
        .await
        .0,
        400
    );
    for body in [
        json!({"size":0}),
        json!({"size":10001}),
        json!({"sort":[{"field":"title"}]}),
        json!({"query":{"range":{"count":{"gt":1,"gte":2}}}}),
        json!({"query":{"range":{"count":{"gte":4,"lte":2}}}}),
        json!({"query":{"range":{"count":{"gte":2.5}}}}),
        json!({"query":{"typo":{}}}),
    ] {
        assert_eq!(
            raw(&app, "POST", "/books/_search", body.to_string())
                .await
                .0,
            400
        );
    }
    assert_eq!(
        raw(
            &app,
            "PUT",
            "/books/_doc/huge",
            format!("{{\"title\":\"{}\"}}", "x".repeat(2 * 1024 * 1024))
        )
        .await
        .0,
        413
    );
    assert_eq!(
        request(&app, "POST", "/books/_search", json!({})).await["hits"]["total"]["value"],
        0
    );
}
