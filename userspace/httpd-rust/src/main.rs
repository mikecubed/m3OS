use std::env;
use std::io::Write;
use std::net::TcpListener;

const HTML_BODY: &str = "<html><body><h1>Hello from Rust on m3OS!</h1></body></html>";

fn main() {
    let port = env::args()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(8080);

    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("httpd-rust: failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    println!("httpd-rust: listening on {addr}");

    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                     Content-Type: text/html\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\
                     \r\n\
                     {}",
                    HTML_BODY.len(),
                    HTML_BODY,
                );
                let _ = stream.write_all(response.as_bytes());
            }
            Err(e) => {
                eprintln!("httpd-rust: accept error: {e}");
            }
        }
    }
}
