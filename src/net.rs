//! What every HTTP client shares (Jira, Linear, the updater): how an agent is built, the guard that
//! keeps unit tests off the network, the certificates a tracker's connection trusts, and asking
//! again when a connection drops. Each client keeps its own wording for errors.

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use ureq::config::ConfigBuilder;
use ureq::http::{Request, Response};
use ureq::middleware::MiddlewareNext;
use ureq::tls::{Certificate, RootCerts, TlsConfig};
use ureq::typestate::AgentScope;
use ureq::{Body, SendBody};

const CERT_FILE: &str = "SSL_CERT_FILE";
const CERT_DIR: &str = "SSL_CERT_DIR";

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
    agent: Arc<OnceLock<ureq::Agent>>,
}

impl TrackerAgent {
    /// An agent limiting each request to `timeout` and answering whatever the status: a tracker
    /// says in the body why it refused.
    pub fn new(timeout: Duration) -> Self {
        Self { timeout, agent: Arc::default() }
    }

    /// How long each request may take.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn ready(&self) -> &ureq::Agent {
        self.agent.get_or_init(|| {
            let config = config(self.timeout).http_status_as_error(false);
            // Unit tests stay on `replay`'s plain HTTP, and away from this machine's certificates.
            if cfg!(test) {
                return config.build().into();
            }
            let tls = TlsConfig::builder().root_certs(trusted().roots.clone()).build();
            config.tls_config(tls).build().into()
        })
    }
}

/// How long reading the system's certificates may take: every tracker request waits for it, before
/// any of their own time limits starts.
const READ_LIMIT: Duration = Duration::from_secs(5);

/// What a tracker's connection trusts, read once for all its agents.
struct Trusted {
    roots: RootCerts,
    /// Why some of the certificates couldn't be read, such as an `SSL_CERT_FILE` that doesn't
    /// exist: the first thing to know when a certificate is then refused.
    unread: Option<String>,
}

fn trusted() -> &'static Trusted {
    static TRUSTED: OnceLock<Trusted> = OnceLock::new();
    TRUSTED.get_or_init(|| trust(set_variables().is_some(), read_within(READ_LIMIT, read_system)))
}

/// What's trusted, given what was read and whether a variable named it. Variables that yield
/// nothing between them trust nothing, and say why, rather than falling back on a list their user
/// may have set them to get away from.
fn trust(variable: bool, system: System) -> Trusted {
    let none = system.certs.is_empty();
    let unread = match (system.unread, variable && none) {
        (Some(unread), true) => Some(format!("{unread}, so no certificates are trusted")),
        (None, true) => Some("it holds no certificates, so none are trusted".to_string()),
        (Some(unread), false) if none => {
            Some(format!("{unread}, so only the bundled list is trusted"))
        }
        (unread, _) => unread,
    };
    Trusted { roots: roots_for(system.certs, variable), unread }
}

/// The system's certificates, as far as they could be read.
struct System {
    certs: Vec<Certificate<'static>>,
    unread: Option<String>,
}

/// `read`'s result, or none of the certificates when it takes longer than `limit`, as on a stale
/// network path, or dies. Its thread is left to finish or hang by itself.
fn read_within(limit: Duration, read: impl FnOnce() -> System + Send + 'static) -> System {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read());
    });
    rx.recv_timeout(limit).unwrap_or_else(|e| System {
        certs: Vec::new(),
        unread: Some(match e {
            RecvTimeoutError::Timeout => {
                format!("reading the certificates took over {}s", limit.as_secs())
            }
            RecvTimeoutError::Disconnected => "reading the certificates failed".to_string(),
        }),
    })
}

/// A file that isn't a regular one is never handed to the reader: a FIFO blocks it for good, and
/// `/dev/zero` has it read for good.
fn read_system() -> System {
    if let Some(file) = std::env::var_os(CERT_FILE)
        && std::fs::metadata(&file).is_ok_and(|m| !m.is_file())
    {
        let file = std::path::Path::new(&file).display();
        let unread = format!("{CERT_FILE} isn't a regular file: {file}");
        return System { certs: Vec::new(), unread: Some(unread) };
    }
    let system = rustls_native_certs::load_native_certs();
    System {
        certs: system.certs.iter().map(|c| Certificate::from_der(c).to_owned()).collect(),
        unread: system.errors.first().map(ToString::to_string),
    }
}

/// The roots for what was read. A variable's certificates replace everything, as someone may have
/// set one to narrow what's trusted. Without one, the system's and the bundled ones, so a machine
/// with an old store still reaches public sites; and with no system certificates, ureq's own list,
/// as only that keeps a root's name limit.
fn roots_for(system: Vec<Certificate<'static>>, variable: bool) -> RootCerts {
    match (variable, system.is_empty()) {
        (true, _) => system.into(),
        (false, true) => RootCerts::WebPki,
        (false, false) => with_bundled(system).into(),
    }
}

/// `system` and the bundled roots, but for those limited to some names: the list ureq takes holds
/// only whole certificates, which don't carry the limit, so a root limited to one country's sites
/// would be trusted for every site. A system store that lists such a root, as most do, has it
/// trusted without the limit all the same, as by other tools that read the store.
fn with_bundled(system: Vec<Certificate<'static>>) -> Vec<Certificate<'static>> {
    let limited: Vec<&[u8]> = webpki_roots::TLS_SERVER_ROOTS
        .iter()
        .filter(|root| root.name_constraints.is_some())
        .map(|root| root.subject_public_key_info.as_ref())
        .collect();
    let bundled = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|c| c.as_ref())
        .filter(|der| !limited.iter().any(|key| holds(der, key)))
        .map(|der| Certificate::from_der(der).to_owned());
    system.into_iter().chain(bundled).collect()
}

/// Whether the certificate `der` holds the public key `key`.
fn holds(der: &[u8], key: &[u8]) -> bool {
    der.windows(key.len()).any(|part| part == key)
}

/// Why the server's certificate was refused, in rustls's words, when that's what `e` is. ureq hands
/// it over as an IO error, so it's known by those words.
pub fn certificate_refused(e: &ureq::Error) -> Option<String> {
    let ureq::Error::Io(io) = e else { return None };
    io.to_string().strip_prefix("invalid peer certificate: ").map(String::from)
}

/// What to tell the user when `host`'s certificate was refused because of `why`.
pub fn certificate_failure(host: &str, why: &str) -> String {
    if cfg!(test) {
        return certificate_words(host, why, None, None);
    }
    certificate_words(host, why, set_variables(), trusted().unread.as_deref())
}

fn set_variables() -> Option<&'static str> {
    variables_set(
        std::env::var_os(CERT_FILE).is_some(),
        std::env::var_os(CERT_DIR).is_some_and(|dir| !dir.is_empty()),
    )
}

/// What to do given the variables read in place of the system's store, as `rustls-native-certs`
/// has it: a file that's set, even empty, or a folder that isn't empty.
fn variables_set(file: bool, dir: bool) -> Option<&'static str> {
    match (file, dir) {
        (true, true) => Some(
            "SSL_CERT_FILE and SSL_CERT_DIR are set and are read instead of the system's store: add the CA there, or unset them",
        ),
        (true, false) => Some(
            "SSL_CERT_FILE is set and is read instead of the system's store: add the CA there, or unset it",
        ),
        (false, true) => Some(
            "SSL_CERT_DIR is set and is read instead of the system's store: add the CA there, or unset it",
        ),
        (false, false) => None,
    }
}

fn certificate_words(host: &str, why: &str, set: Option<&str>, unread: Option<&str>) -> String {
    if why != "UnknownIssuer" {
        return format!("{host}'s certificate was refused: {why}");
    }
    let untrusted = format!("{host}'s certificate comes from a CA this machine doesn't trust");
    let said = match set {
        Some(set) => format!("{untrusted}. {set}, then restart taskman"),
        None => format!(
            "{untrusted}: add the CA to the system's certificate store, or set SSL_CERT_FILE to a file holding it, then restart taskman"
        ),
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

    /// Whole certificates can't carry a limit to some names, and only ureq's own list keeps one.
    #[test]
    fn a_bundled_roots_name_limit_survives() {
        assert!(matches!(roots_for(Vec::new(), false), RootCerts::WebPki));
        let private = Certificate::from_der(b"a private CA").to_owned();
        let RootCerts::Specific(merged) = roots_for(vec![private], false) else { panic!() };
        assert_eq!(merged.len(), with_bundled(Vec::new()).len() + 1);

        let limited: Vec<_> = webpki_roots::TLS_SERVER_ROOTS
            .iter()
            .filter(|root| root.name_constraints.is_some())
            .collect();
        let bundled = webpki_root_certs::TLS_SERVER_ROOT_CERTS;
        for root in &limited {
            let key = root.subject_public_key_info.as_ref();
            assert_eq!(bundled.iter().filter(|c| holds(c.as_ref(), key)).count(), 1);
            assert!(!with_bundled(Vec::new()).iter().any(|c| holds(c.der(), key)));
        }
        assert_eq!(with_bundled(Vec::new()).len(), bundled.len() - limited.len());
    }

    /// The two crates must be at one version, or a root limited in one could be whole in the other.
    #[test]
    fn the_two_bundled_lists_are_the_same_roots() {
        let bundled = webpki_root_certs::TLS_SERVER_ROOT_CERTS;
        assert_eq!(webpki_roots::TLS_SERVER_ROOTS.len(), bundled.len());
        for root in webpki_roots::TLS_SERVER_ROOTS {
            let key = root.subject_public_key_info.as_ref();
            assert!(bundled.iter().any(|c| holds(c.as_ref(), key)));
        }
    }

    #[test]
    fn trusts_the_systems_certificates_and_the_bundled_ones() {
        let private: &[u8] = b"a private CA";
        let both = with_bundled(vec![Certificate::from_der(private).to_owned()]);
        assert_eq!(both.len(), with_bundled(Vec::new()).len() + 1);
        assert_eq!(both[0].der(), private);
    }

    /// A variable replaces what's trusted: its certificates alone, with no bundled list, and no
    /// fallback when it yields none.
    #[test]
    fn a_variable_replaces_what_is_trusted() {
        let private = Certificate::from_der(b"a private CA").to_owned();
        let RootCerts::Specific(only) = roots_for(vec![private.clone()], true) else { panic!() };
        assert_eq!(only.len(), 1);
        let RootCerts::Specific(none) = roots_for(Vec::new(), true) else { panic!() };
        assert!(none.is_empty());

        let read = |certs: Vec<Certificate<'static>>, unread: Option<&str>| System {
            certs,
            unread: unread.map(String::from),
        };
        let unread = |variable, system| trust(variable, system).unread;
        assert_eq!(
            unread(true, read(Vec::new(), None)).unwrap(),
            "it holds no certificates, so none are trusted"
        );
        assert_eq!(
            unread(true, read(Vec::new(), Some("no such file"))).unwrap(),
            "no such file, so no certificates are trusted"
        );
        assert_eq!(
            unread(false, read(Vec::new(), Some("too slow"))).unwrap(),
            "too slow, so only the bundled list is trusted"
        );
        assert_eq!(unread(false, read(Vec::new(), None)), None);
        assert_eq!(unread(true, read(vec![private.clone()], None)), None);
        assert_eq!(
            unread(true, read(vec![private], Some("one bad file"))).unwrap(),
            "one bad file"
        );
    }

    #[test]
    fn gives_up_on_a_store_that_takes_too_long() {
        let private = Certificate::from_der(b"a private CA").to_owned();
        let read = read_within(Duration::from_secs(30), move || System {
            certs: vec![private],
            unread: None,
        });
        assert_eq!((read.certs.len(), read.unread), (1, None));

        let stalled = read_within(Duration::from_millis(10), || {
            std::thread::sleep(Duration::from_secs(5));
            System { certs: Vec::new(), unread: None }
        });
        assert!(stalled.certs.is_empty());
        assert_eq!(stalled.unread.unwrap(), "reading the certificates took over 0s");

        let died = read_within(Duration::from_secs(30), || panic!("a reader that dies"));
        assert_eq!(died.unread.unwrap(), "reading the certificates failed");
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
        let host = "jira.example.com";
        let unknown = certificate_words(host, "UnknownIssuer", None, None);
        assert_eq!(
            unknown,
            "jira.example.com's certificate comes from a CA this machine doesn't trust: add the CA to the system's certificate store, or set SSL_CERT_FILE to a file holding it, then restart taskman"
        );
        let unread = certificate_words(host, "UnknownIssuer", None, Some("no such file"));
        assert_eq!(unread, format!("{unknown} (no such file)"));
        // The store has nothing to do with these, so neither it nor what couldn't be read is said.
        let expired = certificate_words(host, "certificate expired", None, Some("no such file"));
        assert_eq!(expired, "jira.example.com's certificate was refused: certificate expired");
    }

    /// With a variable set the store isn't read, so adding the CA to it wouldn't help.
    #[test]
    fn says_which_variable_is_read_instead_of_the_store() {
        let said = |file, dir| {
            certificate_words("jira.example.com", "UnknownIssuer", variables_set(file, dir), None)
        };
        let start = "jira.example.com's certificate comes from a CA this machine doesn't trust. ";
        let store = "read instead of the system's store: add the CA there, or unset";
        let end = "then restart taskman";
        assert_eq!(
            said(true, false),
            format!("{start}SSL_CERT_FILE is set and is {store} it, {end}")
        );
        assert_eq!(
            said(false, true),
            format!("{start}SSL_CERT_DIR is set and is {store} it, {end}")
        );
        assert_eq!(
            said(true, true),
            format!("{start}SSL_CERT_FILE and SSL_CERT_DIR are set and are {store} them, {end}")
        );
    }

    #[test]
    fn unit_tests_reach_only_the_local_server() {
        let plain: ureq::Agent = config(TRACKER_TIMEOUT).build().into();
        let tracker = TrackerAgent::new(TRACKER_TIMEOUT);
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
