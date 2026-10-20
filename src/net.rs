//! What every HTTP client shares (Jira, Linear, the updater): how an agent is built, the guard that
//! keeps unit tests off the network, and asking again when a connection drops. Each client keeps
//! its own wording for errors.

use std::time::Duration;
use ureq::config::ConfigBuilder;
use ureq::http::{Request, Response};
use ureq::middleware::MiddlewareNext;
use ureq::typestate::AgentScope;
use ureq::{Body, SendBody};

/// How long a tracker's request may take, from connecting to the end of the answer.
pub const TRACKER_TIMEOUT: Duration = Duration::from_secs(20);

/// An agent's settings, each request limited to `timeout` as a whole; the caller adds its own and
/// builds it. In unit tests, the agent reaches only `replay`'s server, even through an App's
/// background fetch.
pub fn config(timeout: Duration) -> ConfigBuilder<AgentScope> {
    let config = ureq::Agent::config_builder().timeout_global(Some(timeout));
    if cfg!(test) { config.middleware(loopback_only) } else { config }
}

fn loopback_only(
    request: Request<SendBody>,
    next: MiddlewareNext,
) -> Result<Response<Body>, ureq::Error> {
    if request.uri().to_string().starts_with("http://127.0.0.1:") {
        next.handle(request)
    } else {
        Err(ureq::Error::ConnectionFailed)
    }
}

/// The connection closed under the request: a server can close one it kept open as the client
/// reuses it, and a network can drop one.
fn dropped(e: &ureq::Error) -> bool {
    use std::io::ErrorKind::*;
    let ureq::Error::Io(e) = e else { return false };
    matches!(e.kind(), UnexpectedEof | ConnectionReset | ConnectionAborted | BrokenPipe)
}

/// `request`'s result, asked once more when the connection dropped. The connection that broke
/// isn't kept, so the second try takes another. Only for a request that can safely run twice: one
/// that dropped may have reached the server.
pub fn retry_dropped<T>(
    mut request: impl FnMut() -> Result<T, ureq::Error>,
) -> Result<T, ureq::Error> {
    match request() {
        Err(e) if dropped(&e) => request(),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay;

    #[test]
    fn retries_once_when_the_connection_drops() {
        use std::io::ErrorKind::*;
        let io = |kind| ureq::Error::Io(std::io::Error::new(kind, "x"));
        // How many times retry_dropped asks when the tries fail with `errors` in turn, and whether
        // it succeeds.
        let tries = |mut errors: Vec<ureq::Error>| {
            errors.reverse();
            let mut n = 0;
            let ok = retry_dropped(|| {
                n += 1;
                errors.pop().map_or(Ok(()), Err)
            });
            (n, ok.is_ok())
        };
        assert_eq!(tries(vec![]), (1, true));
        assert_eq!(tries(vec![io(UnexpectedEof)]), (2, true));
        assert_eq!(tries(vec![io(ConnectionReset), io(BrokenPipe)]), (2, false));
        assert_eq!(tries(vec![io(ConnectionRefused)]), (1, false));
        assert_eq!(tries(vec![ureq::Error::StatusCode(404)]), (1, false));
        assert_eq!(tries(vec![ureq::Error::HostNotFound]), (1, false));
    }

    #[test]
    fn unit_tests_reach_only_the_local_server() {
        let agent: ureq::Agent = config(TRACKER_TIMEOUT).build().into();
        for url in ["https://example.atlassian.net/rest/api/3/myself", "http://localhost:1/"] {
            assert!(matches!(agent.get(url).call(), Err(ureq::Error::ConnectionFailed)), "{url}");
        }
        let server = replay::serve(vec![(200, "{}")]);
        assert_eq!(agent.get(&server.url).call().unwrap().status(), 200);
    }

    #[test]
    fn a_dropped_connection_is_asked_again() {
        let server = replay::serve(vec![(replay::DROP, "")]);
        let agent: ureq::Agent = config(TRACKER_TIMEOUT).build().into();
        let first = agent.get(&server.url).call().unwrap_err();
        assert!(dropped(&first), "{first:?}");
        let server = replay::serve(vec![(replay::DROP, ""), (200, "{}")]);
        assert_eq!(retry_dropped(|| agent.get(&server.url).call()).unwrap().status(), 200);
        assert_eq!(server.requests().len(), 2);
    }
}
