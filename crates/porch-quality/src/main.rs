//! `porch-quality` — M16 review engine binary (M3 argv).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use porch_quality::{RunOpts, load_builtin_packs, run_quality, write_output};

#[derive(Debug, Parser)]
#[command(
    name = "porch-quality",
    about = "Porch-owned review quality engine (coverage, relocate, rule packs)",
    disable_help_subcommand = true
)]
struct Args {
    /// Base SHA (exclusive) for the review range.
    #[arg(long)]
    from: Option<String>,
    /// Tip SHA (inclusive) for the review range.
    #[arg(long)]
    to: Option<String>,
    /// Output format (only `json` is supported).
    #[arg(long, default_value = "json")]
    format: String,
    /// Path to write the review JSON.
    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    if args.format != "json" {
        eprintln!("porch-quality: only --format json is supported");
        return ExitCode::from(2);
    }
    let Some(from) = args.from.as_deref() else {
        // --help already handled by clap; bare invoke without required flags.
        eprintln!("porch-quality: --from is required");
        return ExitCode::from(2);
    };
    let Some(to) = args.to.as_deref() else {
        eprintln!("porch-quality: --to is required");
        return ExitCode::from(2);
    };
    let Some(output) = args.output.as_ref() else {
        eprintln!("porch-quality: --output is required");
        return ExitCode::from(2);
    };

    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("porch-quality: cwd: {e}");
            return ExitCode::from(1);
        }
    };

    let packs = match load_builtin_packs() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("porch-quality: packs: {e}");
            return ExitCode::from(1);
        }
    };

    let review = match run_quality(&RunOpts {
        work_tree: &cwd,
        from_sha: from,
        to_sha: to,
        changed_override: None,
        packs: &packs,
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("porch-quality: {e}");
            return ExitCode::from(1);
        }
    };

    if let Err(e) = write_output(output, &review) {
        eprintln!("porch-quality: write: {e}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
