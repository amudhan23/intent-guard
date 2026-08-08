use bytes::Bytes;
use clap::Parser;
use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, body::Body};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

mod scanner;

#[derive(Parser)]
struct RequestParsed {
    method: String,
    path: String,
    body: Option<String>,
}

#[derive(Deserialize)]
struct Rules {
    suspicious_body_patterns: Vec<String>,
    risky_privilege_paths: Vec<String>,
}

async fn echo(
    req: Request<hyper::body::Incoming>,
    rules: Arc<Rules>,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, hyper::Error> {
    match (req.method(), req.uri().path()) {
        (&Method::PUT, path) => {
            if scanner::scan_privilege(req.method().as_str(), path, &rules.risky_privilege_paths) {
                let mut resp = Response::new(full("Privileged error"));
                *resp.status_mut() = hyper::StatusCode::FORBIDDEN;
                return Ok(resp);
            }
            match forward_to_backend(req, "httpbin.org", 80).await {
                Ok(resp) => Ok(resp),
                Err(e) => {
                    eprintln!("Forwarding failed: {}", e);
                    let mut resp = Response::new(full("Backend unavailable"));
                    *resp.status_mut() = hyper::StatusCode::BAD_GATEWAY;
                    Ok(resp)
                }
            }
        }

        (&Method::GET, path) => {
            let method = req.method().clone();
            let uri = req.uri().clone();
            let new_req = Request::builder()
                .method(method)
                .uri(uri)
                .body(Empty::<Bytes>::new())
                .unwrap();
            match forward_to_backend(new_req, "httpbin.org", 80).await {
                Ok(resp) => Ok(resp),
                Err(e) => {
                    eprintln!("Forwarding failed: {}", e);
                    let mut resp = Response::new(full("Backend unavailable"));
                    *resp.status_mut() = hyper::StatusCode::BAD_GATEWAY;
                    Ok(resp)
                }
            }
        }

        (&Method::POST, path) => {
            let method = req.method().clone();
            let uri = req.uri().clone();

            let max = req.body().size_hint().upper().unwrap_or(u64::MAX);
            if max > 1024 * 64 {
                let mut resp = Response::new(full("Body too big"));
                *resp.status_mut() = hyper::StatusCode::PAYLOAD_TOO_LARGE;
                return Ok(resp);
            }

            let whole_body = req.collect().await?.to_bytes();
            let body_str = String::from_utf8_lossy(&whole_body);

            if let Some(_) = scanner::scan_body(&body_str, &rules.suspicious_body_patterns) {
                let mut resp = Response::new(full("Privileged error"));
                *resp.status_mut() = hyper::StatusCode::FORBIDDEN;
                return Ok(resp);
            }

            let new_req = Request::builder()
                .method(method)
                .uri(uri)
                .body(Full::new(whole_body)) // reuse the bytes you already collected
                .unwrap();

            match forward_to_backend(new_req, "httpbin.org", 80).await {
                Ok(resp) => Ok(resp),
                Err(e) => {
                    eprintln!("Forwarding failed: {}", e);
                    let mut resp = Response::new(full("Backend unavailable"));
                    *resp.status_mut() = hyper::StatusCode::BAD_GATEWAY;
                    Ok(resp)
                }
            }
        }

        _ => {
            let mut not_found = Response::new(empty());
            *not_found.status_mut() = StatusCode::NOT_FOUND;
            Ok(not_found)
        }
    }
}

fn empty() -> BoxBody<Bytes, hyper::Error> {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

fn full<T: Into<Bytes>>(chunk: T) -> BoxBody<Bytes, hyper::Error> {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

async fn forward_to_backend<B>(
    req: Request<B>,
    target_host: &str,
    target_port: u16,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>, Box<dyn std::error::Error + Send + Sync>>
where
    B: hyper::body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let address = format!("{}:{}", target_host, target_port);
    let stream = TcpStream::connect(&address).await?; // io::Error, auto-boxed
    let io = TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?; // hyper::Error, auto-boxed

    tokio::task::spawn(async move {
        if let Err(err) = conn.await {
            eprintln!("Connection to backend failed: {:?}", err);
        }
    });

    let (mut parts, body) = req.into_parts();
    parts
        .headers
        .insert(hyper::header::HOST, target_host.parse()?); // also auto-boxed
    let outbound_req = Request::from_parts(parts, body);

    let res = sender.send_request(outbound_req).await?; // hyper::Error, auto-boxed

    let (parts, body) = res.into_parts();
    let boxed_body = body.boxed();
    Ok(Response::from_parts(parts, boxed_body))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let listener = TcpListener::bind(addr).await?;
    println!("Listening on http://{}", addr);

    let rules_content = match std::fs::read_to_string("rules.yaml") {
        Ok(file_content) => file_content,
        Err(e) => {
            eprintln!("Error reading rules file : {}", e);
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "rules.yaml file not found",
            )) as Box<dyn std::error::Error + Send + Sync>);
        }
    };

    let rules: Rules = match serde_yaml::from_str(&rules_content) {
        Ok(r) => r,
        Err(e) => {
            eprint!("Error parsing yaml : {}", e);
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "rules.yaml file not found",
            )) as Box<dyn std::error::Error + Send + Sync>);
        }
    };

    let rules = Arc::new(rules);

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        let rules = Arc::clone(&rules);

        let service = service_fn(move |req| {
            let rules = Arc::clone(&rules);
            async move { echo(req, rules).await }
        });

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                println!("Error serving connection: {:?}", err);
            }
        });
    }
}
