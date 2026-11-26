//! `corduroy init`: write the default agent file, saying where its keys come from.

use std::path::Path;
use std::process::ExitCode;

use corduroy_std::Standard;

use super::Done;
use crate::agent::{AGENT_FILE, Keys, Starter, Vendor};
use crate::terminal::{Out, say};

/// What the words after `init` ask for: whose model, and where the keys come from. Either
/// may be left to the command.
#[derive(Debug, Default, PartialEq, Eq)]
struct Wanted {
    vendor: Option<Vendor>,
    keys: Option<Keys>,
}

fn asked(words: &[&str]) -> Result<Wanted, String> {
    let (mut vendor, mut keys) = (None, None);
    let mut words = words.iter();
    while let Some(word) = words.next() {
        if *word == "--secrets" {
            keys = Some(match words.next() {
                Some(&"keychain") => Keys::Keychain,
                Some(&"env") => Keys::Environment,
                _ => return Err("--secrets takes `keychain` or `env`".to_owned()),
            });
        } else {
            vendor = Some(Vendor::named(word).ok_or_else(|| {
                format!(
                    "`{word}` is not something init writes an agent for: one of {}",
                    Vendor::WORDS.join(", ")
                )
            })?);
        }
    }
    Ok(Wanted { vendor, keys })
}

/// What the person does next about `vendor`'s key, kept as `keys` says.
fn next(vendor: Vendor, keys: Keys, standard: &Standard) -> Vec<String> {
    let (Some(name), variable) = (vendor.secret(), vendor.variable()) else {
        return Vec::new();
    };
    let here = |variable: &str| std::env::var_os(variable).is_some_and(|value| !value.is_empty());
    match (vendor, keys, variable) {
        (Vendor::ChatGpt, ..) => vec![format!(
            "sign in to your ChatGPT plan:  corduroy login {name}"
        )],
        (_, Keys::Keychain, variable) => {
            let mut lines = vec![format!(
                "put the key `{name}` in {}:  corduroy secrets set {name}",
                standard.keychain().name()
            )];
            if let Some(variable) = variable.filter(|variable| here(variable)) {
                let piped = if cfg!(windows) {
                    format!("echo $env:{variable} | corduroy secrets set {name}")
                } else {
                    format!("printenv {variable} | corduroy secrets set {name}")
                };
                lines.push(format!(
                    "  (${variable} is set here; this copies it:  {piped})"
                ));
            }
            lines
        }
        (_, _, Some(variable)) => vec![format!(
            "the key `{name}` is read from ${variable}, which is {} here",
            if here(variable) { "set" } else { "not set" }
        )],
        (_, _, None) => Vec::new(),
    }
}

/// Write the default agent file into `workspace`: for the vendor the words name (or the one
/// whose key the environment holds), with its keys in the keychain where this machine has
/// one and in the environment where it has none, unless `--secrets` says which.
pub(super) fn init(words: &[&str], workspace: &Path, standard: &Standard, out: &Out) -> Done {
    let path = workspace.join(AGENT_FILE);
    if path.exists() {
        return Err(format!("{} already exists", path.display()).into());
    }
    let Wanted { vendor, keys } = asked(words)?;
    let reachable = standard.keychain().reachable();
    let (keys, why_not) = match (keys, reachable) {
        // Asked for and not there: nothing else is chosen in its place.
        (Some(Keys::Keychain), Err(why)) => {
            return Err(format!("{why}: `--secrets env` reads keys from the environment").into());
        }
        (Some(keys), _) => (keys, None),
        (None, Ok(())) => (Keys::Keychain, None),
        (None, Err(why)) => (Keys::Environment, Some(why)),
    };
    let starter = Starter::from_environment(vendor, keys);
    std::fs::write(&path, starter.text())?;
    let wrote = format!(
        "wrote {}: edit it to change what the agent can do",
        path.display()
    );
    say(out, &wrote);
    if let Some(why) = why_not.filter(|_| !starter.secrets().is_empty()) {
        // A sign-in is no variable: without a keychain it is kept in a file.
        let instead = if starter.vendor == Vendor::ChatGpt {
            "so the sign-in is kept in a file of your own"
        } else {
            "so its keys are read from the environment"
        };
        say(out, &format!("{why}, {instead}"));
    }
    for vendor in starter.secrets() {
        for line in next(vendor, starter.keys, standard) {
            say(out, &line);
        }
    }
    say(out, "then try it for real:  corduroy doctor");
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_after_init_say_whose_model_and_where_keys_come_from() {
        assert_eq!(asked(&[]), Ok(Wanted::default()));
        assert_eq!(
            asked(&["chatgpt", "--secrets", "keychain"]),
            Ok(Wanted {
                vendor: Some(Vendor::ChatGpt),
                keys: Some(Keys::Keychain)
            })
        );
        assert_eq!(
            asked(&["--secrets", "env", "openai"]),
            Ok(Wanted {
                vendor: Some(Vendor::OpenAi),
                keys: Some(Keys::Environment)
            })
        );
        assert!(
            asked(&["--secrets"])
                .unwrap_err()
                .contains("`keychain` or `env`")
        );
        assert!(asked(&["--secrets", "file"]).is_err());
        let unknown = asked(&["gemini"]).unwrap_err();
        assert!(
            unknown.contains("anthropic, openai, chatgpt, ollama"),
            "{unknown}"
        );
    }
}
