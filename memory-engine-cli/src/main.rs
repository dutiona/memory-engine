use clap::Parser;

/// memory-engine-cli — operator tool for agent memory databases
#[derive(Parser)]
#[command(name = "memory-engine-cli", version, about)]
struct Cli {
    /// Path to the memory-engine SQLite database
    #[arg(long, env = "MEMORY_ENGINE_DB")]
    db: std::path::PathBuf,
}

fn main() {
    let _cli = Cli::parse();
    eprintln!("memory-engine-cli: not yet implemented");
    std::process::exit(1);
}
