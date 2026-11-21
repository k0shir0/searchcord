use searchcord_snapshot::{
    SnapshotError, export_media_with_hook, publish_with_hooks, resolve_path,
};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
fn checkpoint() -> Result<(), SnapshotError> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        Err(SnapshotError("Snapshot export interrupted".into()))
    } else {
        Ok(())
    }
}
const USAGE: &str = "Usage: searchcord-snapshot SOURCE.db OUTPUT.db [--block-rows 128] [--report REPORT.json] [--with-media]";
fn integer(value: &str) -> Result<usize, (i32, String)> {
    let value = value
        .parse::<i64>()
        .map_err(|_| (2, "Block size must be an integer".into()))?;
    Ok(usize::try_from(value).unwrap_or(0))
}
fn run() -> Result<(), (i32, String)> {
    let mut args = std::env::args_os().skip(1);
    let mut position = Vec::new();
    let mut block_rows = 128;
    let mut report: Option<PathBuf> = None;
    let mut with_media = false;
    let mut positional = false;
    while let Some(arg) = args.next() {
        let text = arg.to_str().unwrap_or("");
        if !positional {
            match text {
                "--help" | "-h" => {
                    println!("{USAGE}");
                    return Ok(());
                }
                "--" => {
                    positional = true;
                    continue;
                }
                "--with-media" => {
                    with_media = true;
                    continue;
                }
                "--block-rows" => {
                    let value = args
                        .next()
                        .ok_or((2, "--block-rows requires a value".into()))?;
                    block_rows = integer(value.to_str().unwrap_or(""))?;
                    continue;
                }
                "--report" => {
                    report = Some(PathBuf::from(
                        args.next().ok_or((2, "--report requires a path".into()))?,
                    ));
                    continue;
                }
                _ => {}
            }
            if let Some(value) = text.strip_prefix("--block-rows=") {
                block_rows = integer(value)?;
                continue;
            }
            if let Some(value) = text.strip_prefix("--report=") {
                report = Some(PathBuf::from(value));
                continue;
            }
            if text.starts_with('-') && text != "-" {
                return Err((2, format!("Unknown option: {text}")));
            }
        }
        position.push(arg);
    }
    if position.len() != 2 {
        return Err((2, USAGE.into()));
    }
    let source = resolve_path(&PathBuf::from(&position[0])).map_err(|e| (1, e.to_string()))?;
    let destination = resolve_path(&PathBuf::from(&position[1])).map_err(|e| (1, e.to_string()))?;
    let media = destination
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("media");
    if with_media && (media.exists() || media.is_symlink()) {
        return Err((
            2,
            "Media export needs a new destination directory without an existing media cache".into(),
        ));
    }
    if let Some(path) = &report {
        let path = resolve_path(path).map_err(|e| (1, e.to_string()))?;
        if path == source || path == destination {
            return Err((
                2,
                "The report must be separate from both archive files".into(),
            ));
        }
        if path.exists() || path.is_symlink() {
            return Err((2, "Refusing to replace an existing report".into()));
        }
    }
    ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst))
        .map_err(|e| (1, e.to_string()))?;
    let built = publish_with_hooks(
        &source,
        &destination,
        block_rows,
        |messages, total| {
            checkpoint()?;
            if messages % (block_rows as u64 * 1024) == 0 {
                println!(
                    "{}",
                    serde_json::json!({"phase":"pack","messages":messages,"total":total})
                );
                std::io::stdout().flush()?;
            }
            Ok(())
        },
        |_| checkpoint(),
    )
    .map_err(|e: SnapshotError| {
        (
            if INTERRUPTED.load(Ordering::SeqCst) {
                130
            } else {
                1
            },
            e.to_string(),
        )
    })?;
    let mut value = serde_json::to_value(built).map_err(|e| (1, e.to_string()))?;
    if with_media {
        value["media"] = serde_json::to_value(
            export_media_with_hook(&source, &destination, |_| checkpoint()).map_err(|e| {
                (
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        130
                    } else {
                        1
                    },
                    e.to_string(),
                )
            })?,
        )
        .map_err(|e| (1, e.to_string()))?;
    }
    checkpoint().map_err(|e| (130, e.to_string()))?;
    let text = serde_json::to_string_pretty(&value).map_err(|e| (1, e.to_string()))?;
    if let Some(path) = report {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| (1, e.to_string()))?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| (1, e.to_string()))?;
        writeln!(file, "{text}").map_err(|e| (1, e.to_string()))?;
    }
    println!("{text}");
    Ok(())
}
fn main() {
    if let Err((code, message)) = run() {
        eprintln!("{message}");
        std::process::exit(code);
    }
}
