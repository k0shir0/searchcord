//! Offline native save measurement. Fixture parsing and startup precede timing.
use searchcord_collector::{PageContext, Store};
use serde_json::{Value, json};
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    assert_eq!(args.len(), 3);
    let messages: Vec<Value> = serde_json::from_slice(&std::fs::read(&args[2]).unwrap()).unwrap();
    let mut store = Store::open(&args[1]).unwrap();
    let started = std::time::Instant::now();
    let mut count = 0;
    for page in messages.chunks(100) {
        count += store
            .save_page(
                page,
                &PageContext {
                    channel_id: "20",
                    channel_name: "Fixture",
                    guild_id: Some("10"),
                    guild_name: "Fixture",
                    ..PageContext::default()
                },
            )
            .unwrap();
    }
    println!(
        "{}",
        json!({"seconds":started.elapsed().as_secs_f64(),"result":{"messages":count}})
    );
}
