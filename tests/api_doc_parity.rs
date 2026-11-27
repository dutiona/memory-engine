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
//!
//! #1160 adds two checks of the same kind. Every `trait X` and `impl X for T` in a
//! guide's `rust` fence (README.md and `docs/`, less the dated records) must show
//! X's own signatures, parameter names aside, and an `impl` every method X has no
//! default for. Every `` `X` fields`` list in api.md must name exactly the public
//! fields of the `pub struct X`, typed where a bullet gives a type. Both fail
//! closed: an unreadable fence, an unknown trait, an unreadable list line or a run
//! that checked nothing is an error, never a skip.

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

/// Std and external traits a guide may implement: their signatures are not this
/// repository's to hold, so a fence naming one is not checked (F3's only exemption).
const FOREIGN_TRAITS: &[&str] = &[];

/// `Result<T, MemoryError>` is the crate alias `Result<T>`, spelled out.
struct ResultAlias;

impl VisitMut for ResultAlias {
    fn visit_path_segment_mut(&mut self, seg: &mut syn::PathSegment) {
        if seg.ident == "Result"
            && let syn::PathArguments::AngleBracketed(args) = &mut seg.arguments
            && args.args.len() == 2
            && args
                .args
                .last()
                .is_some_and(|a| a.to_token_stream().to_string() == "MemoryError")
        {
            args.args.pop();
            if args.args.trailing_punct() {
                args.args.pop_punct();
            }
        }
        syn::visit_mut::visit_path_segment_mut(self, seg);
    }
}

/// A signature as compared with a guide's: `normalized`, the alias spelled
/// short, and every parameter's pattern erased (an impl may rename it).
fn shape(sig: &syn::Signature) -> String {
    let mut sig = sig.clone();
    for arg in &mut sig.inputs {
        if let syn::FnArg::Typed(typed) = arg {
            *typed.pat = syn::parse_quote!(_);
        }
    }
    Normalize.visit_signature_mut(&mut sig);
    ResultAlias.visit_signature_mut(&mut sig);
    normalized(&sig)
}

/// A type as compared with a field list's: last segments, the alias short.
fn type_shape(ty: &syn::Type) -> String {
    let mut ty = ty.clone();
    Normalize.visit_type_mut(&mut ty);
    ResultAlias.visit_type_mut(&mut ty);
    ty.to_token_stream().to_string()
}

/// Every `pub trait` and `pub struct` of the repository's crates, by name, outside
/// `#[cfg(test)]` modules: the code side of both #1160 checks.
#[derive(Default)]
struct Code {
    traits: std::collections::BTreeMap<String, Vec<(String, syn::ItemTrait)>>,
    structs: std::collections::BTreeMap<String, Vec<(String, syn::ItemStruct)>>,
}

impl Code {
    fn add(&mut self, file: &str, items: &[syn::Item]) {
        for item in items {
            match item {
                syn::Item::Trait(t) if matches!(t.vis, syn::Visibility::Public(_)) => self
                    .traits
                    .entry(t.ident.to_string())
                    .or_default()
                    .push((file.to_string(), t.clone())),
                syn::Item::Struct(s) if matches!(s.vis, syn::Visibility::Public(_)) => self
                    .structs
                    .entry(s.ident.to_string())
                    .or_default()
                    .push((file.to_string(), s.clone())),
                syn::Item::Mod(m) if !is_test_mod(m) => {
                    if let Some((_, nested)) = &m.content {
                        self.add(file, nested);
                    }
                }
                _ => {}
            }
        }
    }

    fn read(root: &std::path::Path) -> Self {
        let mut code = Self::default();
        let mut crates: Vec<PathBuf> = fs::read_dir(root.join("crates"))
            .unwrap()
            .map(|e| e.unwrap().path().join("src"))
            .filter(|p| p.is_dir())
            .collect();
        crates.sort();
        for src in crates {
            let files = parse_src_dir(&src).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
            for (file, rel) in files {
                code.add(&format!("{}/{rel}", src.display()), &file.items);
            }
        }
        code
    }

    fn one<'c, T>(
        found: Option<&'c Vec<(String, T)>>,
        kind: &str,
        name: &str,
    ) -> Result<Option<&'c T>, String> {
        match found.map(Vec::as_slice) {
            None | Some([]) => Ok(None),
            Some([(_, item)]) => Ok(Some(item)),
            Some(many) => Err(format!(
                "`{name}` is ambiguous: a pub {kind} in {}",
                many.iter()
                    .map(|(f, _)| f.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

fn is_test_mod(m: &syn::ItemMod) -> bool {
    m.attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<syn::Ident>().is_ok_and(|arg| arg == "test")
    })
}

/// The `trait` and `impl … for` items anywhere in a parsed fence.
#[derive(Default)]
struct FenceItems {
    traits: Vec<syn::ItemTrait>,
    impls: Vec<(String, syn::ItemImpl)>,
}

impl VisitMut for FenceItems {
    fn visit_item_trait_mut(&mut self, t: &mut syn::ItemTrait) {
        self.traits.push(t.clone());
    }

    fn visit_item_impl_mut(&mut self, i: &mut syn::ItemImpl) {
        if let Some((_, path, _)) = &i.trait_
            && let Some(last) = path.segments.last()
        {
            self.impls.push((last.ident.to_string(), i.clone()));
        }
    }
}

/// A fence's text with rustdoc's hidden lines (`# x`, a lone `#`) shown.
fn unhide(body: &str) -> String {
    body.lines()
        .map(|l| {
            let t = l.trim_start();
            if t == "#" {
                ""
            } else if let Some(rest) = t.strip_prefix("# ") {
                rest
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every ` ```rust… ` fence of `page`: (line of its opening, its body).
/// An unclosed fence at end of file is an error, not a silent drop.
fn rust_fences(page: &str) -> Result<Vec<(usize, String)>, String> {
    let mut out = Vec::new();
    let mut open: Option<(usize, Vec<&str>)> = None;
    for (i, line) in page.lines().enumerate() {
        let t = line.trim_start();
        match &mut open {
            None if t.starts_with("```rust") => open = Some((i + 1, Vec::new())),
            None => {}
            Some((at, body)) if t.starts_with("```") => {
                out.push((*at, body.join("\n")));
                open = None;
            }
            Some((_, body)) => body.push(line),
        }
    }
    if let Some((at, _)) = open {
        return Err(format!("line {at}: a ```rust fence is never closed"));
    }
    Ok(out)
}

/// Whether a line of `text` opens a trait or an impl, visibility and `unsafe` aside.
fn could_hold_a_signature(text: &str) -> bool {
    text.lines().any(|l| {
        let mut t = l.trim_start();
        while let Some(rest) = ["pub(crate) ", "pub ", "unsafe "]
            .iter()
            .find_map(|p| t.strip_prefix(p))
        {
            t = rest;
        }
        t.starts_with("trait ") || t.starts_with("impl")
    })
}

/// One fence against the code: the errors it holds and how many fns it checked.
fn check_fence(code: &Code, at: &str, body: &str) -> (Vec<String>, usize) {
    let text = unhide(body);
    let mut items = FenceItems::default();
    if let Ok(mut file) = syn::parse_str::<syn::File>(&text) {
        items.visit_file_mut(&mut file);
    } else if let Ok(mut block) = syn::parse_str::<syn::Block>(&format!("{{\n{text}\n}}")) {
        items.visit_block_mut(&mut block);
    } else {
        // F2: a fence that could hold a signature is never skipped.
        return if could_hold_a_signature(&text) {
            (
                vec![format!(
                    "{at}: a fence holding a trait or impl does not parse"
                )],
                0,
            )
        } else {
            (Vec::new(), 0)
        };
    }
    let mut errors = Vec::new();
    let mut checked = 0;
    let shown = items
        .traits
        .iter()
        .map(|t| {
            let fns: Vec<&syn::Signature> = t
                .items
                .iter()
                .filter_map(|i| match i {
                    syn::TraitItem::Fn(f) => Some(&f.sig),
                    _ => None,
                })
                .collect();
            (t.ident.to_string(), fns, None)
        })
        .chain(items.impls.iter().map(|(name, i)| {
            let fns: Vec<&syn::Signature> = i
                .items
                .iter()
                .filter_map(|it| match it {
                    syn::ImplItem::Fn(f) => Some(&f.sig),
                    _ => None,
                })
                .collect();
            (name.clone(), fns, Some(()))
        }));
    for (name, fns, is_impl) in shown {
        if FOREIGN_TRAITS.contains(&name.as_str()) {
            continue;
        }
        let real = match Code::one(code.traits.get(&name), "trait", &name) {
            Ok(Some(real)) => real,
            // F3: an unknown trait is a doc bug, not a skip.
            Ok(None) => {
                errors.push(format!("{at}: `{name}` is no pub trait of the workspace"));
                continue;
            }
            Err(e) => {
                errors.push(format!("{at}: {e}"));
                continue;
            }
        };
        let real_fns: Vec<&syn::TraitItemFn> = real
            .items
            .iter()
            .filter_map(|i| match i {
                syn::TraitItem::Fn(f) => Some(f),
                _ => None,
            })
            .collect();
        for sig in &fns {
            checked += 1;
            match real_fns.iter().find(|f| f.sig.ident == sig.ident) {
                None => errors.push(format!("{at}: `{name}` has no fn `{}`", sig.ident)),
                Some(f) if shape(&f.sig) != shape(sig) => errors.push(format!(
                    "{at}: `{name}::{}`: the guide says `{}` but the code says `{}`",
                    sig.ident,
                    shape(sig),
                    shape(&f.sig)
                )),
                Some(_) => {}
            }
        }
        // F4: an impl that lacks a required fn does not compile against the trait.
        if is_impl.is_some() {
            for f in real_fns.iter().filter(|f| f.default.is_none()) {
                if !fns.iter().any(|s| s.ident == f.sig.ident) {
                    errors.push(format!(
                        "{at}: this `impl {name}` lacks its required fn `{}`",
                        f.sig.ident
                    ));
                }
            }
        }
    }
    (errors, checked)
}

/// The guides: README.md and `docs/**/*.md`, less the dated records (`design/`,
/// `audits/`, `ROADMAP.md`), the working plans and the generated site.
fn guides(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("README.md")];
    let mut stack = vec![root.join("docs")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root.join("docs")).unwrap();
            let first = rel.components().next().unwrap().as_os_str();
            if ["design", "audits", "plans", "_build", "ROADMAP.md"]
                .iter()
                .any(|skip| first == *skip)
            {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_guide_trait_fence_is_the_codes() {
    let root = repo_root();
    let code = Code::read(&root);
    let mut errors = Vec::new();
    let mut checked = 0;
    for page in guides(&root) {
        let text = fs::read_to_string(&page).unwrap();
        let rel = page.strip_prefix(&root).unwrap().display().to_string();
        match rust_fences(&text) {
            Ok(fences) => {
                for (line, body) in fences {
                    let (e, n) = check_fence(&code, &format!("{rel}:{line}"), &body);
                    errors.extend(e);
                    checked += n;
                }
            }
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
    }
    // F1: a reader that matched nothing is broken, not green.
    assert!(
        checked > 0,
        "no guide trait fn was checked: the fence reader broke"
    );
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// One api.md field list: its header line, its struct and its bullets' fields.
struct FieldList {
    line: usize,
    name: String,
    fields: Vec<(String, Option<String>)>,
}

/// The page's `` `X` fields`` lists. F5: a line that names fields in any other
/// form is an error, so a list can never escape the check by its spelling.
fn field_lists(page: &str) -> Result<Vec<FieldList>, Vec<String>> {
    let lines: Vec<&str> = page.lines().collect();
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let header = line
            .strip_prefix('`')
            .and_then(|rest| rest.split_once('`'))
            .filter(|(_, after)| after.starts_with(" fields"))
            .map(|(name, _)| name.to_string());
        let Some(name) = header else {
            if line.contains("` fields") || line.contains("Fields: `") {
                errors.push(format!(
                    "line {}: fields named outside a list header",
                    i + 1
                ));
            }
            i += 1;
            continue;
        };
        let at = i + 1;
        i += 1;
        while i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
        let mut bullets: Vec<(usize, String)> = Vec::new();
        while i < lines.len() {
            let l = lines[i];
            if let Some(b) = l.strip_prefix("- ") {
                bullets.push((i + 1, b.to_string()));
            } else if l.starts_with(char::is_whitespace) && !l.trim().is_empty() {
                if let Some((_, last)) = bullets.last_mut() {
                    last.push(' ');
                    last.push_str(l.trim());
                }
            } else {
                break;
            }
            i += 1;
        }
        let mut fields = Vec::new();
        let unread = errors.len();
        for (bl, bullet) in &bullets {
            let Some((head, _)) = bullet.split_once(" -- ") else {
                errors.push(format!("line {bl}: a `{name}` bullet without ` -- `"));
                continue;
            };
            let spans: Vec<&str> = head.split('`').skip(1).step_by(2).collect();
            if spans.is_empty() {
                errors.push(format!("line {bl}: a `{name}` bullet names no field"));
            }
            for span in spans {
                match span.split_once(':') {
                    Some((n, ty)) => {
                        fields.push((n.trim().to_string(), Some(ty.trim().to_string())));
                    }
                    None => fields.push((span.trim().to_string(), None)),
                }
            }
        }
        if fields.is_empty() && errors.len() == unread {
            errors.push(format!("line {at}: the `{name}` list has no field"));
        }
        out.push(FieldList {
            line: at,
            name,
            fields,
        });
    }
    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

/// One list against its struct: exactly its `pub` named fields, typed alike.
fn check_list(code: &Code, list: &FieldList) -> Vec<String> {
    let at = format!("api.md:{}", list.line);
    let item = match Code::one(code.structs.get(&list.name), "struct", &list.name) {
        Ok(Some(item)) => item,
        Ok(None) => {
            return vec![format!(
                "{at}: `{}` is no pub struct of the workspace",
                list.name
            )];
        }
        Err(e) => return vec![format!("{at}: {e}")],
    };
    let syn::Fields::Named(named) = &item.fields else {
        return vec![format!("{at}: `{}` has no named fields", list.name)];
    };
    let public: Vec<(String, &syn::Type)> = named
        .named
        .iter()
        .filter(|f| matches!(f.vis, syn::Visibility::Public(_)))
        .map(|f| (f.ident.as_ref().unwrap().to_string(), &f.ty))
        .collect();
    let mut errors = Vec::new();
    for (name, ty) in &list.fields {
        match public.iter().find(|(n, _)| n == name) {
            None => errors.push(format!("{at}: `{}` has no pub field `{name}`", list.name)),
            Some((_, real)) => {
                if let Some(ty) = ty {
                    let given = syn::parse_str::<syn::Type>(ty)
                        .map_or_else(|_| format!("unreadable `{ty}`"), |t| type_shape(&t));
                    if given != type_shape(real) {
                        errors.push(format!(
                            "{at}: `{}::{name}`: the page says `{given}` but the code says `{}`",
                            list.name,
                            type_shape(real)
                        ));
                    }
                }
            }
        }
    }
    for (name, _) in &public {
        if !list.fields.iter().any(|(n, _)| n == name) {
            errors.push(format!(
                "{at}: the `{}` list omits its pub field `{name}`",
                list.name
            ));
        }
    }
    errors
}

#[test]
fn every_api_field_list_is_the_codes() {
    let root = repo_root();
    let code = Code::read(&root);
    let page = fs::read_to_string(root.join("docs/reference/api.md")).unwrap();
    let lists = field_lists(&page).unwrap_or_else(|e| panic!("{}", e.join("\n")));
    assert!(
        !lists.is_empty(),
        "no field list found: the list reader broke"
    );
    let errors: Vec<String> = lists.iter().flat_map(|l| check_list(&code, l)).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

fn inline_code(src: &str) -> Code {
    let mut code = Code::default();
    code.add("inline", &syn::parse_file(src).unwrap().items);
    code
}

#[test]
fn a_trait_fence_mismatch_fails() {
    let code = inline_code(
        "pub trait T {
            fn need(&self, items: &[u8]) -> Result<String>;
            fn optional(&self) -> bool { false }
        }",
    );
    let check = |fence: &str| check_fence(&code, "inline", fence).0;
    // Renamed and `_`-prefixed parameters, the spelled-out alias, an omitted
    // default fn and a hidden line all pass.
    assert_eq!(
        check("trait T { fn need(&self, other: &[u8]) -> Result<String>; }"),
        Vec::<String>::new()
    );
    assert_eq!(
        check(
            "# struct S;\nimpl T for S {\n    fn need(&self, _x: &[u8]) -> Result<String, MemoryError> { todo!() }\n}"
        ),
        Vec::<String>::new()
    );
    // A changed parameter type, an unknown fn, a missing required fn all fail.
    assert_eq!(
        check("trait T { fn need(&self, items: &[i8]) -> Result<String>; }").len(),
        1
    );
    assert_eq!(check("trait T { fn other(&self); }").len(), 1);
    assert_eq!(
        check("struct S;\nimpl T for S { fn optional(&self) -> bool { true } }").len(),
        1
    );
}

#[test]
fn an_unreadable_or_unknown_trait_fence_fails() {
    let code = inline_code("pub trait T { fn f(&self); }");
    let check = |fence: &str| check_fence(&code, "inline", fence).0;
    assert_eq!(
        check("impl T for {").len(),
        1,
        "an unreadable impl fence fails"
    );
    assert_eq!(
        check("  unsafe impl T for {").len(),
        1,
        "so does an indented unsafe one"
    );
    assert!(
        check("let x = ;").is_empty(),
        "an unreadable fence with no trait is no signature"
    );
    assert_eq!(
        check("trait Gone { fn f(&self); }").len(),
        1,
        "an unknown trait fails"
    );
    assert_eq!(
        check("fn main() {\n    struct S;\n    impl T for S {}\n}").len(),
        1
    );
}

#[test]
fn an_unterminated_rust_fence_fails() {
    let page = "```rust\ntrait A { fn f(&self); }\n```\ntext\n```rust\ntrait B { fn g(&self); }\n";
    let err = rust_fences(page).unwrap_err();
    assert!(
        err.contains("line 5"),
        "the error names the unclosed fence's opening line: {err}"
    );
    let closed = "```rust\ntrait A { fn f(&self); }\n```\n";
    let fences = rust_fences(closed).unwrap();
    assert_eq!(fences.len(), 1);
}

#[test]
fn a_field_list_mismatch_fails() {
    let code =
        inline_code("pub struct C { pub a: Option<String>, pub b: usize, pub(crate) hidden: u8 }");
    let check = |page: &str| -> Vec<String> {
        match field_lists(page) {
            Ok(lists) => lists.iter().flat_map(|l| check_list(&code, l)).collect(),
            Err(e) => e,
        }
    };
    assert_eq!(
        check("`C` fields:\n\n- `a: Option<String>` -- a.\n- `b` -- b.\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        check("`C` fields:\n\n- `a: Option<String>` -- a.\n").len(),
        1,
        "missing"
    );
    assert_eq!(
        check("`C` fields:\n\n- `a`, `b`, `hidden` -- all.\n").len(),
        1,
        "pub(crate) is not listed"
    );
    assert_eq!(
        check("`C` fields:\n\n- `a: String`, `b` -- typed wrong.\n").len(),
        1
    );
    assert_eq!(
        check("`C` fields:\n\n- `a`, `b`, `c` -- extra.\n").len(),
        1,
        "extra"
    );
    assert_eq!(
        check("C. Fields: `a`, `b`.\n").len(),
        1,
        "an inline list is refused"
    );
    assert_eq!(check("`C` fields:\n\n- `a`, `b` no dashes\n").len(), 1);
}
