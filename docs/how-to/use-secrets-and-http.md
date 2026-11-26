# Give an add-on a secret, and let it make HTTP requests

An add-on never sees a secret's value. It gets a handle to it, hands the handle to an HTTP
request, and the host writes the value into the header it sends. The log records the
secret's name, never its value, and leaves response bodies out.

## 1. Declare the secret

```toml
[secrets.github]          # kept in your keychain; nothing more to write
```

Put the value in once. It is written into the header exactly as it is, so give the whole
header value (`Bearer ghp_...`); it is not shown as you type or paste it:

```bash
corduroy secrets set github
```

Where there is no keychain (a server, a container), or the value is already in a variable
there, name the variable and it is read from the environment when a request needs it:

```toml
[secrets.github]
env = "GITHUB_AUTH"
```

```bash
export GITHUB_AUTH="Bearer ghp_..."
```

The add-on cannot tell the difference: it gets the same handle either way.

## 2. Use it from the add-on

In Rust, with `corduroy-guest`:

```rust
use corduroy_guest::host::http::{self, Header, HeaderValue, Request};
use corduroy_guest::host::secret;

let token = secret::get("github")?;
let response = http::send(&Request {
    method: "GET".to_owned(),
    url: format!("https://api.github.com/repos/{repo}/issues?state=open"),
    headers: vec![
        Header { name: "authorization".to_owned(), value: HeaderValue::Secret(&token) },
        Header { name: "user-agent".to_owned(), value: HeaderValue::Text("corduroy".to_owned()) },
    ],
    body: None,
})?;
let body = String::from_utf8_lossy(&response.body);
```

In JavaScript and Python the same functions are imported from `corduroy:plugin/secret@0.1.0`
and `corduroy:plugin/http@0.1.0` (JavaScript), or `wit_world.imports.secret` and
`wit_world.imports.http` (Python); build with `--capabilities secret,http`. JavaScript's
`fetch` is not available in an add-on.

## 3. Grant both, narrowly

```toml
[[plugins]]
name = "gh-issues"
path = "gh_issues.wasm"
grants = { secret = "github", http = "api.github.com" }
```

`secret` is scoped by secret name and `http` by host name. The request is refused if its URL
is not on a granted host.

## What the host enforces

- No redirects are followed: the grant was checked against one host, and a redirect could
  lead anywhere. A 3xx comes back to the add-on as a response.
- URLs that parsers might read differently (a user name before the host, a backslash,
  whitespace, control characters) are refused.
- Requests wait at most a bounded time, set by the host.
- The request and response are recorded as `http/request` and `http/response`, with the
  secret named and bodies left out.
