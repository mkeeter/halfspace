use clap::Parser;

/// An experimental CAD tool
#[derive(clap::Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Show verbose logging
    #[clap(short, long)]
    verbose: bool,

    /// File to edit (created if not present)
    target: Option<std::path::PathBuf>,
}

pub fn main() -> Result<(), eframe::Error> {
    let args = Args::parse();
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(if args.verbose {
            "halfspace=trace"
        } else {
            "halfspace=info"
        }),
    )
    .init();

    halfspace::native::run(args.target)
}
