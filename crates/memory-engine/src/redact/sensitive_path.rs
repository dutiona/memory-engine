//! Which paths are secrets (#1024): an event touching one is stored as a stub.
//!
//! A port of my-dotfiles' live command guard, `guard/internal/rules/names.go` `IsSecret` for a
//! path the agent spelled, with its table as the tests. Two differences, because the guard
//! decides what an agent may read on its own machine while this decides what persists:
//! - the server never knows a client's home, so a home-relative credential store matches as a
//!   whole path or after any `/` (wider than the guard, never narrower on the spelled path);
//! - every file under a `.ssh` directory but public keys, `config`, `known_hosts*` and
//!   `authorized_keys*` is a secret, and so are `/etc/shadow` and `/etc/gshadow`.
//!
//! The guard's exemptions for my-dotfiles' own policy files are not ported. Symlink targets are
//! outside the contract: the server cannot see the client's file system.

const TEMPLATE_SUFFIXES: &[&str] = &[".example", ".sample", ".template"];

const EXACT_SECRETS: &[&str] = &[
    "secrets.json",
    "credentials.json",
    ".credentials.json",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "id_dsa",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".pgpass",
    ".git-credentials",
    "aws_credentials",
    "gcloud_credentials.json",
    "service_account.json",
    "keystore.jks",
    "keystore.p12",
    ".htpasswd",
];

const KEY_EXTENSIONS: &[&str] = &[".pem", ".key", ".p12", ".pfx", ".jks", ".keystore"];

/// Credential stores, relative to a home directory.
const CREDENTIAL_STORES: &[&str] = &[
    ".codex/auth.json",
    ".claude/.credentials.json",
    ".claude.json",
    ".config/opencode/auth.json",
    ".local/share/opencode/auth.json",
    ".aws/credentials",
    ".config/gh/hosts.yml",
    ".config/gcloud/application_default_credentials.json",
    ".gemini/antigravity-cli/antigravity-oauth-token",
    ".docker/config.json",
    ".kube/config",
];

/// Credential-store directories, relative to a home directory: the directory and all under it.
const CREDENTIAL_DIRS: &[&str] = &[
    ".config/ai-provider-secrets",
    ".config/memory-engine/secrets",
    ".config/memory-engine/test-secrets",
];

const DATA_EXTENSIONS: &[&str] = &["", "json", "yaml", "yml", "toml", "ini", "txt", "env"];
const SECRET_NAME_PREFIXES: &[&str] =
    &["secrets.", "secret.", "credentials.", "password.", "token."];
const SECRET_NAME_PARTS: &[&str] = &["api_key", "apikey", "private_key"];
const SECRET_DIRS: &[&str] = &["secrets", "credentials", "vault"];

/// Whether `path`, as an agent spelled it (absolute, `~/`-relative or relative), names a secret.
///
/// # Examples
///
/// ```
/// use memory_engine::redact::is_sensitive_path;
///
/// assert!(is_sensitive_path("/srv/app/.env"));
/// assert!(is_sensitive_path("~/.kube/config"));
/// assert!(!is_sensitive_path("/srv/app/.env.example"));
/// assert!(!is_sensitive_path("src/token.ts"));
/// ```
#[must_use]
#[allow(clippy::case_sensitive_file_extension_comparisons)] // `name` is folded to lower case
pub fn is_sensitive_path(path: &str) -> bool {
    let clean = clean(&path.replace('\\', "/"));
    // The exemptions read the plainly lower-cased name; every denial reads the folded path, so a
    // spelling a case-insensitive file system opens as the secret is denied, and folding never
    // widens an exemption.
    let plain = base(&clean).to_lowercase();
    if TEMPLATE_SUFFIXES.iter().any(|s| plain.ends_with(s)) {
        return false;
    }
    let folded = fold(&clean);
    let name = base(&folded);
    if name == ".env" || name.starts_with(".env.") || name.ends_with(".env") {
        return true;
    }
    if EXACT_SECRETS.contains(&name) || credential_store(&folded) {
        return true;
    }
    if KEY_EXTENSIONS.iter().any(|e| name.ends_with(e)) {
        return true;
    }
    let dirs: Vec<&str> = dir(&folded).split('/').collect();
    if ssh_private_key(&dirs, name)
        || (dirs.last() == Some(&"etc") && matches!(name, "shadow" | "gshadow"))
    {
        return true;
    }
    if name.ends_with(".crt") {
        return true;
    }
    let ext = name
        .rfind('.')
        .filter(|&i| i > 0)
        .map_or("", |i| &name[i + 1..]);
    if !DATA_EXTENSIONS.contains(&ext) {
        return false;
    }
    SECRET_NAME_PREFIXES.iter().any(|p| name.starts_with(p))
        || SECRET_NAME_PARTS.iter().any(|p| name.contains(p))
        || dirs.iter().any(|d| SECRET_DIRS.contains(d))
}

#[allow(clippy::case_sensitive_file_extension_comparisons)] // `name` is folded to lower case
fn ssh_private_key(dirs: &[&str], name: &str) -> bool {
    dirs.contains(&".ssh")
        && !name.ends_with(".pub")
        && name != "config"
        && !name.starts_with("known_hosts")
        && !name.starts_with("authorized_keys")
}

fn credential_store(folded: &str) -> bool {
    let at_boundary = |rel: &str| {
        folded == rel
            || folded
                .strip_suffix(rel)
                .is_some_and(|head| head.ends_with('/'))
    };
    CREDENTIAL_STORES.iter().any(|s| at_boundary(s))
        || CREDENTIAL_DIRS.iter().any(|d| {
            at_boundary(d)
                || folded.starts_with(&format!("{d}/"))
                || folded.contains(&format!("/{d}/"))
        })
}

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn dir(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

/// Go's `path.Clean`: one `/` between elements, no `.` element, `..` collapsed into its parent
/// (and dropped at the root), `.` for an empty result.
fn clean(path: &str) -> String {
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            ".." if rooted => {}
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_owned(),
        (false, false) => joined,
    }
}

/// The form a case-insensitive file system (macOS's APFS: full Unicode case folding) compares,
/// as the guard computes it: the folds that expand to ASCII, then upper case, then lower case.
fn fold(s: &str) -> String {
    const EXPANDING: &[(char, &str)] = &[
        ('ß', "ss"),
        ('ẞ', "ss"),
        ('ﬀ', "ff"),
        ('ﬁ', "fi"),
        ('ﬂ', "fl"),
        ('ﬃ', "ffi"),
        ('ﬄ', "ffl"),
        ('ﬅ', "st"),
        ('ﬆ', "st"),
    ];
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match EXPANDING.iter().find(|(from, _)| *from == c) {
            Some((_, to)) => out.push_str(to),
            // Go lower-cases U+0130 to a plain `i`; Rust would add a combining dot.
            None if c == 'İ' => out.push('i'),
            None => out.extend(c.to_uppercase().flat_map(char::to_lowercase)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::is_sensitive_path;

    /// my-dotfiles' `TestSecretNames` spelled table, row for row, less the guard's own policy-file
    /// rows, with one host-specific path made neutral; then ME's own rows.
    const SPELLED: &[(&str, bool)] = &[
        (".env", true),
        (".env.local", true),
        (".env.test", true),
        (".ENV", true),
        ("a/b/.env.production", true),
        ("host.env", true),
        ("mcp.env", true),
        ("/etc/app/host.env", true),
        (".env.example", false),
        (".env.sample", false),
        (".env.template", false),
        ("host.env.example", false),
        ("id_rsa", true),
        ("id_ed25519", true),
        ("id_ecdsa", true),
        ("id_dsa", true),
        ("k.pem", true),
        ("tls.key", true),
        ("a.p12", true),
        ("a.pfx", true),
        ("a.jks", true),
        ("a.keystore", true),
        ("ca.crt", true),
        ("~/.ssh/config", false),
        ("~/.ssh/known_hosts", false),
        ("~/.ssh/id_rsa.pub", false),
        ("authorized_keys", false),
        ("secrets.json", true),
        ("credentials.json", true),
        (".credentials.json", true),
        (".netrc", true),
        (".npmrc", true),
        (".pypirc", true),
        (".htpasswd", true),
        (".git-credentials", true),
        ("aws_credentials", true),
        ("gcloud_credentials.json", true),
        ("service_account.json", true),
        ("keystore.jks", true),
        ("keystore.p12", true),
        ("/etc/passwd", false),
        ("~/.codex/auth.json", true),
        ("~/.claude/.credentials.json", true),
        ("~/.claude.json", true),
        ("~/.config/opencode/auth.json", true),
        ("~/.local/share/opencode/auth.json", true),
        ("~/.aws/credentials", true),
        ("~/.config/gh/hosts.yml", true),
        (
            "~/.config/gcloud/application_default_credentials.json",
            true,
        ),
        ("~/.config/ai-provider-secrets/openai", true),
        ("~/.gemini/antigravity-cli/antigravity-oauth-token", true),
        ("~/.config/memory-engine/secrets/token", true),
        ("~/.config/memory-engine/secrets", true),
        (
            "~/.config/memory-engine/test-secrets/claude-oauth-token",
            true,
        ),
        ("~/.config/memory-engine/config.toml", false),
        ("~/.config/memory-engine/secrets.bak", false),
        ("~/.config/memory-engine/ſecrets/token", true),
        ("~/.ssh/id_rſa", true),
        (".env.ſample", true),
        (
            "~/.conﬁg/memory-engine/test-secrets/claude-oauth-token",
            true,
        ),
        ("~/.pgpaß", true),
        ("~/.config/gh/hoﬆs.yml", true),
        ("~/.docker/config.json", true),
        ("~/.kube/config", true),
        ("token.json", true),
        ("secret.yaml", true),
        ("credentials/x.json", true),
        ("api_key.txt", true),
        ("apikey", true),
        ("private_key", true),
        ("vault/prod.toml", true),
        ("secrets.py", false),
        ("token.ts", false),
        ("test_web_api_key.py", false),
        ("credentials/__init__.py", false),
        ("src/secret.go", false),
        ("auth.json", false),
        ("~/.local/share/opencode/log.txt", false),
        ("process.env", true),
        // ME's own rows: no home is known, the persistence-side additions, folding.
        ("/srv/x/.kube/config", true),
        (".aws/credentials", true),
        ("~/.ssh/github", true),
        ("/home/u/.ssh/deploy_key", true),
        ("/etc/shadow", true),
        ("/etc/gshadow", true),
        ("İd_rsa", true),
        ("secrets.json.example", false),
        ("a/./b/../.env", true),
        ("C:\\work\\.env", true),
    ];

    #[test]
    fn the_command_guard_name_table() {
        for &(path, want) in SPELLED {
            assert_eq!(is_sensitive_path(path), want, "{path}");
            if let Some(rest) = path.strip_prefix("~/") {
                assert_eq!(
                    is_sensitive_path(&format!("/home/u/{rest}")),
                    want,
                    "absolute {path}"
                );
                assert_eq!(is_sensitive_path(rest), want, "relative {path}");
            }
        }
    }
}
