use searchcord_collector::{
    Result,
    discord::Discord,
    http::{self, App},
    pacing::{Gate, SystemClock},
    writer::Writer,
};
use searchcord_viewer::terminal::{Terminal, local_url};
use std::{io::Write, path::PathBuf, sync::Arc, time::Duration};

fn application_path(path: PathBuf) -> Result<PathBuf> {
    let path = if path.is_absolute() || path.to_string_lossy().starts_with('~') {
        path
    } else {
        std::env::current_exe()?
            .parent()
            .ok_or("Could not locate application folder")?
            .join(path)
    };
    Ok(searchcord_collector::credentials::resolve(&path)?)
}
fn open_browser(url: &str) -> bool {
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler");
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}
async fn browser_when_ready(url: String, instance: String) {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
    else {
        return;
    };
    loop {
        if let Ok(response) = client.get(format!("{url}/api/health")).send().await
            && response.status() == 200
            && response
                .headers()
                .get("x-searchcord-instance")
                .is_some_and(|v| v == instance.as_str())
            && response
                .bytes()
                .await
                .is_ok_and(|b| b == "searchcord-ready")
        {
            println!("Searchcord: {url}");
            if !open_browser(&url) {
                eprintln!("Could not open the default browser. Open {url} manually.");
            }
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
async fn signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
async fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mut database = None;
    let mut explicit_data_dir = false;
    let mut data =
        PathBuf::from(std::env::var_os("SEARCHCORD_DATA_DIR").unwrap_or_else(|| "data".into()));
    let mut host = std::env::var("SEARCHCORD_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let mut port = std::env::var("SEARCHCORD_PORT")
        .unwrap_or_else(|_| "8000".into())
        .parse::<u16>()?;
    let mut browser = std::env::var("SEARCHCORD_NO_BROWSER").ok().as_deref() != Some("1");
    while let Some(arg) = args.next() {
        match arg.to_str().ok_or("Arguments must be valid Unicode")? {
            "--help" | "-h" => {
                println!(
                    "Usage: searchcord [--data-dir DIR | --db FILE] [--host 127.0.0.1] [--port 8000] [--no-browser]\nSEARCHCORD_DATA_DIR, SEARCHCORD_HOST, SEARCHCORD_PORT and the external credential path variables are supported. Relative archive paths resolve beside the executable."
                );
                return Ok(());
            }
            "--db" => database = Some(PathBuf::from(args.next().ok_or("--db needs a path")?)),
            "--data-dir" => {
                data = PathBuf::from(args.next().ok_or("--data-dir needs a path")?);
                explicit_data_dir = true;
            }
            "--host" => {
                host = args
                    .next()
                    .ok_or("--host needs an address")?
                    .into_string()
                    .map_err(|_| "Invalid host")?
            }
            "--port" => {
                port = args
                    .next()
                    .ok_or("--port needs a number")?
                    .to_str()
                    .ok_or("Invalid port")?
                    .parse()?
            }
            "--no-browser" => browser = false,
            _ => return Err(format!("Unknown option: {}", arg.to_string_lossy()).into()),
        }
    }
    if database.is_some() && explicit_data_dir {
        return Err("--db and --data-dir are mutually exclusive".into());
    }
    let _terminal = Terminal::start();
    let path = application_path(database.unwrap_or_else(|| data.join("searchcord.db")))?;
    println!("Preparing Searchcord on {}", local_url(&host, port));
    println!(
        "On a first database upgrade, this may take several minutes. The browser opens when ready."
    );
    let parent = path.parent().ok_or("Invalid archive path")?.to_owned();
    tokio::task::spawn_blocking(move || std::fs::create_dir_all(parent)).await??;
    let writer = Writer::open(path.clone()).await?;
    let clock = SystemClock::default();
    let gate = Gate::open(path.clone(), clock.clone()).await?;
    let app = App::open(
        path,
        Discord::new(gate.clone())?,
        Arc::new(clock),
        writer.clone(),
    )
    .await?;
    let listener = match tokio::net::TcpListener::bind((host.as_str(), port)).await {
        Ok(listener) => listener,
        Err(error) => {
            app.shutdown().await?;
            writer.shutdown().await?;
            return Err(error.into());
        }
    };
    let url = local_url(&host, listener.local_addr()?.port());
    let instance = app.instance_id().to_owned();
    println!("Local database ready. Searchcord: {url}");
    println!(
        "Saving each fetched page; shared scrape interval 0.5–1.5s, break 60–90s after 100 successful responses; live polling 3s."
    );
    std::io::stdout().flush()?;
    let browser_task = browser.then(|| tokio::spawn(browser_when_ready(url, instance)));
    let shutdown_app = app.clone();
    let served = axum::serve(listener, http::router(app.clone()))
        .with_graceful_shutdown(async move {
            signal().await;
            if let Err(error) = shutdown_app.shutdown().await {
                eprintln!("Could not suspend collection: {error}");
            }
        })
        .await;
    if let Some(task) = browser_task {
        task.abort();
        let _ = task.await;
    }
    let shutdown = app.shutdown().await;
    let pacing = gate.barrier().await;
    let closed = writer.shutdown().await;
    served?;
    shutdown?;
    pacing?;
    closed?;
    Ok(())
}
