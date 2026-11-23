//! #1092: every method signature `docs/reference/api.md` documents MUST be the code's.
//!
//! No rustdoc is hosted: the page *is* the public API surface, a mirror of the
//! facade's source. A signature drift between the page and the code is a silent
//! contract break — exactly what this guard fails on, as `gate_matrix_parity`
//! does for the gate matrix. It runs inside `cargo test --workspace
//! --all-features`, so it cannot be skipped by anyone who runs the gate.
//!
//! Both sides are parsed with `syn` (not string-scraped): the page's `## Public
//! Methods by Category` tables yield rows, every `pub fn` in the facade's source
//! (free functions plus `impl` methods, recursively into inline modules) yields a
//! candidate, and the two are compared after normalization — qualified paths
//! reduced to their last segment, a trailing comma dropped, `mut` dropped from by-value bindings and from
//! a bare `self` receiver (so `mut self` reads as `self`; `&mut self` keeps its
//! `&mut`).

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;

use quote::ToTokens;
use syn::visit_mut::VisitMut;

/// This test's crate manifest is at `<repo>/crates/memory-engine`; the docs and
/// the facade's source it checks live two levels up.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root resolves from CARGO_MANIFEST_DIR")
}

/// One row of the page's `Public Methods by Category` tables.
struct Row {
    owner: Option<String>,
    name: String,
    signature: String,
}

/// Type rows are documented, not callable: their "signature" is a struct or
/// enum literal, not a function signature, so the parity check skips them.
const TYPE_ROWS: &[&str] = &["Backoff", "Settled"];

/// A table row whose cells are all `-`, `:` and spaces — the `| --- |` line.
fn is_separator(line: &str) -> bool {
    let cells: Vec<&str> = line
        .split('|')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    !cells.is_empty()
        && cells
            .iter()
            .all(|c| c.chars().all(|c| c == '-' || c == ':' || c == ' '))
}

/// Extract the text between the first two backticks of `cell`.
fn code_span(cell: &str) -> Option<&str> {
    let (_, rest) = cell.split_once('`')?;
    let (span, _) = rest.split_once('`')?;
    Some(span)
}

/// The page's rows: the table lines between `## Public Methods by Category` and
/// the next `## ` heading. A header row (its next line is a separator) is
/// skipped; a separator row is skipped. Cell 1 is the name (`` `name` `` or
/// `` `Type::name` `` — split on the last `::` into owner and name), cell 2 the
/// signature. A row without a code span in either cell is an error naming the
/// line.
fn rows(page: &str) -> Result<Vec<Row>, String> {
    let lines: Vec<&str> = page.lines().collect();
    let start = lines
        .iter()
        .position(|l| *l == "## Public Methods by Category")
        .ok_or("api.md has no '## Public Methods by Category' section")?;
    let mut rows = Vec::new();
    for (i, line) in lines[start + 1..].iter().enumerate() {
        if line.starts_with("## ") {
            break;
        }
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        if is_separator(line) {
            continue;
        }
        let next = lines.get(start + 1 + i + 1).map(|l| l.trim());
        if next.is_some_and(is_separator) {
            continue; // header row
        }
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        let name_cell = cells
            .get(1)
            .ok_or_else(|| format!("line {}: table row without a name cell", start + 1 + i))?;
        let sig_cell = cells
            .get(2)
            .ok_or_else(|| format!("line {}: table row without a signature cell", start + 1 + i))?;
        let name = code_span(name_cell)
            .ok_or_else(|| format!("line {}: no code span in the name cell", start + 1 + i))?;
        let signature = code_span(sig_cell)
            .ok_or_else(|| format!("line {}: no code span in the signature cell", start + 1 + i))?;
        let (owner, name) = match name.rsplit_once("::") {
            Some((owner, name)) => (Some(owner.to_string()), name),
            None => (None, name),
        };
        if TYPE_ROWS.contains(&name) {
            continue;
        }
        rows.push(Row {
            owner,
            name: name.to_string(),
            signature: signature.to_string(),
        });
    }
    Ok(rows)
}

/// Rebuild a full `fn` signature from the page's `(args) -> Ret` form and parse
/// it. The page writes `async` methods as `async (…) -> …`; the `async` prefix
/// is stripped here and the `fn` keyword inserted.
fn parse_row(row: &Row) -> Result<syn::Signature, String> {
    let text = row.signature.strip_prefix("async ").map_or_else(
        || format!("fn {} {}", row.name, row.signature),
        |rest| format!("async fn {} {rest}", row.name),
    );
    syn::parse_str::<syn::Signature>(&text).map_err(|e| {
        format!(
            "row `{}` is unreadable: {}: text was `{}`",
            row.name, e, text
        )
    })
}

/// A `pub fn` from the facade's source: a free function (owner `None`) or an
/// `impl` method (owner = the impl's type, last path segment, no generics).
struct Candidate {
    owner: Option<String>,
    file: String,
    sig: syn::Signature,
}

/// The impl's `self_ty`'s last path segment ident without generics (`impl<B:
/// Backing> MemoryEngineBuilder<B>` -> `MemoryEngineBuilder`).
fn impl_owner(impl_: &syn::ItemImpl) -> Option<String> {
    let syn::Type::Path(tp) = impl_.self_ty.as_ref() else {
        return None;
    };
    tp.path.segments.last().map(|seg| seg.ident.to_string())
}

fn push_pub_fn(
    out: &mut Vec<Candidate>,
    owner: Option<String>,
    file: &str,
    vis: &syn::Visibility,
    sig: &syn::Signature,
) {
    if matches!(vis, syn::Visibility::Public(_)) {
        out.push(Candidate {
            owner,
            file: file.to_string(),
            sig: sig.clone(),
        });
    }
}

fn collect(out: &mut Vec<Candidate>, file: &str, items: &mut dyn Iterator<Item = &syn::Item>) {
    for item in items {
        match item {
            syn::Item::Fn(fn_) => push_pub_fn(out, None, file, &fn_.vis, &fn_.sig),
            syn::Item::Impl(impl_) => {
                let owner = impl_owner(impl_);
                for impl_item in &impl_.items {
                    if let syn::ImplItem::Fn(fn_) = impl_item {
                        push_pub_fn(out, owner.clone(), file, &fn_.vis, &fn_.sig);
                    }
                }
            }
            syn::Item::Mod(mod_) => {
                // A `#[cfg(test)]` module is dead code, not API: skip it.
                if mod_.attrs.iter().any(|a| {
                    a.path().is_ident("cfg")
                        && a.parse_args::<syn::Ident>().is_ok_and(|arg| arg == "test")
                }) {
                    continue;
                }
                if let Some((_, nested)) = &mod_.content {
                    collect(out, file, &mut nested.iter());
                }
            }
            _ => {}
        }
    }
}

fn candidates(file: &syn::File, path: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    collect(&mut out, path, &mut file.items.iter());
    out
}

/// Reduce a path to its last segment, so `Result<Fact>` and
/// `me_types::Fact` compare equal.
struct Normalize;

impl VisitMut for Normalize {
    fn visit_path_mut(&mut self, i: &mut syn::Path) {
        let last = i.segments.last().cloned();
        i.leading_colon = None;
        i.segments.clear();
        if let Some(last) = last {
            i.segments.push(last);
        }
        syn::visit_mut::visit_path_mut(self, i);
    }

    /// `mut self` is `self`; a by-value binding's `mut` is dropped.
    fn visit_pat_ident_mut(&mut self, i: &mut syn::PatIdent) {
        i.mutability = None;
        syn::visit_mut::visit_pat_ident_mut(self, i);
    }

    /// A bare `self` receiver's `mut` is dropped; `&mut self` keeps its `&mut`.
    fn visit_receiver_mut(&mut self, i: &mut syn::Receiver) {
        if i.reference.is_none() {
            i.mutability = None;
        }
        syn::visit_mut::visit_receiver_mut(self, i);
    }
}

fn normalized(sig: &syn::Signature) -> String {
    let mut sig = sig.clone();
    sig.constness = None;
    // rustfmt's trailing comma after a wrapped parameter list.
    if sig.inputs.trailing_punct() {
        sig.inputs.pop_punct();
    }
    Normalize.visit_signature_mut(&mut sig);
    sig.to_token_stream().to_string()
}

fn check(row: &Row, all: &[Candidate]) -> Result<(), String> {
    let name = row.name.as_str();
    let cands: Vec<&Candidate> = all
        .iter()
        .filter(|c| c.sig.ident == name)
        .filter(|c| {
            row.owner.as_deref().map_or_else(
                || c.owner.is_none() || c.owner.as_deref() == Some("MemoryEngine"),
                |owner| c.owner.as_deref() == Some(owner),
            )
        })
        .collect();
    if cands.is_empty() {
        return Err(format!("no such fn: `{}`", row.name));
    }
    if row.owner.is_none() {
        let distinct: std::collections::BTreeSet<String> =
            cands.iter().map(|c| normalized(&c.sig)).collect();
        if distinct.len() > 1 {
            let owners: Vec<String> = cands
                .iter()
                .map(|c| c.owner.clone().unwrap_or_else(|| "free fn".to_string()))
                .collect();
            return Err(format!(
                "`{}` is ambiguous — qualify it: {}",
                row.name,
                owners.join(", ")
            ));
        }
    }
    let row_norm = normalized(&parse_row(row)?);
    for c in &cands {
        if normalized(&c.sig) == row_norm {
            return Ok(());
        }
    }
    Err(format!(
        "`{}`: page says `{}` but the code says `{}` (in {})",
        row.name,
        row_norm,
        normalized(&cands[0].sig),
        cands[0].file
    ))
}

fn parse_src_dir(root: &std::path::Path) -> Result<Vec<(syn::File, String)>, String> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).map_err(|e| format!("reading {}: {}", dir.display(), e))?;
        for entry in entries {
            let path = entry
                .map_err(|e| format!("reading entry in {}: {}", dir.display(), e))?
                .path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let src = fs::read_to_string(&path)
                    .map_err(|e| format!("reading {}: {}", path.display(), e))?;
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                let file =
                    syn::parse_file(&src).map_err(|e| format!("{rel} is not parseable: {e}"))?;
                files.push((file, rel));
            }
        }
    }
    Ok(files)
}

#[test]
fn every_documented_signature_is_the_codes() {
    let root = repo_root();
    let page = fs::read_to_string(root.join("docs/reference/api.md"))
        .expect("docs/reference/api.md exists");
    let src_dir = root.join("crates/memory-engine/src");
    let files =
        parse_src_dir(&src_dir).expect("every .rs file under crates/memory-engine/src parses");
    let mut all = Vec::new();
    for (file, rel) in &files {
        all.extend(candidates(file, rel));
    }
    let rows = rows(&page).expect("api.md rows parse");
    assert!(!rows.is_empty(), "no rows found — the parser broke");
    let errors: Vec<String> = rows
        .iter()
        .filter_map(|row| check(row, &all).err())
        .collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

fn guard(src: &str) -> Vec<Candidate> {
    let file = syn::parse_file(src).expect("guard input parses");
    candidates(&file, "inline")
}

#[test]
fn an_async_or_return_type_mismatch_fails() {
    let code = guard("pub async fn f(&self) -> u8 { loop {} }");
    // Return type u8 vs u16: fails.
    let row = Row {
        owner: None,
        name: "f".to_string(),
        signature: "(&self) -> u16".to_string(),
    };
    assert!(check(&row, &code).is_err(), "u8 vs u16 must fail");
    // Page drops the asyncness: fails.
    let row = Row {
        owner: None,
        name: "f".to_string(),
        signature: "(&self) -> u8".to_string(),
    };
    assert!(check(&row, &code).is_err(), "async vs sync must fail");
    // Page declares async: passes.
    let row = Row {
        owner: None,
        name: "f".to_string(),
        signature: "async (&self) -> u8".to_string(),
    };
    assert!(check(&row, &code).is_ok(), "async (&self) -> u8 must pass");
}

#[test]
fn mut_self_is_self_but_mut_ref_is_not_ref() {
    let code = guard(
        "pub fn f(mut self) -> u8 { 0 }
         pub fn g(&mut self) -> u8 { 0 }",
    );
    // `mut self` reads as `self`.
    let row = Row {
        owner: None,
        name: "f".to_string(),
        signature: "(self) -> u8".to_string(),
    };
    assert!(check(&row, &code).is_ok(), "mut self must match self");
    // `&mut self` is not `&self`.
    let row = Row {
        owner: None,
        name: "g".to_string(),
        signature: "(&self) -> u8".to_string(),
    };
    assert!(
        check(&row, &code).is_err(),
        "&mut self must not match &self"
    );
}

#[test]
fn a_qualified_row_matches_only_its_type() {
    let code = guard(
        "impl A { pub fn path(&self) -> u8 { 0 } }
         impl B { pub fn path(&self) -> u16 { 0 } }",
    );
    let row = Row {
        owner: Some("A".to_string()),
        name: "path".to_string(),
        signature: "(&self) -> u8".to_string(),
    };
    assert!(check(&row, &code).is_ok(), "A::path must match A");
    let row = Row {
        owner: Some("A".to_string()),
        name: "path".to_string(),
        signature: "(&self) -> u16".to_string(),
    };
    assert!(check(&row, &code).is_err(), "A::path must not match B");
}

#[test]
fn rows_skip_headers_separators_and_type_rows() {
    let page = "\
## Public Methods by Category

| Method | Signature | Description |
| ------ | --------- | ----------- |
| `get_fact` | `async (&self, id: i64) -> Result<Fact>` | Read one. |
| `StoreLock::path` | `(&self) -> &Path` (any lock) | Its path. |
| `Backoff` | `Backoff { base, cap }` | A type row. |

## Next
| `after` | `(&self)` | Outside the section. |
";
    let rows = rows(page).unwrap();
    let read: Vec<(Option<&str>, &str, &str)> = rows
        .iter()
        .map(|r| (r.owner.as_deref(), r.name.as_str(), r.signature.as_str()))
        .collect();
    assert_eq!(
        read,
        [
            (None, "get_fact", "async (&self, id: i64) -> Result<Fact>"),
            (Some("StoreLock"), "path", "(&self) -> &Path"),
        ]
    );
}

#[test]
fn a_wrapped_signature_equals_its_one_line_form() {
    let code = guard(
        "pub fn f(
            &self,
            a: std::sync::Arc<dyn Send>,
        ) -> crate::Result<u8> { 0 }",
    );
    let row = Row {
        owner: None,
        name: "f".to_string(),
        signature: "(&self, a: Arc<dyn Send>) -> Result<u8>".to_string(),
    };
    assert!(check(&row, &code).is_ok(), "{:?}", check(&row, &code));
}
