//! What a tracker's connection trusts, through the real binary: a local TLS server behind a private
//! CA, made per run, answers as Jira would. No test reaches a real tracker.

mod common;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

const ISSUE: &str = r#"{"key": "ENG-52", "fields": {"summary": "Fix login", "status": {"name": "To Do"},
    "issuetype": {"name": "Task"}, "comment": {"comments": [], "total": 0}}}"#;

/// A CA called `name`: its certificate as PEM, and what signs with it. Two with one name would read
/// as one CA whose signature is wrong, rather than as one that isn't trusted.
fn ca(name: &str) -> (String, Issuer<'static, KeyPair>) {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.distinguished_name.push(DnType::CommonName, name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let key = KeyPair::generate().unwrap();
    let pem = params.self_signed(&key).unwrap().pem();
    (pem, Issuer::new(params, key))
}

/// A server on 127.0.0.1 presenting a certificate for `name` from `issuer`; its port. It answers
/// every request with `ISSUE`, for as long as the test runs.
fn serve(issuer: &Issuer<'_, KeyPair>, name: &str) -> u16 {
    let key = KeyPair::generate().unwrap();
    let params = CertificateParams::new(vec![name.to_string()]).unwrap();
    let cert = params.signed_by(&key, issuer).unwrap();
    let chain = vec![CertificateDer::from(cert.der().to_vec())];
    let key = PrivateKeyDer::try_from(key.serialize_der()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let config = config.clone();
            // A client refusing the certificate ends the handshake: nothing to answer then.
            std::thread::spawn(move || {
                let tls = StreamOwned::new(ServerConnection::new(config).unwrap(), stream);
                let _ = answer(tls);
            });
        }
    });
    port
}

fn answer(tls: StreamOwned<ServerConnection, std::net::TcpStream>) -> std::io::Result<()> {
    let mut reader = BufReader::new(tls);
    let mut line = String::new();
    while reader.read_line(&mut line)? > 2 {
        line.clear();
    }
    let mut tls = reader.into_inner();
    let length = ISSUE.len();
    write!(
        tls,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{ISSUE}"
    )?;
    tls.conn.send_close_notify();
    tls.flush()
}

/// A project on the Jira at `port`, its account's token in `JIRA_API_TOKEN`.
fn jira_project(dir: &Path, port: u16) -> PathBuf {
    let repo = common::project(dir, &[]);
    let site = format!("https://127.0.0.1:{port}");
    let saved = serde_json::json!({
        "projects": [{"name": "acme", "repo": repo, "tracker": {"provider": "jira", "site": site, "key": "ENG", "account": "jira-1"}}],
        "tickets": [],
    });
    std::fs::write(dir.join("state/taskman/tickets.json"), saved.to_string()).unwrap();
    let config = serde_json::json!({ "accounts": [
        {"id": "jira-1", "provider": "jira", "site": site, "email": "ana@example.com", "token": "env:JIRA_API_TOKEN"},
    ]});
    std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
    repo
}

fn status(dir: &Path, env: &[(&str, &Path)]) -> Output {
    let mut cmd = common::taskman(dir);
    cmd.args(["status", "ENG-52", "--project", "acme"]).env("JIRA_API_TOKEN", "t");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A project on a server behind a private CA: the test's folder, and the file with the CA's
/// certificate.
fn behind_a_private_ca(name: &str) -> (PathBuf, PathBuf) {
    let dir = common::scratch_dir(name);
    let (pem, issuer) = ca("Mock Private CA");
    let ca_file = dir.join("ca.pem");
    std::fs::write(&ca_file, pem).unwrap();
    jira_project(&dir, serve(&issuer, "127.0.0.1"));
    (dir, ca_file)
}

#[test]
fn trusts_the_ca_ssl_cert_file_names() {
    let (dir, ca_file) = behind_a_private_ca("tls-file");
    let out = status(&dir, &[("SSL_CERT_FILE", &ca_file)]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("Fix login"), "{}", stdout(&out));
}

#[test]
fn trusts_the_ca_ssl_cert_dir_holds() {
    let (dir, ca_file) = behind_a_private_ca("tls-dir");
    let folder = dir.join("certs");
    std::fs::create_dir(&folder).unwrap();
    std::fs::copy(&ca_file, folder.join("ca.pem")).unwrap();
    let out = status(&dir, &[("SSL_CERT_DIR", &folder)]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("Fix login"), "{}", stdout(&out));
}

/// The system's own store, with no variable set: a CA made a moment ago can't be in it.
#[test]
fn says_what_to_do_about_a_ca_it_doesnt_trust() {
    let (dir, _) = behind_a_private_ca("tls-untrusted");
    let out = status(&dir, &[]);
    assert!(!out.status.success(), "{}", stdout(&out));
    let said = stdout(&out);
    assert!(said.contains("certificate comes from a CA this machine doesn't trust"), "{said}");
    assert!(said.contains("set SSL_CERT_FILE"), "{said}");
    assert!(said.contains("then restart taskman"), "{said}");
}

/// A variable that's set is read in place of the store, so that's what the message points at.
#[test]
fn says_when_a_variable_is_read_instead_of_the_store() {
    let (dir, _) = behind_a_private_ca("tls-other-ca");
    let other = dir.join("other.pem");
    std::fs::write(&other, ca("Another Mock CA").0).unwrap();
    let said = stdout(&status(&dir, &[("SSL_CERT_FILE", &other)]));
    assert!(
        said.contains("SSL_CERT_FILE is set and is read instead of the system's store"),
        "{said}"
    );
    assert!(said.contains("then restart taskman"), "{said}");
}

/// A FIFO would block the reader for good, and every tracker request with it.
#[test]
fn doesnt_read_an_ssl_cert_file_that_isnt_a_file() {
    let (dir, _) = behind_a_private_ca("tls-fifo");
    let fifo = dir.join("certs.fifo");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    let said = stdout(&status(&dir, &[("SSL_CERT_FILE", &fifo)]));
    assert!(said.contains("this machine doesn't trust"), "{said}");
    assert!(said.contains("SSL_CERT_FILE isn't a regular file"), "{said}");
}

#[test]
fn says_when_ssl_cert_file_cant_be_read() {
    let (dir, _) = behind_a_private_ca("tls-missing");
    let missing = dir.join("no-such-file.pem");
    let said = stdout(&status(&dir, &[("SSL_CERT_FILE", &missing)]));
    assert!(said.contains("this machine doesn't trust"), "{said}");
    assert!(said.contains("no-such-file.pem"), "{said}");
}

/// A CA it trusts doesn't help a certificate for another name, and the message doesn't point at the
/// store.
#[test]
fn refuses_a_certificate_for_another_name() {
    let dir = common::scratch_dir("tls-name");
    let (pem, issuer) = ca("Mock Private CA");
    let ca_file = dir.join("ca.pem");
    std::fs::write(&ca_file, pem).unwrap();
    jira_project(&dir, serve(&issuer, "elsewhere.example.com"));
    let out = status(&dir, &[("SSL_CERT_FILE", &ca_file)]);
    assert!(!out.status.success(), "{}", stdout(&out));
    let said = stdout(&out);
    assert!(said.contains("certificate was refused"), "{said}");
    assert!(!said.contains("SSL_CERT_FILE"), "{said}");
}
