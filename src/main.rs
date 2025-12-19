use bytes::Bytes;
use clap::Parser;
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, header, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{convert::Infallible, net::SocketAddr, path::Path, result::Result};
use tokio::{
    fs::{self, File},
    io::{self, AsyncReadExt, BufReader},
    net::TcpListener,
};

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
async fn error_text(main_text: String, err: &str) -> String {
    // if we are running in dev include the actual error
    #[cfg(debug_assertions)]
    let result = main_text + ": " + err;
    // otherwise don't
    #[cfg(not(debug_assertions))]
    let result = main_text;
    result
}
async fn respond(
    request: Request<impl hyper::body::Body>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let args = Args::parse();
    let mut requested_path = request.uri().path().trim_start_matches('/');
    // Handle empty path (default to index.html)
    if requested_path.is_empty() {
        requested_path = "index.html";
    }

    // Validate path to prevent directory traversal attacks
    let p = requested_path;
    let path = Path::new(p);

    let step_dir = match std::env::current_dir() {
        Ok(dir) => dir,

        Err(e) => {
            // If we can't determine the current directory, fail safely
            let error = error_text("Internal server error".to_string(), &e.to_string()).await;
            return Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from(error)))
                .unwrap());
        }
    };
    let base_dir = step_dir.join(args.serve_path.clone());

    println!("{}", base_dir.join(path).to_str().unwrap());
    // Canonicalize the base directory to handle symlinks and relative paths
    let canonical_base_dir = match base_dir.canonicalize() {
        Ok(dir) => dir,

        Err(e) => {
            let error = error_text("Internal server error".to_string(), &e.to_string()).await;
            return Ok(Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Full::new(Bytes::from(error)))
                .unwrap());
        }
    };

    match canonical_base_dir.join(path).canonicalize() {
        Ok(canonical_path) => {
            // Ensure the canonical path is within the canonical base directory
            if !canonical_path.starts_with(&canonical_base_dir) {
                return Ok(Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Full::new(Bytes::from("Access denied")))
                    .unwrap());
            }
        }

        Err(e) => {
            // Canonicalization can fail for various reasons (file not found, permission issues, etc.)
            // give generic message for security
            let error = error_text("Accessed denied".to_string(), &e.to_string()).await;
            eprintln!("{}", error);
            eprint!(
                "Trying to access: {}",
                canonical_base_dir.join(path).to_str().unwrap()
            );
            return Ok(Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Full::new(Bytes::from(error)))
                .unwrap());
        }
    }

    // Handle file reading with proper error handling

    println!("{}", requested_path);
    let temp = args.serve_path + "/" + requested_path;
    let getpath = Path::new(&temp);
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
                    "html" | "htm" => "text/html; charset=utf-8",
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
            Ok(resp)
        }
        Err(e) => {
            eprintln!("{}", e);
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
    pretty_env_logger::init();

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
                println!("Error serving connection: {:?}", err);
            }
        });
    }
}
