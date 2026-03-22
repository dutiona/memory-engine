use std::path::{Path, PathBuf};

use memory_engine::{EngineConfig, MemoryEngine};

#[derive(clap::Args)]
pub struct ImportArgs {
    /// Path to JSON snapshot file (plain, .gz, or .zst — auto-detected)
    snapshot: PathBuf,

    /// Embedding dimension (required for compressed snapshots, auto-detected for plain JSON)
    #[arg(long)]
    embed_dim: Option<usize>,
}

pub fn run(db: &Path, args: &ImportArgs) -> anyhow::Result<()> {
    if db.exists() {
        anyhow::bail!(
            "target database {} already exists — import requires a fresh path",
            db.display()
        );
    }

    let embed_dim = match args.embed_dim {
        Some(dim) => dim,
        None => peek_embed_dim(&args.snapshot).map_err(|e| {
            anyhow::anyhow!("{e}\n\nHint: for compressed snapshots, pass --embed-dim explicitly")
        })?,
    };

    let config = EngineConfig::new(db.to_path_buf(), embed_dim);
    let _engine = MemoryEngine::restore_json(&args.snapshot, &config)?;

    eprintln!(
        "Imported {} into {} (embed_dim={})",
        args.snapshot.display(),
        db.display(),
        embed_dim,
    );
    Ok(())
}

/// Peek at a JSON snapshot file to extract `embed_dim` without loading everything.
fn peek_embed_dim(path: &Path) -> anyhow::Result<usize> {
    #[derive(serde::Deserialize)]
    struct SnapshotHeader {
        embed_dim: usize,
    }

    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);

    let header: SnapshotHeader = serde_json::from_reader(reader)
        .map_err(|e| anyhow::anyhow!("failed to read snapshot header: {e}"))?;

    Ok(header.embed_dim)
}
