//! A value longer than a keychain takes in one item, kept as several.
//!
//! The Windows Credential Manager holds at most 2560 bytes in a credential, and a sign-in's
//! tokens are longer than that. A long value is cut into pieces kept as `<item>#1`,
//! `<item>#2` and so on, and `<item>` itself then holds a line saying how many there are.
//! `#` is no character of an item's name, so a piece is never taken for an item.

/// A store of bytes by name, as a system offers it.
pub(super) trait Raw {
    /// The bytes kept as `item`, if any.
    fn read(&self, item: &str) -> Result<Option<Vec<u8>>, String>;
    /// Keep `value` as `item`.
    fn write(&self, item: &str, value: &[u8]) -> Result<(), String>;
    /// Forget `item`. Returns whether it was there.
    fn erase(&self, item: &str) -> Result<bool, String>;
}

/// What an item that was cut up holds, before the number of its pieces.
const MARK: &[u8] = b"corduroy-pieces ";

fn piece(item: &str, number: usize) -> String {
    format!("{item}#{number}")
}

/// How many pieces `held` says an item was cut into: none if it holds the value itself.
fn count(held: Option<&[u8]>) -> usize {
    held.and_then(|held| held.strip_prefix(MARK))
        .and_then(|number| str::from_utf8(number).ok())
        .and_then(|number| number.parse().ok())
        .unwrap_or(0)
}

/// The value kept as `item` in `raw`, put together again if `most` (the most bytes one item
/// takes there) had it cut up.
pub(super) fn get(
    raw: &dyn Raw,
    item: &str,
    most: Option<usize>,
) -> Result<Option<Vec<u8>>, String> {
    let Some(held) = raw.read(item)? else {
        return Ok(None);
    };
    let pieces = most.map_or(0, |_| count(Some(&held)));
    if pieces == 0 {
        return Ok(Some(held));
    }
    let mut value = Vec::new();
    for number in 1..=pieces {
        let part = raw.read(&piece(item, number))?.ok_or_else(|| {
            format!("part {number} of {pieces} of `{item}` is missing: set it again")
        })?;
        value.extend(part);
    }
    Ok(Some(value))
}

/// Keep `value` as `item` in `raw`, in pieces of at most `most` bytes if there is a most
/// and it is longer than that.
pub(super) fn set(
    raw: &dyn Raw,
    item: &str,
    value: &[u8],
    most: Option<usize>,
) -> Result<(), String> {
    let Some(most) = most.filter(|most| *most > 0) else {
        return raw.write(item, value);
    };
    let before = count(raw.read(item)?.as_deref());
    let pieces = if value.len() <= most && !value.starts_with(MARK) {
        raw.write(item, value)?;
        0
    } else {
        let parts: Vec<&[u8]> = value.chunks(most).collect();
        for (index, part) in parts.iter().enumerate() {
            raw.write(&piece(item, index.saturating_add(1)), part)?;
        }
        let mut says = MARK.to_vec();
        says.extend(parts.len().to_string().bytes());
        raw.write(item, &says)?;
        parts.len()
    };
    // What an earlier, longer value left behind.
    for number in pieces.saturating_add(1)..=before {
        raw.erase(&piece(item, number))?;
    }
    Ok(())
}

/// Forget `item` in `raw`, with its pieces if `most` had it cut up. Returns whether it was
/// there.
pub(super) fn remove(raw: &dyn Raw, item: &str, most: Option<usize>) -> Result<bool, String> {
    let pieces = match most {
        Some(_) => count(raw.read(item)?.as_deref()),
        None => 0,
    };
    for number in 1..=pieces {
        raw.erase(&piece(item, number))?;
    }
    raw.erase(item)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;

    /// A store that takes at most eight bytes in an item, as a system with a limit does.
    #[derive(Default)]
    struct Small(Mutex<BTreeMap<String, Vec<u8>>>);

    impl Small {
        fn kept(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        fn names(&self) -> Vec<String> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .keys()
                .cloned()
                .collect()
        }
    }

    impl Raw for Small {
        fn read(&self, item: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(item)
                .cloned())
        }

        fn write(&self, item: &str, value: &[u8]) -> Result<(), String> {
            // The line that counts the pieces is the one thing longer than a piece.
            if value.len() > 8 && !value.starts_with(MARK) {
                return Err(format!("{} bytes are too long", value.len()));
            }
            self.kept().insert(item.to_owned(), value.to_vec());
            Ok(())
        }

        fn erase(&self, item: &str) -> Result<bool, String> {
            Ok(self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(item)
                .is_some())
        }
    }

    #[test]
    fn a_long_value_is_kept_in_pieces_and_read_back_whole() {
        let store = Small::default();
        let most = Some(8);
        // Cut in the middle of a character, and put together again.
        let long = "a token wïth more than eight bytes";
        set(&store, "plan", long.as_bytes(), most).unwrap();
        assert_eq!(
            store.names(),
            ["plan", "plan#1", "plan#2", "plan#3", "plan#4", "plan#5"]
        );
        assert_eq!(get(&store, "plan", most).unwrap().unwrap(), long.as_bytes());

        // A shorter value leaves no piece of the longer one behind.
        set(&store, "plan", b"0123456789", most).unwrap();
        assert_eq!(store.names(), ["plan", "plan#1", "plan#2"]);
        assert_eq!(get(&store, "plan", most).unwrap().unwrap(), b"0123456789");
        set(&store, "plan", b"short", most).unwrap();
        assert_eq!(store.names(), ["plan"]);
        assert_eq!(get(&store, "plan", most).unwrap().unwrap(), b"short");

        set(&store, "plan", long.as_bytes(), most).unwrap();
        assert!(remove(&store, "plan", most).unwrap());
        assert_eq!(store.names(), Vec::<String>::new());
        assert!(!remove(&store, "plan", most).unwrap());
        assert_eq!(get(&store, "plan", most).unwrap(), None);
    }

    #[test]
    fn a_missing_piece_is_said_and_a_store_without_a_limit_keeps_values_whole() {
        let store = Small::default();
        set(&store, "plan", b"0123456789", Some(8)).unwrap();
        store.erase("plan#2").unwrap();
        let error = get(&store, "plan", Some(8)).unwrap_err();
        assert_eq!(error, "part 2 of 2 of `plan` is missing: set it again");

        // With no limit nothing is cut, and nothing read is taken for a count of pieces.
        let whole = Small::default();
        assert!(set(&whole, "plan", b"0123456789", None).is_err());
        set(&whole, "plan", b"corduroy-pieces 3", None).unwrap();
        assert_eq!(
            get(&whole, "plan", None).unwrap().unwrap(),
            b"corduroy-pieces 3"
        );
        assert!(remove(&whole, "plan", None).unwrap());
    }

    #[test]
    fn a_value_that_looks_like_a_count_is_kept_in_pieces_so_it_reads_back_as_itself() {
        let store = Small::default();
        set(&store, "odd", b"corduroy-pieces 2", Some(8)).unwrap();
        assert_eq!(
            get(&store, "odd", Some(8)).unwrap().unwrap(),
            b"corduroy-pieces 2"
        );
    }
}
