use std::fs;

use super::*;
use http_body_util::BodyExt;
use tempfile::TempDir;

const INDEX: &str = "<!doctype html><title>Vue</title>";
const ASSET: &str = "console.log('Vue');";

fn fixture(index: &str) -> (TempDir, Server) {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("index.html"), index).unwrap();
    fs::create_dir(directory.path().join("assets")).unwrap();
    fs::write(directory.path().join("assets/app.js"), ASSET).unwrap();
    let server = Server::new(directory.path()).unwrap();
    (directory, server)
}

fn request(method: &str, path: &str) -> Request<()> {
    Request::builder()
        .method(method)
        .uri(path)
        .body(())
        .unwrap()
}

async fn body(response: Response<ResponseBody>) -> Bytes {
    response.into_body().collect().await.unwrap().to_bytes()
}

#[tokio::test]
async fn routes_assets_and_head_requests() {
    let (_directory, server) = fixture(INDEX);
    for (path, expected) in [
        ("/", INDEX),
        ("/index.html", INDEX),
        ("/users/123?tab=profile", INDEX),
        ("/users/123/", INDEX),
        ("/missing.js", INDEX),
        ("/assets/", INDEX),
        ("/assets/app.js", ASSET),
        ("/assets//app.js", ASSET),
    ] {
        for method in ["GET", "HEAD"] {
            let response = server.handle(request(method, path)).await;
            assert_eq!(response.status(), StatusCode::OK, "{method} {path}");
            assert!(response.headers().contains_key(header::LAST_MODIFIED));
            assert_eq!(
                response.headers()[header::CONTENT_LENGTH],
                expected.len().to_string()
            );
            if expected == INDEX {
                assert!(
                    response.headers()[header::CONTENT_TYPE]
                        .to_str()
                        .unwrap()
                        .starts_with("text/html")
                );
            } else {
                assert!(
                    response.headers()[header::CONTENT_TYPE]
                        .to_str()
                        .unwrap()
                        .contains("javascript")
                );
            }
            assert_eq!(
                body(response).await,
                if method == "HEAD" { "" } else { expected }
            );
        }
    }
}

#[tokio::test]
async fn rejects_unsupported_methods_and_paths() {
    let (_directory, server) = fixture(INDEX);
    for method in ["POST", "PUT", "DELETE", "OPTIONS"] {
        let response = server.handle(request(method, "/")).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "GET, HEAD");
    }
    for path in [
        "/../secret",
        "/%2e%2e/secret",
        "/assets/../../secret",
        "/%00",
        "/%ff",
        "/..%5csecret",
        "/bad%",
        "/bad%GG",
    ] {
        let response = server.handle(request("GET", path)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
    let response = server.handle(request("HEAD", "/../secret")).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(body(response).await.is_empty());
}

#[tokio::test]
async fn ranges_and_conditional_requests() {
    let (_directory, server) = fixture(INDEX);
    for (path, content) in [
        ("/", INDEX),
        ("/users/123", INDEX),
        ("/assets/app.js", ASSET),
    ] {
        let response = server.handle(request("GET", path)).await;
        let modified = response.headers()[header::LAST_MODIFIED].clone();
        let mut req = request("GET", path);
        req.headers_mut()
            .insert(header::IF_MODIFIED_SINCE, modified.clone());
        let response = server.handle(req).await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert!(body(response).await.is_empty());

        let mut req = request("GET", path);
        req.headers_mut()
            .insert(header::RANGE, "bytes=2-9".parse().unwrap());
        req.headers_mut().insert(header::IF_RANGE, modified);
        let response = server.handle(req).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert!(response.headers().contains_key(header::CONTENT_TYPE));
        assert_eq!(body(response).await, &content.as_bytes()[2..10]);

        let mut req = request("GET", path);
        req.headers_mut()
            .insert(header::RANGE, "bytes=2-9".parse().unwrap());
        req.headers_mut().insert(
            header::IF_RANGE,
            "Fri, 09 Aug 1996 14:21:40 GMT".parse().unwrap(),
        );
        let response = server.handle(req).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, content);

        let mut req = request("GET", path);
        req.headers_mut()
            .insert(header::RANGE, "bytes=999999-".parse().unwrap());
        assert_eq!(
            server.handle(req).await.status(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );

        let mut req = request("GET", path);
        req.headers_mut()
            .insert(header::RANGE, "bytes=0-1,4-5".parse().unwrap());
        let response = server.handle(req).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert!(
            response.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("multipart/byteranges")
        );
        let data = body(response).await;
        assert!(
            data.windows(2)
                .any(|part| part == &content.as_bytes()[0..2])
        );
        assert!(
            data.windows(2)
                .any(|part| part == &content.as_bytes()[4..6])
        );
    }
}

#[tokio::test]
async fn conditional_header_precedence() {
    let (_directory, server) = fixture(INDEX);
    let response = server.handle(request("GET", "/")).await;
    let etag = response.headers()[header::ETAG].clone();
    let modified = response.headers()[header::LAST_MODIFIED].clone();
    for (name, value, expected) in [
        (
            header::IF_NONE_MATCH,
            etag.clone(),
            StatusCode::NOT_MODIFIED,
        ),
        (
            header::IF_NONE_MATCH,
            "*".parse().unwrap(),
            StatusCode::NOT_MODIFIED,
        ),
        (header::IF_MATCH, "*".parse().unwrap(), StatusCode::OK),
        (
            header::IF_MATCH,
            etag.clone(),
            StatusCode::PRECONDITION_FAILED,
        ),
        (
            header::IF_UNMODIFIED_SINCE,
            "Fri, 09 Aug 1996 14:21:40 GMT".parse().unwrap(),
            StatusCode::PRECONDITION_FAILED,
        ),
    ] {
        let mut req = request("GET", "/");
        req.headers_mut().insert(name, value);
        let response = server.handle(req).await;
        assert_eq!(response.status(), expected);
        if expected == StatusCode::NOT_MODIFIED {
            assert_eq!(response.headers()[header::ETAG], etag);
            assert!(body(response).await.is_empty());
        }
    }
    let mut req = request("GET", "/");
    req.headers_mut()
        .insert(header::IF_NONE_MATCH, "\"old\"".parse().unwrap());
    req.headers_mut()
        .insert(header::IF_MODIFIED_SINCE, modified);
    assert_eq!(server.handle(req).await.status(), StatusCode::OK);

    let mut req = request("GET", "/");
    req.headers_mut()
        .insert(header::IF_MATCH, "*".parse().unwrap());
    req.headers_mut().insert(
        header::IF_UNMODIFIED_SINCE,
        "Fri, 09 Aug 1996 14:21:40 GMT".parse().unwrap(),
    );
    assert_eq!(server.handle(req).await.status(), StatusCode::OK);

    let mut req = request("GET", "/");
    req.headers_mut().insert(header::IF_RANGE, etag);
    req.headers_mut()
        .insert(header::RANGE, "bytes=2-9".parse().unwrap());
    let response = server.handle(req).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, INDEX);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_index_requests_have_independent_file_positions() {
    let index = INDEX.repeat(4096);
    let (_directory, server) = fixture(&index);
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..64 {
        let server = server.clone();
        let index = index.clone();
        tasks.spawn(async move {
            let path = ["/", "/index.html", "/users/123"][i % 3];
            let mut req = request("GET", path);
            let (status, expected) = if i % 2 == 0 {
                req.headers_mut()
                    .insert(header::RANGE, "bytes=7-1023".parse().unwrap());
                (StatusCode::PARTIAL_CONTENT, &index[7..1024])
            } else {
                (StatusCode::OK, index.as_str())
            };
            let response = server.handle(req).await;
            assert_eq!(response.status(), status);
            assert_eq!(body(response).await, expected);
        });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn confines_symlinks_to_web_root() {
    use std::os::unix::fs::symlink;

    let (directory, server) = fixture(INDEX);
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), "secret").unwrap();
    symlink("index.html", directory.path().join("inside")).unwrap();
    symlink(
        outside.path().join("secret"),
        directory.path().join("outside"),
    )
    .unwrap();
    symlink(outside.path(), directory.path().join("outside-dir")).unwrap();
    symlink("loop", directory.path().join("loop")).unwrap();
    assert_eq!(
        body(server.handle(request("GET", "/inside")).await).await,
        INDEX
    );
    for path in [
        "/outside",
        "/outside-dir/secret",
        "/outside-dir/missing",
        "/loop",
    ] {
        let response = server.handle(request("GET", path)).await;
        assert!(
            response.status().is_client_error() || response.status().is_server_error(),
            "{path}"
        );
        let data = body(response).await;
        assert!(!data.windows(6).any(|part| part == b"secret"));
        assert!(!String::from_utf8_lossy(&data).contains(directory.path().to_str().unwrap()));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn permission_errors_are_not_spa_responses() {
    use std::os::unix::fs::PermissionsExt;

    let (directory, server) = fixture(INDEX);
    let path = directory.path().join("private");
    fs::write(&path, "secret").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&path).is_ok() {
        return;
    }
    assert_eq!(
        server.handle(request("GET", "/private")).await.status(),
        StatusCode::FORBIDDEN
    );
}

#[test]
fn startup_requires_regular_index() {
    let directory = tempfile::tempdir().unwrap();
    assert!(Server::new(directory.path()).is_err());
    fs::create_dir(directory.path().join("index.html")).unwrap();
    assert!(Server::new(directory.path()).is_err());
}

#[tokio::test]
async fn missing_index_after_startup_is_an_error() {
    let (directory, server) = fixture(INDEX);
    fs::remove_file(directory.path().join("index.html")).unwrap();
    assert_eq!(
        server.handle(request("GET", "/missing")).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
}
