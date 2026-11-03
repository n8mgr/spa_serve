use std::{
    convert::Infallible,
    env, io,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use cap_std::{ambient_authority, fs::Dir};
use headers::{ETag, HeaderMapExt, IfMatch, IfModifiedSince, IfNoneMatch, IfUnmodifiedSince};
use http_body_util::{Either, Full};
use hyper::{Method, Request, Response, StatusCode, header, service::service_fn};
use hyper_staticfile::{ResolvedFile, util::FileResponseBuilder};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto,
};
use percent_encoding::percent_decode_str;
use tokio::{fs::File, net::TcpListener};
use tokio_io_timeout::TimeoutStream;

type ResponseBody = Either<hyper_staticfile::Body, Full<Bytes>>;

#[derive(Clone)]
struct Server {
    root: Arc<Dir>,
}

impl Server {
    fn new(base: &Path) -> io::Result<Self> {
        let root = Dir::open_ambient_dir(base, ambient_authority())?;
        let index = root.open("index.html")?;
        if !index.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "index.html is not a regular file",
            ));
        }
        Ok(Self {
            root: Arc::new(root),
        })
    }

    async fn handle<B>(&self, request: Request<B>) -> Response<ResponseBody> {
        if request.method() != Method::GET && request.method() != Method::HEAD {
            let mut response = error_response(StatusCode::METHOD_NOT_ALLOWED, false);
            response
                .headers_mut()
                .insert(header::ALLOW, "GET, HEAD".parse().unwrap());
            return response;
        }
        let head = request.method() == Method::HEAD;
        let name = match request_path(request.uri().path()) {
            Ok(name) => name,
            Err(()) => return error_response(StatusCode::BAD_REQUEST, head),
        };
        let root = self.root.clone();
        let file = match tokio::task::spawn_blocking(move || resolve(&root, name)).await {
            Ok(Ok(file)) => file,
            Ok(Err(err)) if err.kind() == io::ErrorKind::PermissionDenied => {
                return error_response(StatusCode::FORBIDDEN, head);
            }
            _ => return error_response(StatusCode::INTERNAL_SERVER_ERROR, head),
        };
        let content_type = file.content_type.clone().unwrap();
        let size = file.size;
        let modified = file.modified;
        let mut builder = FileResponseBuilder::new();
        builder.request(&request).if_modified_since(None);
        if request
            .headers()
            .get(header::IF_RANGE)
            .is_some_and(|value| value.as_bytes().starts_with(b"W/"))
        {
            builder.range = None;
        }
        let mut response = match builder.build(file) {
            Ok(response) => response.map(Either::Left),
            Err(_) => return error_response(StatusCode::INTERNAL_SERVER_ERROR, head),
        };
        if let Some(status) = precondition_status(request.headers(), response.headers(), modified) {
            if status == StatusCode::PRECONDITION_FAILED {
                return error_response(status, head);
            }
            *response.status_mut() = status;
            *response.body_mut() = Either::Left(hyper_staticfile::Body::Empty);
            for header in [
                header::CONTENT_LENGTH,
                header::CONTENT_RANGE,
                header::CONTENT_TYPE,
            ] {
                response.headers_mut().remove(header);
            }
        } else if response.status().is_success() {
            response
                .headers_mut()
                .entry(header::CONTENT_TYPE)
                .or_insert(content_type.parse().unwrap());
        } else if response.status() == StatusCode::RANGE_NOT_SATISFIABLE {
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                format!("bytes */{size}").parse().unwrap(),
            );
        }
        response
    }
}

fn precondition_status(
    request: &header::HeaderMap,
    response: &header::HeaderMap,
    modified: Option<SystemTime>,
) -> Option<StatusCode> {
    let etag = response.typed_get::<ETag>();
    let modified = modified
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| UNIX_EPOCH + Duration::from_secs(duration.as_secs()));
    if request.contains_key(header::IF_MATCH) {
        let matches = request.typed_get::<IfMatch>().is_some_and(|condition| {
            condition.is_any()
                || etag
                    .as_ref()
                    .is_some_and(|etag| condition.precondition_passes(etag))
        });
        if !matches {
            return Some(StatusCode::PRECONDITION_FAILED);
        }
    } else if modified
        .zip(
            request
                .typed_get::<IfUnmodifiedSince>()
                .map(SystemTime::from),
        )
        .is_some_and(|(modified, date)| modified > date)
    {
        return Some(StatusCode::PRECONDITION_FAILED);
    }
    if request.contains_key(header::IF_NONE_MATCH) {
        if request
            .get(header::IF_NONE_MATCH)
            .is_some_and(|value| value == "*")
            || request.typed_get::<IfNoneMatch>().is_some_and(|condition| {
                etag.as_ref()
                    .is_some_and(|etag| !condition.precondition_passes(etag))
            })
        {
            return Some(StatusCode::NOT_MODIFIED);
        }
    } else if modified
        .zip(request.typed_get::<IfModifiedSince>().map(SystemTime::from))
        .is_some_and(|(modified, date)| modified <= date)
    {
        return Some(StatusCode::NOT_MODIFIED);
    }
    None
}

fn request_path(path: &str) -> Result<PathBuf, ()> {
    for (i, byte) in path.bytes().enumerate() {
        if byte == b'%'
            && !path
                .as_bytes()
                .get(i + 1..i + 3)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
        {
            return Err(());
        }
    }
    let decoded = percent_decode_str(path).decode_utf8().map_err(|_| ())?;
    if decoded.contains(['\0', '\\']) {
        return Err(());
    }
    let mut name = PathBuf::new();
    for part in decoded.split('/') {
        match part {
            ".." => return Err(()),
            "" | "." => (),
            _ => name.push(part),
        }
    }
    if name.as_os_str().is_empty() {
        name.push("index.html");
    }
    Ok(name)
}

fn resolve(root: &Dir, mut name: PathBuf) -> io::Result<ResolvedFile> {
    let file = match root.open(&name) {
        Ok(file) if !file.metadata()?.is_dir() => file,
        Ok(_) => {
            name = PathBuf::from("index.html");
            root.open(&name)?
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            name = PathBuf::from("index.html");
            root.open(&name)?
        }
        Err(err) => return Err(err),
    };
    let file = file.into_std();
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "not a regular file",
        ));
    }
    Ok(ResolvedFile {
        handle: File::from_std(file),
        content_type: Some(
            mime_guess::from_path(&name)
                .first_or_octet_stream()
                .to_string(),
        ),
        path: name,
        size: metadata.len(),
        modified: metadata.modified().ok(),
        encoding: None,
    })
}

fn error_response(status: StatusCode, head: bool) -> Response<ResponseBody> {
    let message = format!("{}\n", status.canonical_reason().unwrap_or("Error"));
    let body = if head {
        Bytes::new()
    } else {
        Bytes::from(message.clone())
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CONTENT_LENGTH, message.len())
        .body(Either::Right(Full::new(body)))
        .unwrap()
}

async fn run() -> io::Result<()> {
    let mut args = env::args_os().skip(1);
    let base = args
        .next()
        .filter(|_| args.next().is_none())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "usage: spa_serve <web-root>")
        })?;
    let server = Server::new(Path::new(&base))?;
    let listener = TcpListener::bind("0.0.0.0:80").await?;
    loop {
        let (stream, _) = listener.accept().await?;
        let server = server.clone();
        tokio::spawn(async move {
            let mut stream = TimeoutStream::new(stream);
            stream.set_read_timeout(Some(Duration::from_secs(60)));
            let service = service_fn(move |request| {
                let server = server.clone();
                async move { Ok::<_, Infallible>(server.handle(request).await) }
            });
            let mut builder = auto::Builder::new(TokioExecutor::new());
            builder
                .http1()
                .timer(TokioTimer::new())
                .header_read_timeout(Duration::from_secs(10));
            if let Err(err) = builder
                .serve_connection(TokioIo::new(Box::pin(stream)), service)
                .await
            {
                eprintln!("connection error: {err}");
            }
        });
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("spa_serve: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
