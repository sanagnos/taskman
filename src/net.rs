//! What every HTTP client shares (Jira, Linear, the updater): how an agent is built, the guard that
//! keeps unit tests off the network, the certificates a tracker's connection trusts, and asking
//! again when a connection drops. Each client keeps its own wording for errors.

use std::sync::{Arc, OnceLock};
use std::time::Duration;
use ureq::config::ConfigBuilder;
use ureq::http::{Request, Response};
use ureq::middleware::MiddlewareNext;
use ureq::tls::{Certificate, RootCerts, TlsConfig};
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

/// A tracker's agent, built at its first request. Clients are made on the UI thread, requests never
/// are, and the first agent reads the system's certificates, which takes macOS a noticeable moment.
#[derive(Clone)]
pub struct TrackerAgent {
    timeout: Duration,
    http_status_as_error: bool,
    agent: Arc<OnceLock<ureq::Agent>>,
}

impl TrackerAgent {
    /// An agent limiting each request to `timeout`; `http_status_as_error` as in ureq's config.
    pub fn new(timeout: Duration, http_status_as_error: bool) -> Self {
        Self { timeout, http_status_as_error, agent: Arc::default() }
    }

    pub fn ready(&self) -> &ureq::Agent {
        self.agent.get_or_init(|| {
            let config = config(self.timeout).http_status_as_error(self.http_status_as_error);
            // Unit tests stay on `replay`'s plain HTTP, and away from this machine's certificates.
            if cfg!(test) {
                return config.build().into();
            }
            let tls = TlsConfig::builder().root_certs(trusted().roots.clone()).build();
            config.tls_config(tls).build().into()
        })
    }
}

/// What a tracker's connection trusts, read once for all its agents.
struct Trusted {
    roots: RootCerts,
    /// Why some of the system's certificates couldn't be read, such as an `SSL_CERT_FILE` that
    /// doesn't exist: the first thing to know when a certificate is then refused.
    unread: Option<String>,
}

fn trusted() -> &'static Trusted {
    static TRUSTED: OnceLock<Trusted> = OnceLock::new();
    TRUSTED.get_or_init(|| {
        let system = rustls_native_certs::load_native_certs();
        Trusted {
            roots: with_bundled(system.certs.iter().map(|c| c.as_ref())).into(),
            unread: system.errors.first().map(ToString::to_string),
        }
    })
}

/// The system's certificates and the bundled ones. All of them, rather than the bundled ones only
/// when the system has none: whatever connected before the system's were read still does, on a
/// machine with an old store or an `SSL_CERT_FILE` that names only a private CA.
fn with_bundled<'a>(system: impl Iterator<Item = &'a [u8]>) -> Vec<Certificate<'static>> {
    let bundled = webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().map(|c| c.as_ref());
    system.chain(bundled).map(|der| Certificate::from_der(der).to_owned()).collect()
}

/// Why the server's certificate was refused, in rustls's words, when that's what `e` is. ureq hands
/// it over as an IO error, so it's known by those words.
pub fn certificate_refused(e: &ureq::Error) -> Option<String> {
    let ureq::Error::Io(io) = e else { return None };
    io.to_string().strip_prefix("invalid peer certificate: ").map(String::from)
}

/// What to tell the user when `host`'s certificate was refused because of `why`.
pub fn certificate_failure(host: &str, why: &str) -> String {
    let unread = if cfg!(test) { None } else { trusted().unread.as_deref() };
    certificate_words(host, why, unread)
}

fn certificate_words(host: &str, why: &str, unread: Option<&str>) -> String {
    let said = if why == "UnknownIssuer" {
        format!(
            "{host}'s certificate comes from a CA this machine doesn't trust: add the CA to the system's certificate store, or set SSL_CERT_FILE to a file holding it"
        )
    } else {
        format!("{host}'s certificate was refused: {why}")
    };
    match unread {
        Some(unread) => format!("{said} ({unread})"),
        None => said,
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
    fn trusts_the_systems_certificates_and_the_bundled_ones() {
        let bundled = webpki_root_certs::TLS_SERVER_ROOT_CERTS.len();
        assert!(bundled > 50, "{bundled}");
        assert_eq!(with_bundled(std::iter::empty()).len(), bundled);
        let private: &[u8] = b"a private CA";
        let both = with_bundled(std::iter::once(private));
        assert_eq!(both.len(), bundled + 1);
        assert_eq!(both[0].der(), private);
    }

    /// The updater's agent replaces taskman's own binary: nothing in the environment widens what it
    /// trusts.
    #[test]
    fn the_plain_config_stays_on_the_bundled_certificates() {
        let config = config(TRACKER_TIMEOUT).build();
        assert!(matches!(config.tls_config().root_certs(), RootCerts::WebPki));
    }

    #[test]
    fn knows_a_refused_certificate_by_rustlss_words() {
        use std::io::{Error, ErrorKind};
        let io = |words: &str| ureq::Error::Io(Error::new(ErrorKind::InvalidData, words));
        let refused = certificate_refused(&io("invalid peer certificate: UnknownIssuer"));
        assert_eq!(refused.as_deref(), Some("UnknownIssuer"));
        assert_eq!(certificate_refused(&io("received corrupt message")), None);
        assert_eq!(certificate_refused(&ureq::Error::HostNotFound), None);
    }

    #[test]
    fn says_what_to_do_about_a_refused_certificate() {
        let unknown = certificate_words("jira.example.com", "UnknownIssuer", None);
        assert_eq!(
            unknown,
            "jira.example.com's certificate comes from a CA this machine doesn't trust: add the CA to the system's certificate store, or set SSL_CERT_FILE to a file holding it"
        );
        let unread = certificate_words("jira.example.com", "UnknownIssuer", Some("no such file"));
        assert_eq!(unread, format!("{unknown} (no such file)"));
        // The store has nothing to do with these, so it isn't what the message points at.
        let expired = certificate_words("jira.example.com", "certificate expired", None);
        assert_eq!(expired, "jira.example.com's certificate was refused: certificate expired");
    }

    #[test]
    fn unit_tests_reach_only_the_local_server() {
        let plain: ureq::Agent = config(TRACKER_TIMEOUT).build().into();
        let tracker = TrackerAgent::new(TRACKER_TIMEOUT, true);
        for agent in [&plain, tracker.ready()] {
            for url in ["https://example.atlassian.net/rest/api/3/myself", "http://localhost:1/"] {
                let got = agent.get(url).call();
                assert!(matches!(got, Err(ureq::Error::ConnectionFailed)), "{url}");
            }
            let server = replay::serve(vec![(200, "{}")]);
            assert_eq!(agent.get(&server.url).call().unwrap().status(), 200);
        }
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
