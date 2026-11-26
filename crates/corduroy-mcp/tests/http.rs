//! A server over HTTP whose bearer token is a secret: what is sent is the secret's value,
//! revealed when the server is connected to. No network beyond the loopback interface.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use corduroy_core::{App, FiberState, SECRETS, Secret, Secrets, TOOLS, Tools};
use corduroy_mcp::{Transport, mcp_server};

/// The `Authorization` header among a request's header lines, or nothing.
fn authorization_of(stream: &std::net::TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let mut header = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 && line != "\r\n" {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("authorization")
        {
            value.trim().clone_into(&mut header);
        }
        line.clear();
    }
    header
}

/// An endpoint that is no MCP server: it keeps the `Authorization` header of the first
/// request and answers that there is nothing here.
fn endpoint() -> std::io::Result<(String, mpsc::Receiver<String>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    let (seen, authorization) = mpsc::channel();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let header = authorization_of(&stream);
            drop(seen.send(header));
            let _written = (&stream).write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    Ok((url, authorization))
}

/// An app, and its secrets.
type Host = (App, Arc<Secrets>);

/// An app with the registries an MCP plugin over HTTP needs.
fn app() -> Result<Host, Box<dyn std::error::Error>> {
    let mut app = App::new();
    let secrets = Arc::new(Secrets::new());
    app.provide(TOOLS, Arc::new(Tools::new()))?;
    app.provide(SECRETS, Arc::clone(&secrets))?;
    Ok((app, secrets))
}

#[test]
fn a_servers_bearer_token_is_the_secret_it_names() {
    let (url, authorization) = endpoint().unwrap();
    let (mut app, secrets) = app().unwrap();
    let revealed = Arc::new(Mutex::new(0_u32));
    let count = Arc::clone(&revealed);
    let token: Arc<dyn Secret> = Arc::new(move || {
        *count.lock().map_err(|_| "poisoned".to_owned())? += 1;
        Ok("s3cret-token".to_owned())
    });
    secrets.insert("remote-token", token).unwrap();
    let transport = Transport::Http {
        url,
        bearer_token: None,
    };

    let id = app.plugin(
        mcp_server("remote", transport)
            .with_secret("remote-token")
            .with_timeout(Duration::from_secs(5)),
    );

    let sent = authorization.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(sent, "Bearer s3cret-token");
    assert_eq!(*revealed.lock().unwrap(), 1);
    // What answered is no MCP server, so the plugin failed, without the token in the reason.
    let Some(FiberState::Failed(reason)) = app.state(id) else {
        panic!("the plugin should have failed");
    };
    assert!(reason.contains("MCP server `remote`"), "{reason}");
    assert!(!reason.contains("s3cret-token"), "{reason}");
}

#[test]
fn a_secret_that_cannot_be_revealed_fails_the_server_and_says_which() {
    let (mut app, _secrets) = app().unwrap();
    let transport = Transport::Http {
        url: "http://127.0.0.1:9/mcp".to_owned(),
        bearer_token: None,
    };
    let id = app.plugin(mcp_server("remote", transport).with_secret("missing"));
    let Some(FiberState::Failed(reason)) = app.state(id) else {
        panic!("the plugin should have failed");
    };
    assert_eq!(
        reason,
        "MCP server `remote`: secret `missing`: no secret is registered as `missing`"
    );
}
