use searchcord_archive::Archive;
use std::{io::Write, path::PathBuf};
fn application_path(path: PathBuf) -> PathBuf {
    let path = if let Some(s) = path.to_str().and_then(|s| s.strip_prefix("~/")) {
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(s)
    } else {
        path
    };
    if path.is_absolute() {
        path
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default()
            .join(path)
    }
}
#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("{e}");
        std::process::exit(2)
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: std::collections::VecDeque<_> = std::env::args_os().skip(1).collect();
    let mut database = None;
    let mut data = None;
    let mut host = "127.0.0.1".to_owned();
    let mut port = 8001;
    let mut immutable = std::env::var("SEARCHCORD_IMMUTABLE").ok().as_deref() == Some("1");
    while let Some(mut arg) = args.pop_front() {
        if let Some((name, value)) = arg
            .to_str()
            .filter(|s| s.starts_with("--"))
            .and_then(|s| s.split_once('='))
        {
            let name = name.to_owned();
            let value = std::ffi::OsString::from(value);
            args.push_front(value);
            arg = name.into();
        }
        match arg.to_str().unwrap_or("") {
            "--help" | "-h" => {
                println!(
                    "Usage: searchcord-search [--db FILE | --data-dir DIR] [--snapshot] [--host 127.0.0.1] [--port 8001]"
                );
                return Ok(());
            }
            "--snapshot" => immutable = true,
            "--db" => database = Some(PathBuf::from(args.pop_front().ok_or("--db needs a path")?)),
            "--data-dir" => {
                data = Some(PathBuf::from(
                    args.pop_front().ok_or("--data-dir needs a path")?,
                ))
            }
            "--host" => {
                host = args
                    .pop_front()
                    .ok_or("--host needs an address")?
                    .into_string()
                    .map_err(|_| "Invalid host")?
            }
            "--port" => {
                port = args
                    .pop_front()
                    .ok_or("--port needs a number")?
                    .to_str()
                    .ok_or("Invalid port")?
                    .parse::<u16>()?
            }
            _ => return Err(format!("Unknown option: {}", arg.to_string_lossy()).into()),
        }
    }
    if database.is_some() && data.is_some() {
        return Err("--db and --data-dir are mutually exclusive".into());
    }
    let path = database
        .or_else(|| data.map(|p| p.join("searchcord.db")))
        .or_else(|| std::env::var_os("SEARCHCORD_DB").map(PathBuf::from))
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("SEARCHCORD_DATA_DIR").unwrap_or("data".into()))
                .join("searchcord.db")
        });
    let mut archive = Archive::new(application_path(path), immutable);
    if let Ok(window) = std::env::var("SEARCHCORD_VIEWER_SCAN_WINDOW") {
        archive.scan_window = window.parse()?;
    }
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    let actual = listener.local_addr()?;
    println!("Searchcord: http://{actual}");
    std::io::stdout().flush()?;
    axum::serve(listener, searchcord_viewer::router(archive))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
