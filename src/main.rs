use bytes::Bytes;
use http_body_util::Full;
use hyper::{Request, Response, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{convert::Infallible, net::SocketAddr};
use tokio::{
    fs::File,
    io::{self, AsyncReadExt, BufReader},
    net::TcpListener,
};
async fn get_file(path_str: String) -> io::Result<String> {
    let file = File::open(path_str).await?;
    let mut reader = BufReader::new(file);
    let mut buffer = String::new();
    let _ = reader.read_to_string(&mut buffer).await;
    Ok(buffer)
}
async fn respond(
    request: Request<impl hyper::body::Body>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    Ok(Response::new(Full::new(
        get_file(request.uri().port().unwrap().to_string())
            .await
            .unwrap()
            .into_bytes()
            .into(),
    )))
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
