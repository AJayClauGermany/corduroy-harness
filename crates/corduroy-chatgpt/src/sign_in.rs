//! Signing in through the browser, and out again.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use crate::id_token::Identity;
use crate::id_token::{self, Expected};
use crate::store::Record;
use crate::token::{Exchange, Tokens};
use crate::web::{decode, query};
use crate::{APP_NAME, ChatGpt, RESOURCE, SCOPE, now, random, token};

/// The client ID a first sign-in asks with: the service then issues one of the person's own.
const REGISTRATION: &str = "dynamic_agent_client";

/// The path the browser comes back to. It may never change: the service compares it.
const CALLBACK: &str = "/auth/callback";

/// The port asked for first. Another is taken if it is in use.
const PORT: u16 = 1455;

/// How long a sign-in waits for the browser.
const WAIT: Duration = Duration::from_secs(300);

/// The page the browser shows once it has come back.
const DONE: &str = "<!doctype html><meta charset=utf-8><title>Corduroy</title>\
                    <p>Corduroy has what it needs. You can close this tab.</p>";

/// What a sign-in tells the person, and how it sends them to the browser.
pub struct Front<'a> {
    /// Open a URL in the person's browser. A failure is no error: the URL is said too.
    pub open: &'a dyn Fn(&str),
    /// Say a line to the person.
    pub say: &'a mut dyn FnMut(&str),
}

impl std::fmt::Debug for Front<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Front").finish_non_exhaustive()
    }
}

/// One attempt: what was sent, to compare with what comes back.
struct Attempt {
    state: String,
    nonce: String,
    verifier: String,
    redirect: String,
}

impl ChatGpt {
    /// Sign in through the browser, waiting up to five minutes for the person, and keep the
    /// tokens. A first sign-in registers this tool for the person; a later one uses the
    /// client that registration issued, and must be the same account.
    ///
    /// # Errors
    ///
    /// If the person declines, nothing comes back in time, what comes back does not belong
    /// to this attempt, the ID token does not check out, or the tokens cannot be kept.
    pub fn sign_in(&self, front: &mut Front<'_>) -> Result<Record, String> {
        self.sign_in_within(front, WAIT)
    }

    /// [`sign_in`](Self::sign_in), waiting `wait` for the browser.
    ///
    /// # Errors
    ///
    /// As [`sign_in`](Self::sign_in).
    pub fn sign_in_within(&self, front: &mut Front<'_>, wait: Duration) -> Result<Record, String> {
        let before = self.store.load()?;
        let host = self.store.host_id()?;
        let listener = listen()?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("cannot listen for the browser: {error}"))?
            .port();
        let attempt = Attempt {
            state: random::text()?,
            nonce: random::text()?,
            verifier: random::text()?,
            redirect: format!("http://127.0.0.1:{port}{CALLBACK}"),
        };
        let url = self.authorize_url(&attempt, &host, before.as_ref());
        (front.say)("Continue with ChatGPT: opening your browser. If it does not open, go to:");
        (front.say)(&format!("  {url}"));
        (front.open)(&url);

        let back = callback(&listener, wait)?;
        let (code, client_id) = self.accepted(&back, &attempt, before.as_ref())?;
        let exchange = Exchange {
            client_id: &client_id,
            code: &code,
            verifier: &attempt.verifier,
            redirect: &attempt.redirect,
        };
        let tokens = token::exchange(&self.web, &self.endpoints, &exchange)?;
        let id_token = tokens
            .id_token
            .clone()
            .ok_or("the sign-in gave no ID token")?;
        let expected = Expected {
            client_id: &client_id,
            nonce: &attempt.nonce,
            now: now(),
        };
        let who = id_token::verify(&self.web, &self.endpoints, &id_token, &expected)?;
        if before
            .as_ref()
            .is_some_and(|before| before.subject != who.subject)
        {
            return Err(
                "that is another account than the one signed in: sign out first, with \
                 `corduroy logout`"
                    .to_owned(),
            );
        }
        let record = record_of(tokens, who, client_id, host)?;
        self.store.save(&record)?;
        Ok(record)
    }

    /// Where the browser is sent for `attempt`.
    fn authorize_url(&self, attempt: &Attempt, host: &str, before: Option<&Record>) -> String {
        let challenge = random::challenge(&attempt.verifier);
        let mut asked = vec![
            (
                "client_id",
                before.map_or(REGISTRATION, |before| &before.client_id),
            ),
            ("ext_agent_host_id", host),
            ("response_type", "code"),
            ("redirect_uri", &attempt.redirect),
            ("scope", SCOPE),
            ("resource", RESOURCE),
            ("state", &attempt.state),
            ("nonce", &attempt.nonce),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge),
        ];
        match before {
            // The name is for a registration only; the hint says which account comes back.
            Some(before) => asked.push(("id_token_hint", &before.id_token)),
            None => asked.push(("agent_name_hint", APP_NAME)),
        }
        format!("{}?{}", self.endpoints.authorize, query(&asked))
    }

    /// The code and the client ID of a callback that belongs to `attempt`.
    fn accepted(
        &self,
        back: &BTreeMap<String, String>,
        attempt: &Attempt,
        before: Option<&Record>,
    ) -> Result<(String, String), String> {
        let got = |name: &str| back.get(name).map(String::as_str);
        if got("state") != Some(&attempt.state) {
            return Err("what came back is not the answer to this sign-in".to_owned());
        }
        if let Some(error) = got("error") {
            return Err(if error == "access_denied" {
                "the sign-in was declined".to_owned()
            } else {
                format!("the sign-in was refused: {error}")
            });
        }
        if got("iss").is_some_and(|issuer| issuer != self.endpoints.issuer) {
            return Err("the answer is from another issuer".to_owned());
        }
        let code = got("code").ok_or("the sign-in came back with no code")?;
        let client_id = match (before, got("client_id")) {
            (Some(before), None) => before.client_id.clone(),
            (Some(before), Some(issued)) if issued == before.client_id => issued.to_owned(),
            (Some(_), Some(_)) => return Err("the answer names another client".to_owned()),
            (None, Some(issued)) if issued != REGISTRATION => issued.to_owned(),
            (None, _) => {
                return Err("no client was issued: the registration did not complete".to_owned());
            }
        };
        Ok((code.to_owned(), client_id))
    }

    /// Sign out: revoke the refresh token, then forget the tokens. Returns whether anyone
    /// was signed in. The host's ID and the registration with the service stay.
    ///
    /// # Errors
    ///
    /// If the token cannot be revoked (the tokens are then kept, to try again), or the file
    /// cannot be removed.
    pub fn sign_out(&self) -> Result<bool, String> {
        let Some(record) = self.store.load()? else {
            return Ok(false);
        };
        token::revoke(&self.web, &self.endpoints, &record)?;
        self.store.remove()
    }
}

/// What is kept of a sign-in that checked out.
fn record_of(
    tokens: Tokens,
    who: Identity,
    client_id: String,
    host: String,
) -> Result<Record, String> {
    Ok(Record {
        email: who.email,
        issuer: who.issuer,
        subject: who.subject,
        client_id,
        ext_agent_host_id: host,
        id_token: tokens.id_token.clone().unwrap_or_default(),
        scopes: tokens.scopes(),
        access_token: tokens.access_token,
        refresh_token: tokens
            .refresh_token
            .ok_or("the sign-in gave no refresh token")?,
        token_type: tokens.token_type.unwrap_or_else(|| "Bearer".to_owned()),
        expires_in: tokens.expires_in.unwrap_or(3600),
        saved_at: now(),
        earliest_refresh_at: tokens.earliest_refresh_at,
    })
}

/// Listen on the loopback interface, at the usual port or, if it is taken, any other.
fn listen() -> Result<TcpListener, String> {
    TcpListener::bind(("127.0.0.1", PORT))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
        .map_err(|error| format!("cannot listen for the browser: {error}"))
}

/// Wait up to `wait` for the browser to come back to [`CALLBACK`], and return its query.
fn callback(listener: &TcpListener, wait: Duration) -> Result<BTreeMap<String, String>, String> {
    let failed = |error: std::io::Error| format!("cannot listen for the browser: {error}");
    listener.set_nonblocking(true).map_err(failed)?;
    let until = Instant::now() + wait;
    while Instant::now() < until {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(query) = answered(&stream) {
                    return Ok(query);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(failed(error)),
        }
    }
    Err(format!(
        "the browser did not come back within {} seconds",
        wait.as_secs()
    ))
}

/// Answer one request. Returns the query if it was the callback; anything else (a favicon)
/// is told there is nothing here.
fn answered(stream: &TcpStream) -> Option<BTreeMap<String, String>> {
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let target = line.split_whitespace().nth(1)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (status, page) = if path == CALLBACK {
        ("200 OK", DONE)
    } else {
        ("404 Not Found", "")
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{page}",
        page.len()
    );
    let mut writer = stream;
    let _written = writer.write_all(response.as_bytes());
    (path == CALLBACK).then(|| {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(name, value)| (decode(name), decode(value)))
            .collect()
    })
}

/// Open `url` in the person's browser, with the system's own command for it.
pub fn open_in_browser(url: &str) {
    let mut command = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut command = std::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        std::process::Command::new("xdg-open")
    };
    let _started = command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
