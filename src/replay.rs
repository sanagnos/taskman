//! A local HTTP server that answers with recorded responses, for the tracker clients' tests, so no
//! test reaches a real tracker. The recordings are hand-trimmed mock data in tests/fixtures.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc;

/// A request the server answered.
pub struct Request {
    /// Like `GET /rest/api/3/issue/ENG-52?fields=... HTTP/1.1`.
    pub line: String,
    headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_default()
    }
}

pub struct Server {
    /// Where it listens, like `http://127.0.0.1:41234`.
    pub url: String,
    got: mpsc::Receiver<Request>,
}

impl Server {
    /// The requests answered so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.got.try_iter().collect()
    }
}

/// The status that answers nothing: the connection closes once the request is read, as a dropped
/// one does.
pub const DROP: u16 = 0;

/// The status that never answers: the connection stays open and silent for a minute, as a
/// tracker's does when it's overloaded or a proxy stalls.
pub const HANG: u16 = 1;

/// Answers one connection per response, in order, each with that status and JSON body (`DROP` or
/// `HANG` for none). A request is recorded before it's answered, so once the client has its answer,
/// `requests` has it.
pub fn serve(responses: Vec<(u16, &'static str)>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, got) = mpsc::channel();
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((stream, _)) = listener.accept() else { return };
            let mut reader = BufReader::new(stream);
            let Some(request) = read_request(&mut reader) else { return };
            let _ = tx.send(request);
            if status == DROP {
                continue;
            }
            let mut stream = reader.into_inner();
            if status == HANG {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(60));
                    drop(stream);
                });
                continue;
            }
            let _ = write!(
                stream,
                "HTTP/1.1 {status} Recorded\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    Server { url, got }
}

/// A URL where nothing listens, so a request to it is refused.
pub fn closed() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

fn read_request(reader: &mut impl BufRead) -> Option<Request> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let request = Request { line: line.trim_end().to_string(), headers, body: String::new() };
    let len = request.header("content-length").and_then(|n| n.parse().ok()).unwrap_or(0);
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    Some(Request { body: String::from_utf8_lossy(&body).into_owned(), ..request })
}
