//! Linux baseline runner. Artifacts contain aggregate measurements only.
use reqwest::blocking::Client;
use searchcord_parity::{
    Backend, Harness,
    performance::{self, BASE, STEP},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::Command,
    sync::{Arc, Barrier},
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

fn worker(
    h: &Harness,
    operation: &str,
    source: &Path,
    destination: &Path,
    backend: Backend,
) -> Value {
    let native = std::env::var_os(if operation == "snapshot" {
        "SEARCHCORD_PARITY_RUST_SNAPSHOT"
    } else {
        "SEARCHCORD_PARITY_RUST_SAVE"
    })
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|| {
        std::env::current_exe()
            .unwrap()
            .with_file_name(if operation == "snapshot" {
                "searchcord-snapshot"
            } else {
                "parity-save"
            })
    });
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("driver/benchmark.py");
    let output =
        Command::new(std::env::var_os("SEARCHCORD_PARITY_PYTHON").unwrap_or("python".into()))
            .arg("-I")
            .arg(script)
            .arg(if backend == Backend::Rust {
                if operation == "snapshot" {
                    "native-snapshot"
                } else {
                    "native-save"
                }
            } else {
                operation
            })
            .arg(source)
            .arg(destination)
            .arg(native)
            .env("SEARCHCORD_DATA_DIR", h.root().join("archive"))
            .env(
                "SEARCHCORD_CREDENTIAL_FILE",
                h.root().join("credentials/synthetic.vault"),
            )
            .env(
                "SEARCHCORD_CREDENTIAL_KEY_FILE",
                h.root().join("credentials/synthetic.key"),
            )
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn main() {
    assert!(
        cfg!(target_os = "linux"),
        "Quantitative baseline requires Linux x86-64; functional tests run on all platforms"
    );
    assert_eq!(std::env::consts::ARCH, "x86_64");
    let args: Vec<_> = std::env::args().collect();
    assert!(
        (3..=4).contains(&args.len()),
        "usage: parity-benchmark ROWS REPORT.json [python|rust]"
    );
    let backend = Backend::parse(args.get(3).map(String::as_str).unwrap_or("python")).unwrap();
    let rows: usize = args[1].parse().unwrap();
    assert!(rows >= 1000);
    let repetitions = std::env::var("SEARCHCORD_BENCH_REPETITIONS")
        .map(|v| v.parse::<usize>().unwrap())
        .unwrap_or(3);
    assert!((1..=10).contains(&repetitions));
    let output = Path::new(&args[2]);
    assert!(!output.exists(), "Refusing to overwrite a benchmark report");
    let source = performance::benchmark_fixture(rows);
    let digest = performance::records_digest(&source.database());
    let mut exports = vec![];
    let mut packed = source.root().join("archive/packed.db");
    for run in 0..repetitions {
        let destination = source.root().join(format!("archive/packed-{run}.db"));
        let result = worker(
            &source,
            "snapshot",
            &source.archive(),
            &destination,
            backend,
        );
        assert_eq!(result["result"]["messages"], rows);
        assert_eq!(result["result"]["records_sha256"], digest);
        assert_eq!(result["result"]["integrity"], "ok");
        assert_eq!(result["result"]["source_main_file_unchanged"], true);
        exports.push(json!({"run":run,"seconds":result["seconds"],"messages_per_second":rows as f64/result["seconds"].as_f64().unwrap(),
            "peak_rss_bytes":result["peak_rss_bytes"],"snapshot_bytes":result["result"]["snapshot_bytes"],"snapshot_pages":result["result"]["snapshot_pages"],"snapshot_blocks":result["result"]["snapshot_blocks"],"python":result["python"]}));
        packed = destination;
    }
    let mut searches = vec![];
    let workload = Arc::new(performance::search_workload(rows));
    for (format, path) in [("standard", source.archive()), ("packed", packed)] {
        for concurrency in [1, 16] {
            let mut viewer = Harness::viewer_with_backend(backend).unwrap();
            fs::create_dir_all(viewer.root().join("archive")).unwrap();
            fs::copy(&path, viewer.archive()).unwrap();
            viewer.start_viewer("synthetic.db", true, None);
            let start = Instant::now();
            let first = viewer.get("/api/search", &[("q", "common".into())]);
            let fresh_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(first.status, 200);
            assert_eq!(first.json()["messages"].as_array().unwrap().len(), 40);
            let origin = viewer.origin().to_owned();
            let gate = Arc::new(Barrier::new(concurrency));
            let total = Instant::now();
            let threads: Vec<_> = (0..concurrency)
                .map(|_| {
                    let origin = origin.clone();
                    let gate = gate.clone();
                    let workload = workload.clone();
                    thread::spawn(move || {
                        let client = Client::builder()
                            .no_proxy()
                            .timeout(std::time::Duration::from_secs(15))
                            .build()
                            .unwrap();
                        gate.wait();
                        workload
                            .iter()
                            .enumerate()
                            .map(|(case, query)| {
                                let now = Instant::now();
                                let response = client
                                    .get(format!("{origin}/api/search"))
                                    .query(&query.params)
                                    .send()
                                    .unwrap();
                                assert_eq!(response.status().as_u16(), 200);
                                let result: Value = response.json().unwrap();
                                let actual: Vec<_> = result["messages"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .map(|v| v["id"].as_str().unwrap())
                                    .collect();
                                assert_eq!(actual, query.ids, "query {:?}", query.params);
                                (case, now.elapsed().as_secs_f64() * 1000.0)
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            let samples: Vec<(usize, f64)> = threads
                .into_iter()
                .flat_map(|t| t.join().unwrap())
                .collect();
            let seconds = total.elapsed().as_secs_f64();
            let cases: Vec<_> = workload.iter().enumerate().map(|(case, query)| {
                let mut latency: Vec<_> = samples.iter().filter(|s| s.0 == case).map(|s| s.1).collect();
                latency.sort_by(f64::total_cmp);
                json!({"params":query.params,"p50_ms":latency[latency.len()/2],"p95_ms":latency[(latency.len()-1)*95/100]})
            }).collect();
            let mut latency: Vec<_> = samples.iter().map(|s| s.1).collect();
            latency.sort_by(f64::total_cmp);
            let metrics = viewer.command(json!({"op":"process_metrics"}));
            searches.push(json!({"format":format,"concurrency":concurrency,"fresh_ms":fresh_ms,"warm_requests":latency.len(),
            "warm_p50_ms":latency[latency.len()/2],"warm_p95_ms":latency[(latency.len()-1)*95/100],
            "warm_seconds":seconds,"peak_rss_bytes":metrics["peak_rss_bytes"],"cases":cases}));
        }
    }
    let mut saves = vec![];
    for run in 0..repetitions {
        let mut target = Harness::new().unwrap();
        target.init();
        let messages:Vec<_>=(0..10_000).map(|i|json!({"id":(BASE+i as i64*STEP).to_string(),"author":{"id":"30","username":"Fixture"},"content":performance::content(i),"attachments":[]})).collect();
        let payload = target.root().join("save.json");
        fs::write(&payload, serde_json::to_vec(&messages).unwrap()).unwrap();
        let result = worker(&target, "save", &target.archive(), &payload, backend);
        assert_eq!(result["result"]["messages"], 10_000);
        assert_eq!(
            target
                .database()
                .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            10_000
        );
        saves.push(json!({"run":run,"messages_per_second":10_000.0/result["seconds"].as_f64().unwrap(),"seconds":result["seconds"],"peak_rss_bytes":result["peak_rss_bytes"]}));
    }
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .unwrap()
        .lines()
        .find(|s| s.starts_with("model name"))
        .unwrap_or("unknown")
        .to_owned();
    let report = json!({"schema":2,"backend":if backend==Backend::Python {"python"}else{"rust"},"reference_sha":"481f62c6d1e111ca6f5771554a0e89ecd42b088e",
        "toolchain":"1.90.0","captured_unix_seconds":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        "machine":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"cpu":cpu,"parallelism":thread::available_parallelism().unwrap().get()},
        "fixture":{"recipe":"v3-mixed-search-profiles-attachments-public-id-backfill","messages":rows,"records_sha256":digest,"source_bytes":fs::metadata(source.archive()).unwrap().len(),"block_rows":128,"compression_level":9},
        "exports":exports,"searches":searches,"saves":saves,"qualification":false,"unimplemented_measurements":[],"export_timing":if backend==Backend::Rust {"whole native subprocess including startup"}else{"reference function after interpreter imports"},"save_timing":"save calls after fixture parsing and storage startup"});
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .unwrap();
    file.write_all(serde_json::to_string_pretty(&report).unwrap().as_bytes())
        .unwrap();
    println!(
        "Recorded {:?} development measurements in {}",
        backend,
        output.display()
    );
}
