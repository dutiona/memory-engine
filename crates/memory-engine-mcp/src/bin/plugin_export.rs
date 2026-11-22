//! `memory-engine-plugin-export`: the client step of a plugin store's backfill (#1117).
//!
//! It opens a scratch copy of one claude-mem or claude-memory store read-only, its `-wal`
//! applied, and writes one JSON Lines file per store project, each one upload of
//! `POST /backfill/uploads`, plus `index.tsv` naming each file's project. The files hold the
//! store's text unredacted: the server redacts on import, and the files stay in the scratch
//! directory.

use std::fmt::Write as _;
use std::fs::{DirBuilder, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use memory_engine_mcp::adapters::claude_code::{claude_mem, claude_memory};
use memory_engine_mcp::adapters::{ExportError, Slice};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

/// Export a plugin store's rows, one file per project, for the backfill upload.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// The store's plugin, which is also the upload's `ME-Backfill-Kind`.
    kind: Kind,
    /// A scratch copy of the store, its `-wal` beside it when it has one.
    store: PathBuf,
    /// A directory that does not exist yet: created private, it receives the files.
    out: PathBuf,
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    ClaudeMem,
    ClaudeMemory,
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Self::ClaudeMem => "claude-mem",
            Self::ClaudeMemory => "claude-memory",
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Refusal {
    #[error("{0} exists: name a directory that does not")]
    Used(PathBuf),
    #[error(
        "the store is in WAL mode and has no {0} beside it, so it may be older than the store it \
         copies: copy the -wal with it, or, once the copy is known whole, create an empty {0}"
    )]
    WalMissing(PathBuf),
    #[error("the store cannot be read: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Export(#[from] ExportError),
}

/// The `-wal` path SQLite reads beside `store`.
fn wal_of(store: &Path) -> PathBuf {
    let mut wal = store.as_os_str().to_owned();
    wal.push("-wal");
    PathBuf::from(wal)
}

/// Whether the store's header says WAL: its read and write versions, bytes 18 and 19, are 2.
fn in_wal_mode(store: &Path) -> Result<bool, std::io::Error> {
    let mut header = [0; 20];
    std::fs::File::open(store)?.read_exact(&mut header)?;
    Ok(header[18] == 2 && header[19] == 2)
}

/// The copy checks, then the store module's export: nothing is written before both succeed.
fn slices(kind: Kind, store: &Path, out: &Path) -> Result<Vec<Slice>, Refusal> {
    if out.symlink_metadata().is_ok() {
        return Err(Refusal::Used(out.to_owned()));
    }
    let wal = wal_of(store);
    if in_wal_mode(store)? && wal.symlink_metadata().is_err() {
        return Err(Refusal::WalMissing(wal));
    }
    // Never `immutable=1`: SQLite must apply the `-wal`'s committed frames.
    let connection = Connection::open_with_flags(
        store,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(ExportError::from)?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(ExportError::from)?;
    Ok(match kind {
        Kind::ClaudeMem => claude_mem::export(&connection)?,
        Kind::ClaudeMemory => claude_memory::export(&connection)?,
    })
}

/// A slice's file name: the kind and the first 12 hex digits of its project's SHA-256.
fn file_name(kind: Kind, project: Option<&str>) -> String {
    let tag = project.map_or_else(
        || "none".to_owned(),
        |p| {
            Sha256::digest(p.as_bytes())[..6]
                .iter()
                .fold(String::new(), |mut hex, b| {
                    let _ = write!(hex, "{b:02x}");
                    hex
                })
        },
    );
    format!("{}-{tag}.jsonl", kind.name())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

/// Export `args.store` into `args.out`: the slices' files and `index.tsv`. Returns the files
/// and the rows written.
fn run(args: &Args) -> Result<(usize, usize), Refusal> {
    let slices = slices(args.kind, &args.store, &args.out)?;
    DirBuilder::new().mode(0o700).create(&args.out)?;
    let mut index = String::from("file\tproject\tsessions\trows\tbytes\n");
    let mut rows = 0;
    for slice in &slices {
        let name = file_name(args.kind, slice.project.as_deref());
        write_private(&args.out.join(&name), &slice.body)?;
        let _ = writeln!(
            index,
            "{name}\t{}\t{}\t{}\t{}",
            slice.project.as_deref().unwrap_or_default(),
            slice.sessions,
            slice.rows,
            slice.body.len()
        );
        rows += slice.rows;
    }
    write_private(&args.out.join("index.tsv"), index.as_bytes())?;
    Ok((slices.len(), rows))
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok((files, rows)) => {
            println!("{files} files, {rows} rows");
            ExitCode::SUCCESS
        }
        Err(refusal) => {
            eprintln!("memory-engine-plugin-export: {refusal}");
            ExitCode::FAILURE
        }
    }
}
