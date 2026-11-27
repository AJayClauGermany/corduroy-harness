//! Web addresses: what a link or a redirect points at, given the page it is on. Pure, so it
//! is tested natively.

/// Whether `url` is an absolute `http` or `https` address.
pub fn is_web(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Whether `reference` names a scheme of its own, such as `mailto:` or `javascript:`.
fn has_scheme(reference: &str) -> bool {
    let Some((scheme, _)) = reference.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
}

/// The address `reference` points at from the page at `base`, an absolute web address:
/// itself if it is one, else worked out against `base` as a browser would. `None` for
/// what is not a web address at all (`mailto:`, `javascript:`), a place on the same page
/// (`#top`) and nothing.
pub fn resolve(base: &str, reference: &str) -> Option<String> {
    let reference = reference.trim();
    if reference.is_empty() || reference.starts_with('#') {
        return None;
    }
    if is_web(reference) {
        return Some(reference.to_owned());
    }
    if has_scheme(reference) {
        return None;
    }
    let (scheme, rest) = base.split_once("://")?;
    if let Some(elsewhere) = reference.strip_prefix("//") {
        return Some(format!("{scheme}://{elsewhere}"));
    }
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    let origin = format!("{scheme}://{authority}");
    // The page's path, without what follows it.
    let path = path.split(['?', '#']).next().unwrap_or_default();
    if reference.starts_with('?') {
        let path = if path.is_empty() { "/" } else { path };
        return Some(format!("{origin}{path}{reference}"));
    }
    let (target, after) = match reference.find(['?', '#']) {
        Some(at) => reference.split_at(at),
        None => (reference, ""),
    };
    let joined = if target.starts_with('/') {
        target.to_owned()
    } else {
        let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
        format!("{directory}/{target}")
    };
    Some(format!("{origin}{}{after}", tidy(&joined)))
}

/// `path` with its `.` and `..` names worked out.
fn tidy(path: &str) -> String {
    let mut names: Vec<&str> = Vec::new();
    for name in path.split('/') {
        match name {
            "." => {}
            ".." => {
                names.pop();
            }
            name => names.push(name),
        }
    }
    let mut tidied = names
        .iter()
        .filter(|name| !name.is_empty())
        .fold(String::new(), |tidied, name| tidied + "/" + name);
    let ends_in_directory = path.ends_with('/') || path.ends_with("/.") || path.ends_with("/..");
    if tidied.is_empty() || ends_in_directory {
        tidied.push('/');
    }
    tidied
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "https://example.com/docs/guide/intro.html?v=2#top";

    #[test]
    fn an_absolute_address_is_itself() {
        let other = "http://other.example/x";
        assert_eq!(resolve(PAGE, other).unwrap(), other);
        assert_eq!(
            resolve(PAGE, "//cdn.example/a.js").unwrap(),
            "https://cdn.example/a.js"
        );
        assert!(is_web("HTTPS://Example.com"));
        assert!(!is_web("ftp://example.com"));
        assert!(!is_web("example.com"));
    }

    #[test]
    fn a_relative_address_is_worked_out_against_the_page() {
        let at = |reference| resolve(PAGE, reference).unwrap();
        assert_eq!(at("/about"), "https://example.com/about");
        assert_eq!(at("next.html"), "https://example.com/docs/guide/next.html");
        assert_eq!(
            at("./next.html#s"),
            "https://example.com/docs/guide/next.html#s"
        );
        assert_eq!(at("../api/"), "https://example.com/docs/api/");
        assert_eq!(at("../../../../x?q=1"), "https://example.com/x?q=1");
        assert_eq!(
            at("?page=2"),
            "https://example.com/docs/guide/intro.html?page=2"
        );
        assert_eq!(at(".."), "https://example.com/docs/");
        assert_eq!(
            resolve("https://example.com", "a").unwrap(),
            "https://example.com/a"
        );
        assert_eq!(
            resolve("https://example.com", "?q").unwrap(),
            "https://example.com/?q"
        );
    }

    #[test]
    fn what_is_not_a_web_address_is_none() {
        for reference in [
            "",
            "#top",
            "mailto:a@example.com",
            "javascript:void(0)",
            "tel:+1",
        ] {
            assert_eq!(resolve(PAGE, reference), None, "{reference}");
        }
        // A colon later on is not a scheme.
        assert_eq!(
            resolve(PAGE, "a/b:c").unwrap(),
            "https://example.com/docs/guide/a/b:c"
        );
    }
}
