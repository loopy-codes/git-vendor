use clap::Parser;
use git_set_attr::SetAttr;
use git2 as git;
use std::path::PathBuf;
use std::process;

#[derive(Parser)]
#[command(name = "git-set-attr")]
#[command(author, version, about = "Set gitattributes via patterns and key-value pairs", long_about = None)]
struct Cli {
    /// Gitattributes-style pattern (e.g. "*.txt", "path/to/*.bin")
    pattern: String,

    /// Attributes to set (e.g. "diff", "-text", "filter=lfs")
    #[arg(required = true)]
    attributes: Vec<String>,

    /// Path to the .gitattributes file to modify
    #[arg(short, long)]
    file: Option<PathBuf>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Open the repository in current directory
    let repo = git::Repository::open(".")?;

    // Convert attributes to string slices
    let attributes: Vec<&str> = cli.attributes.iter().map(|s| s.as_str()).collect();

    // Set attributes in the appropriate .gitattributes file
    repo.set_attr(&cli.pattern, &attributes, cli.file.as_deref())?;

    Ok(())
}
