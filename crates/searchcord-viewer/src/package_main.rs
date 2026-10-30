use std::path::PathBuf;
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(2);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let destination = PathBuf::from(
        args.next()
            .ok_or("Usage: searchcord-package-search NEW_DIRECTORY [--viewer BINARY]")?,
    );
    let viewer = match args.next() {
        None => std::env::current_exe()?
            .with_file_name(format!("searchcord-search{}", std::env::consts::EXE_SUFFIX)),
        Some(flag) if flag == "--viewer" => {
            PathBuf::from(args.next().ok_or("--viewer needs a binary")?)
        }
        _ => return Err("Unknown argument".into()),
    };
    if args.next().is_some() {
        return Err("Unexpected argument".into());
    }
    println!(
        "{}",
        searchcord_viewer::package::build(&destination, &viewer)?.display()
    );
    Ok(())
}
