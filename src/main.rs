use bytes::Bytes;
use clap::Parser;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, header, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use log::LevelFilter;
use std::{
    convert::Infallible,
    net::SocketAddr,
    path::{Path, PathBuf},
    result::Result,
};
use tokio::{
    fs::{self},
    io::{self},
    net::TcpListener,
};
// setup logger
extern crate pretty_env_logger;
#[macro_use]
extern crate log;
// clap
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// local path to serve
    #[arg(short, long,default_value_t = {"./".to_string()})]
    serve_path: String,
}
async fn get_file_bytes(path: &Path) -> io::Result<Vec<u8>> {
    fs::read(path).await
}
async fn files_search(path: PathBuf) -> Result<PathBuf, io::Error> {
    let realpath = path;
    if realpath.exists() && realpath.is_file() {
        return Ok(realpath);
    } else {
        if realpath.with_extension("html").exists() {
            return Ok(realpath.with_extension("html"));
        } else if realpath.with_extension("htm").exists() {
            return Ok(realpath.with_extension("htm"));
        } else {
            error!("File not found: {:?}", realpath);
            return Err(io::Error::new(io::ErrorKind::NotFound, "File not found."));
        }
    }
}

fn is_dev() -> bool {
    cfg!(debug_assertions)
}
async fn error_text(main_text: String, err: &str) -> String {
    // if we are running in dev include the actual error
    if is_dev() {
        main_text + ": " + err
    }
    // otherwise don't
    else {
        main_text
    }
}
async fn resolve_path(
    mut requested_path: &str,
) -> Result<(PathBuf), Result<Response<Full<Bytes>>, Infallible>> {
    let args = Args::parse();

    if requested_path.is_empty() {
        requested_path = "index.html";
    }
    let p = requested_path;
    let path = Path::new(p);
    let step_dir = match std::env::current_dir() {
        Ok(dir) => dir,

        Err(e) => {
            // If we can't determine the current directory, fail safely
            let error = error_text("Internal server error".to_string(), &e.to_string()).await;
            error!("{} in getting current dir", error);
            return Err(Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from(error)))
                .unwrap()));
        }
    };
    let base_dir = step_dir.join(args.serve_path.clone());
    let canonical_base_dir = match canonicalise_base_dir(base_dir).await {
        Ok(value) => value,
        Err(value) => return Err(value),
    };
    let foundpath = match files_search(canonical_base_dir.join(path)).await {
        Ok(fp) => fp,
        Err(e) => {
            let error = error_text("File not found".to_string(), &e.to_string()).await;
            error!("{} in searching for file", error);
            debug!(
                "Requested path: {}",
                canonical_base_dir.join(path).display()
            );
            return Err(Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::from(error)))
                .unwrap()));
        }
    };
    let _ = match foundpath.canonicalize() {
        Ok(canonical_path) => {
            // Ensure the canonical path is within the canonical base directory
            if !canonical_path.starts_with(&canonical_base_dir) {
                warn!("Attempted directory traversal attack: {}", requested_path);
                return Err(Ok(Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Full::new(Bytes::from("Access denied")))
                    .unwrap()));
            }
            canonical_path
        }

        Err(e) => {
            // Canonicalization can fail for various reasons (file not found, permission issues, etc.)
            // give generic message for security
            let error = error_text("Access denied".to_string(), &e.to_string()).await;
            error!("{} in getting canonical path", error);
            debug!("Requested path: {}", requested_path);
            debug!("Base directory: {:?}", canonical_base_dir);
            debug!("Full path attempted: {:?}", canonical_base_dir.join(path));
            return Err(Ok(Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Full::new(Bytes::from(error)))
                .unwrap()));
        }
    };
    Ok(foundpath)
}

async fn canonicalise_base_dir(
    base_dir: PathBuf,
) -> Result<PathBuf, Result<Response<Full<Bytes>>, Infallible>> {
    let canonical_base_dir = match base_dir.canonicalize() {
        Ok(dir) => dir,

        Err(e) => {
            let error = error_text("Internal server error".to_string(), &e.to_string()).await;
            error!("{} in getting canonical base dir", error);
            debug!("Base directory attempted: {:?}", base_dir);
            return Err(Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from(error)))
                .unwrap()));
        }
    };
    Ok(canonical_base_dir)
}
async fn respond(
    request: Request<impl hyper::body::Body>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let requested_path = request.uri().path().trim_start_matches('/');
    // Handle empty path (default to index.html)
    let foundpath = match resolve_path(requested_path).await {
        Ok(value) => value,
        Err(value) => return value,
    };
    let getpath = foundpath.as_path();
    match get_file_bytes(getpath).await {
        Ok(content) => {
            let mut resp = Response::new(Full::new(Bytes::from(content)));
            // Set Content-Type based on file extension
            let mime = match getpath
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_lowercase())
            {
                Some(ext) => match ext.as_str() {
                    "html" | "htm" | "" => "text/html; charset=utf-8",
                    "css" => "text/css; charset=utf-8",
                    "js" => "text/javascript; charset=utf-8",
                    "mjs" => "text/javascript; charset=utf-8",
                    "json" => "application/json; charset=utf-8",
                    "svg" => "image/svg+xml",
                    "png" => "image/png",
                    "jpg" | "jpeg" => "image/jpeg",
                    "gif" => "image/gif",
                    "webp" => "image/webp",
                    "ico" => "image/x-icon",
                    "txt" | "log" => "text/plain; charset=utf-8",
                    "wasm" => "application/wasm",
                    "map" => "application/json; charset=utf-8",
                    _ => "application/octet-stream",
                },
                None => "application/octet-stream",
            };
            let _ = resp.headers_mut().try_append(
                header::CONTENT_TYPE,
                header::HeaderValue::from_str(mime).unwrap(),
            );
            // cache content
            let _ = resp.headers_mut().try_append(
                header::CACHE_CONTROL,
                header::HeaderValue::from_str("public, max-age=3600").unwrap(),
            );
            // standard headers
            let _ = resp.headers_mut().try_append(
                header::SERVER,
                header::HeaderValue::from_str("Simple-Rust-Server/0.1").unwrap(),
            );
            Ok(resp)
        }
        Err(e) => {
            error!("{}", e);
            debug!("Error reading file at {:?}", foundpath);
            let error = error_text("File not found".to_string(), &e.to_string()).await;
            Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::from(error)))
                .unwrap())
        }
    }
}

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut level = LevelFilter::Warn;
    if is_dev() {
        level = LevelFilter::Debug;
    }
    pretty_env_logger::formatted_builder()
        .filter_level(level)
        .init();
    // This address is localhost
    let addr: SocketAddr = ([127, 0, 0, 1], 3000).into();

    // Bind to the port and listen for incoming TCP connections
    let listener = TcpListener::bind(addr).await?;
    println!("Listening on http://{}", addr);
    loop {
        // When an incoming TCP connection is received grab a TCP stream for
        // client<->server communication.
        //
        // Note, this is a .await point, this loop will loop forever but is not a busy loop. The
        // .await point allows the Tokio runtime to pull the task off of the thread until the task
        // has work to do. In this case, a connection arrives on the port we are listening on and
        // the task is woken up, at which point the task is then put back on a thread, and is
        // driven forward by the runtime, eventually yielding a TCP stream.
        let (tcp, _) = listener.accept().await?;
        // Use an adapter to access something implementing `tokio::io` traits as if they implement
        // `hyper::rt` IO traits.
        let io = TokioIo::new(tcp);

        // Spin up a new task in Tokio so we can continue to listen for new TCP connection on the
        // current task without waiting for the processing of the HTTP1 connection we just received
        // to finish
        tokio::task::spawn(async move {
            // Handle the connection from the client using HTTP1 and pass any
            // HTTP requests received on that connection to the `hello` function
            if let Err(err) = http1::Builder::new()
                .timer(TokioTimer::new())
                .serve_connection(io, service_fn(respond))
                .await
            {
                error!("Error serving connection: {:?}", err);
            }
        });
    }
}
