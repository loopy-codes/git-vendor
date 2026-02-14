use clap::CommandFactory;
use std::fs;
use std::path::PathBuf;

#[derive(clap::Parser)]
#[command(name = "xtask")]
#[command(about = "Development tasks for git-vendor")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Generate man pages for all CLI tools
    GenMan {
        /// Output directory for man pages (will be created as man1/ subdirectory)
        #[arg(short, long, default_value = "target/debug/man")]
        output: PathBuf,
    },
}

fn main() {
    let cli = clap::Parser::parse();

    match cli {
        Cli {
            command: Commands::GenMan { output },
        } => {
            if let Err(e) = generate_man_pages(&output) {
                eprintln!("Error generating man pages: {}", e);
                std::process::exit(1);
            }
        }
    }
}

fn generate_man_pages(output_dir: &PathBuf) -> std::io::Result<()> {
    let man1_dir = output_dir.join("man1");
    fs::create_dir_all(&man1_dir)?;

    println!("Generating man pages to: {}", man1_dir.display());

    generate_git_filter_tree_man(&man1_dir)?;
    generate_git_set_attr_man(&man1_dir)?;

    println!("✓ Man pages generated successfully!");
    println!("\nView with: MANPATH=target/debug/man man git-filter-tree");
    Ok(())
}

fn generate_git_filter_tree_man(output_dir: &PathBuf) -> std::io::Result<()> {
    let cmd = git_filter_tree::cli::Cli::command();
    let man = clap_mangen::Man::new(cmd);
    let mut buffer = Vec::new();
    man.render(&mut buffer)?;

    let man_path = output_dir.join("git-filter-tree.1");
    fs::write(&man_path, buffer)?;

    println!("  → git-filter-tree.1");
    Ok(())
}

fn generate_git_set_attr_man(output_dir: &PathBuf) -> std::io::Result<()> {
    let cmd = git_set_attr::cli::Cli::command();
    let man = clap_mangen::Man::new(cmd);
    let mut buffer = Vec::new();
    man.render(&mut buffer)?;

    let man_path = output_dir.join("git-set-attr.1");
    fs::write(&man_path, buffer)?;

    println!("  → git-set-attr.1");
    Ok(())
}
