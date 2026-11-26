//! The secrets of an agent file, as a person deals with them: how each stands, putting a
//! value in and taking it out, signing in and out. A value is never shown or said.

use std::error::Error;
use std::process::ExitCode;

use corduroy_std::Standard;

use super::{Console, Described, Done, SecretAsked};
use crate::agent::AgentFile;
use crate::extension::Asked;
use crate::terminal::{Out, say};

/// What names the secret `name` in `file`: providers and MCP servers.
fn users(file: &AgentFile, name: &str) -> Vec<String> {
    let named = |secret: &Option<String>| secret.as_deref() == Some(name);
    let providers = file
        .providers
        .iter()
        .filter(|(_, provider)| named(&provider.spec.secret))
        .map(|(provider, _)| format!("provider {provider}"));
    let servers = file
        .mcp
        .iter()
        .filter(|(_, server)| named(&server.secret))
        .map(|(server, _)| format!("MCP server {server}"));
    providers.chain(servers).collect()
}

/// How one credential of the agent stands: a line to say, and whether it can be had.
pub(super) struct Standing {
    /// Whether the value can be had.
    pub(super) good: bool,
    /// What it is: `anthropic (keychain)`.
    pub(super) what: String,
    /// How it stands, and what to do if badly.
    pub(super) state: String,
}

/// How every credential the agent names stands: its secrets, then the keys of the providers
/// that name none and read a variable of the environment. No value is read out.
pub(super) fn standing(
    described: &Described,
    standard: &Standard,
) -> Result<Vec<Standing>, Box<dyn Error>> {
    let Described { file, places } = described;
    let mut all = Vec::new();
    for (name, entry) in &file.secrets {
        let SecretAsked { kind, config } = described.secret(name)?;
        let asked = Asked {
            name,
            config: &config,
            places,
        };
        let state = standard
            .state(&kind, &asked)
            .unwrap_or_else(|| Ok("of a kind that does not say how it stands".to_owned()));
        let users = users(file, name);
        let used = if users.is_empty() {
            String::new()
        } else {
            format!(", for {}", users.join(" and "))
        };
        all.push(Standing {
            good: state.is_ok(),
            what: format!("{name} ({}{used})", entry.kind_said()),
            state: state.unwrap_or_else(|why| why),
        });
    }
    for (name, provider) in &file.providers {
        let Some(variable) = provider.spec.key_variable() else {
            continue;
        };
        let set = std::env::var_os(&variable).is_some_and(|value| !value.is_empty());
        all.push(Standing {
            good: set,
            what: format!("provider {name} (no secret named)"),
            state: format!(
                "reads ${variable} from the environment, which is {}",
                if set { "set" } else { "not set" }
            ),
        });
    }
    Ok(all)
}

/// List the agent's secrets and how each stands. Fails if one cannot be had.
pub(super) fn list(described: &Described, standard: &Standard, out: &Out) -> Done {
    let all = standing(described, standard)?;
    if all.is_empty() {
        say(out, "this agent names no secret and needs no key");
    }
    for standing in &all {
        let mark = if standing.good { "✓" } else { "✗" };
        say(
            out,
            &format!("{mark} {}: {}", standing.what, standing.state),
        );
    }
    Ok(if all.iter().all(|standing| standing.good) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The secrets of the file whose kind `does` holds for: those whose value is put in, or
/// those that are signed in to.
fn those(described: &Described, does: impl Fn(&str) -> bool) -> Vec<String> {
    described
        .file
        .secrets
        .iter()
        .filter(|(_, secret)| does(&secret.extension()))
        .map(|(name, _)| name.clone())
        .collect()
}

/// How to add what there is nothing of, for the error.
const ADD_KEPT: &str = "this agent has no secret kept in the keychain. To keep a key there, \
                        add:\n\n[secrets.anthropic]\nkind = \"keychain\"\n\nand name it as a \
                        provider's `secret`";
const ADD_SIGNED: &str = "this agent has no secret that is signed in to. For a ChatGPT plan, \
                          add:\n\n[secrets.chatgpt]\nkind = \"chatgpt\"\n\nand name it as a \
                          provider's `secret`";

/// The secret meant: the one `named`, or the only one of `candidates`.
fn chosen(
    named: Option<&str>,
    candidates: &[String],
    none: &str,
) -> Result<String, Box<dyn Error>> {
    match (named, candidates) {
        (Some(name), _) => Ok(name.to_owned()),
        (None, [only]) => Ok(only.clone()),
        (None, []) => Err(none.into()),
        (None, several) => Err(format!("say which secret, one of: {}", several.join(", ")).into()),
    }
}

/// Put in (or, if not `set`, take out) the value of a secret of the agent file whose kind
/// keeps one: the one `named`, or the only such secret.
pub(super) fn change(
    set: bool,
    named: Option<&str>,
    described: &Described,
    standard: &Standard,
    console: &mut Console<'_>,
) -> Done {
    let kept = those(described, |kind| standard.keep(kind).is_some());
    let name = chosen(named, &kept, ADD_KEPT)?;
    let SecretAsked { kind, config } = described.secret(&name)?;
    let Some(keep) = standard.keep(&kind) else {
        let how = if standard.sign_in(&kind).is_some() {
            format!("it is signed in to: `corduroy login {name}`")
        } else {
            "its value is set where its kind reads it (an environment variable, for `env`); \
             `kind = \"keychain\"` makes it one that is put in here"
                .to_owned()
        };
        return Err(format!("the value of `{name}` is not put in with this command: {how}").into());
    };
    let asked = Asked {
        name: &name,
        config: &config,
        places: &described.places,
    };
    if set {
        let prompt = format!("Value for `{name}` (not shown as you type or paste): ");
        let value = console.secret(&prompt)?;
        let place = keep.put(&asked, &value)?;
        say(&console.out, &format!("kept `{name}` in {place}"));
    } else if keep.remove(&asked)? {
        say(&console.out, &format!("removed the value of `{name}`"));
    } else {
        say(&console.out, &format!("no value was kept for `{name}`"));
    }
    Ok(ExitCode::SUCCESS)
}

/// The secret `login` and `logout` mean: the one `named`, or the only one that is signed in
/// to. A key is not signed in to: where the file has only secrets whose value is put in, the
/// error says the command that does that.
fn signed(
    named: Option<&str>,
    described: &Described,
    standard: &Standard,
) -> Result<String, Box<dyn Error>> {
    let signed = those(described, |kind| standard.sign_in(kind).is_some());
    let none = match those(described, |kind| standard.keep(kind).is_some()).first() {
        Some(kept) if signed.is_empty() => format!(
            "{ADD_SIGNED}.\n\nA key is not signed in to: the value of `{kept}` is put in with \
             `corduroy secrets set {kept}`"
        ),
        _ => ADD_SIGNED.to_owned(),
    };
    chosen(named, &signed, &none)
}

/// Sign in for (or, if not `inward`, out of) what a secret of the agent file stands for: the
/// one `named`, or the only one that is signed in to.
pub(super) fn sign(
    inward: bool,
    named: Option<&str>,
    described: &Described,
    standard: &Standard,
    console: &Console<'_>,
) -> Done {
    let name = signed(named, described, standard)?;
    let SecretAsked { kind, config } = described.secret(&name)?;
    let Some(sign_in) = standard.sign_in(&kind) else {
        let how = if standard.keep(&kind).is_some() {
            format!(": its value is put in with `corduroy secrets set {name}`")
        } else {
            String::new()
        };
        return Err(format!("`{name}` is no secret that is signed in to{how}").into());
    };
    let asked = Asked {
        name: &name,
        config: &config,
        places: &described.places,
    };
    let out = &console.out;
    let mut tell = |line: &str| say(out, line);
    if inward {
        sign_in.sign_in(&asked, &mut tell)?;
    } else {
        sign_in.sign_out(&asked, &mut tell)?;
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use crate::agent::Places;

    use super::*;

    /// An agent file with these `[secrets.*]` entries, read against no real place.
    fn described(secrets: &str) -> Described {
        let text =
            format!("provider = \"p\"\nmodel = \"m\"\n[providers.p]\nkind = \"ollama\"\n{secrets}");
        Described {
            file: AgentFile::parse(&text).unwrap(),
            places: Places::new("/work".into(), None),
        }
    }

    #[test]
    fn login_means_a_sign_in_and_never_a_key() {
        let standard = Standard::new();
        let meant = |named: Option<&str>, secrets: &str| {
            signed(named, &described(secrets), &standard).map_err(|error| error.to_string())
        };
        let kept = "[secrets.key]\nkind = \"keychain\"\n";
        let plan = "[secrets.plan]\nkind = \"chatgpt\"\n";
        let env = "[secrets.token]\nenv = \"TOKEN\"\n";

        // The only sign-in is meant, whatever keys are beside it.
        assert_eq!(meant(None, &format!("{plan}{env}")), Ok("plan".to_owned()));
        assert_eq!(meant(None, &format!("{kept}{plan}")), Ok("plan".to_owned()));
        // Two have to be told apart.
        let two = format!("{plan}[secrets.other]\nkind = \"chatgpt\"\n");
        assert_eq!(
            meant(None, &two),
            Err("say which secret, one of: other, plan".to_owned())
        );
        // With no sign-in, what to add is said; and where there is a key in the keychain,
        // the command that puts its value in.
        let none = meant(None, env).unwrap_err();
        assert!(
            none.contains("kind = \"chatgpt\"") && !none.contains("secrets set"),
            "{none}"
        );
        let key = meant(None, &format!("{kept}{env}")).unwrap_err();
        assert!(
            key.ends_with("is put in with `corduroy secrets set key`"),
            "{key}"
        );
    }
}
