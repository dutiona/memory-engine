//! Machine tokens (ADR-0020 §4): the server-side token-hash file and the Bearer check.
//!
//! The file holds one `<machine> <sha256-hex>` line per machine. A request names its
//! machine only through the token it presents, so a [`Machine`] can only come out of
//! [`TokenTable::authenticate`].

use std::path::{Path, PathBuf};

pub(crate) use memory_engine::types::is_dns_label;

/// The machine a valid token belongs to: a DNS label from the token-hash file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine(String);

impl Machine {
    /// The machine's name as written in the token-hash file.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A machine for unit tests, which have no token file.
    #[cfg(test)]
    pub(crate) fn named(name: &str) -> Self {
        assert!(is_dns_label(name.as_bytes()), "{name:?}");
        Self(name.to_owned())
    }
}

/// The parsed token-hash file: the SHA-256 of each machine's token.
pub struct TokenTable {
    entries: Vec<([u8; 32], Machine)>,
}

/// Why the token-hash file was refused. A message never quotes the line's bytes, which
/// may hold a pasted token.
#[derive(Debug, thiserror::Error)]
pub enum TokenFileError {
    /// The file is missing or unreadable.
    #[error("cannot read the token-hash file {}: {source}", path.display())]
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// A line breaks the format.
    #[error("token-hash file line {line}: {reason}")]
    Line {
        /// The offending line, numbered from 1.
        line: usize,
        /// What is wrong with it.
        reason: &'static str,
    },
}

impl TokenTable {
    /// Read and parse the token-hash file.
    ///
    /// # Errors
    ///
    /// [`TokenFileError::Read`] when the file cannot be read, [`TokenFileError::Line`]
    /// for the first line that breaks the ADR-0020 §4 format.
    pub fn load(path: &Path) -> Result<Self, TokenFileError> {
        let bytes = std::fs::read(path).map_err(|source| TokenFileError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&bytes)
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, TokenFileError> {
        let mut entries: Vec<([u8; 32], Machine)> = Vec::new();
        for (index, chunk) in bytes.split_inclusive(|&b| b == b'\n').enumerate() {
            let line = index + 1;
            let err = |reason| TokenFileError::Line { line, reason };
            let body = chunk
                .strip_suffix(b"\n")
                .ok_or_else(|| err("no line feed at its end"))?;
            let mut fields = body.split(|&b| b == b' ');
            let (Some(name), Some(hash), None) = (fields.next(), fields.next(), fields.next())
            else {
                return Err(err("not two fields separated by one space"));
            };
            if !is_dns_label(name) {
                return Err(err("the machine is not a lowercase DNS label"));
            }
            let hash =
                decode_hash(hash).ok_or_else(|| err("the hash is not 64 lowercase hex digits"))?;
            // The label check above admits only ASCII.
            let name = String::from_utf8_lossy(name).into_owned();
            if entries.iter().any(|(_, m)| m.0 == name) {
                return Err(err("a repeated machine"));
            }
            if entries.iter().any(|(h, _)| *h == hash) {
                return Err(err("a repeated hash"));
            }
            entries.push((hash, Machine(name)));
        }
        Ok(Self { entries })
    }

    /// The machines the file admits, in file order: the names the engine keeps as written in
    /// its records' and memories' `machine` (#1136).
    ///
    /// # Examples
    ///
    /// ```
    /// use memory_engine_mcp::auth::TokenTable;
    ///
    /// let dir = tempfile::tempdir()?;
    /// let file = dir.path().join("tokens");
    /// std::fs::write(&file, format!("laptop {}\n", "0".repeat(64)))?;
    /// let table = TokenTable::load(&file)?;
    /// assert_eq!(table.machines().map(|m| m.as_str()).collect::<Vec<_>>(), ["laptop"]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn machines(&self) -> impl Iterator<Item = &Machine> {
        self.entries.iter().map(|(_, machine)| machine)
    }

    /// The machine whose token is presented in an `Authorization` header value, if any.
    ///
    /// A value off the token shape is refused before it is hashed; otherwise every entry
    /// is compared in constant time, with no early exit.
    #[must_use]
    pub fn authenticate(&self, authorization: &[u8]) -> Option<&Machine> {
        use sha2::{Digest, Sha256};
        use subtle::ConstantTimeEq;

        let token = authorization.strip_prefix(b"Bearer ")?;
        if !is_token_shaped(token) {
            return None;
        }
        let digest: [u8; 32] = Sha256::digest(token).into();
        let mut found = None;
        for (hash, machine) in &self.entries {
            if bool::from(hash.ct_eq(&digest)) {
                found = Some(machine);
            }
        }
        found
    }
}

/// `me_` followed by 32 base62 characters (ADR-0020 §4).
fn is_token_shaped(token: &[u8]) -> bool {
    token.len() == 35
        && token.starts_with(b"me_")
        && token[3..].iter().all(u8::is_ascii_alphanumeric)
}

/// 64 lowercase hex digits; the lowercase-only nibble map is the validation.
fn decode_hash(hex: &[u8]) -> Option<[u8; 32]> {
    let nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    };
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (byte, [high, low]) in out.iter_mut().zip(hex.as_chunks::<2>().0) {
        *byte = (nibble(*high)? << 4) | nibble(*low)?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write;
            write!(s, "{b:02x}").unwrap();
            s
        })
    }

    fn line(machine: &str, token: &str) -> String {
        format!("{machine} {}\n", hex(&Sha256::digest(token.as_bytes())))
    }

    fn token(fill: char) -> String {
        format!("me_{}", fill.to_string().repeat(32))
    }

    fn bearer(token: &str) -> Vec<u8> {
        format!("Bearer {token}").into_bytes()
    }

    fn parse_str(s: &str) -> Result<TokenTable, TokenFileError> {
        TokenTable::parse(s.as_bytes())
    }

    fn failing_line(s: &str) -> usize {
        match parse_str(s) {
            Err(TokenFileError::Line { line, .. }) => line,
            Err(e) => panic!("expected a line error, got {e}"),
            Ok(_) => panic!("expected {s:?} to be refused"),
        }
    }

    #[test]
    fn adr_test_vector() {
        let t = token('0');
        assert_eq!(
            hex(&Sha256::digest(t.as_bytes())),
            "f7ca4ebc8eb419b37dea09411716d9aa5506b9e5572f1a5e8b3170e815ee13aa"
        );
        let table =
            parse_str("laptop f7ca4ebc8eb419b37dea09411716d9aa5506b9e5572f1a5e8b3170e815ee13aa\n")
                .unwrap();
        assert_eq!(table.authenticate(&bearer(&t)).unwrap().as_str(), "laptop");
    }

    #[test]
    fn each_machine_is_named_by_its_own_token() {
        let file = line("laptop", &token('L')) + &line("desktop", &token('D'));
        let table = parse_str(&file).unwrap();
        assert_eq!(
            table.authenticate(&bearer(&token('L'))).unwrap().as_str(),
            "laptop"
        );
        assert_eq!(
            table.authenticate(&bearer(&token('D'))).unwrap().as_str(),
            "desktop"
        );
        assert!(table.authenticate(&bearer(&token('X'))).is_none());
    }

    #[test]
    fn an_empty_file_admits_no_one() {
        let table = parse_str("").unwrap();
        assert!(table.authenticate(&bearer(&token('0'))).is_none());
    }

    #[test]
    fn a_missing_file_is_a_read_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = TokenTable::load(&dir.path().join("absent")).err().unwrap();
        assert!(matches!(err, TokenFileError::Read { .. }));
        assert!(err.to_string().contains("absent"));
    }

    #[test]
    fn malformed_lines_are_refused_with_their_number() {
        let good = line("laptop", &token('L'));
        let hash = hex(&Sha256::digest(token('D').as_bytes()));
        let cases = [
            format!("desktop {}\n", hash.to_uppercase()),
            format!("Desktop {hash}\n"),
            format!("desktop  {hash}\n"),
            format!("desktop {hash}\r\n"),
            format!("desktop {hash}"),
            format!("laptop {hash}\n"),
            line("desktop", &token('L')),
            "\n".to_owned(),
            format!("# desktop {hash}\n"),
            format!("desktop\t{hash}\n"),
            format!("-desktop {hash}\n"),
            format!("desktop- {hash}\n"),
            format!("{} {hash}\n", "a".repeat(64)),
            format!("desktop {}\n", &hash[..63]),
            format!("desktop {hash}0\n"),
            format!("desktop {hash} \n"),
        ];
        for case in cases {
            assert_eq!(failing_line(&(good.clone() + &case)), 2, "case {case:?}");
        }
    }

    #[test]
    fn a_63_character_label_is_accepted() {
        parse_str(&line(&"a".repeat(63), &token('L'))).unwrap();
    }

    #[test]
    fn a_pasted_token_is_refused_without_being_quoted() {
        let t = token('S');
        let err = parse_str(&format!("laptop {t}\n")).err().unwrap();
        assert!(!err.to_string().contains(&t));
    }

    #[test]
    fn off_shape_values_are_refused() {
        let t = token('L');
        let table = parse_str(&line("laptop", &t)).unwrap();
        let refused = [
            format!("bearer {t}"),
            format!("Basic {t}"),
            format!("Bearer  {t}"),
            format!("Bearer {t} "),
            format!("Bearer {}", &t[..34]),
            format!("Bearer {t}L"),
            format!("Bearer ME_{}", &t[3..]),
            format!("Bearer me_{}é", "L".repeat(30)),
            format!("Bearer me_{}-", "L".repeat(31)),
            t.clone(),
            String::new(),
        ];
        for value in refused {
            assert!(
                table.authenticate(value.as_bytes()).is_none(),
                "value {value:?}"
            );
        }
        assert!(table.authenticate(&bearer(&t)).is_some());
    }

    #[test]
    fn an_off_shape_value_is_never_hashed() {
        let off_shape = format!("me_{}", "0".repeat(31));
        let table = parse_str(&line("ghost", &off_shape)).unwrap();
        assert!(table.authenticate(&bearer(&off_shape)).is_none());
    }
}
