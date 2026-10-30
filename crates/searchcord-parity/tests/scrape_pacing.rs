//! Exact clock/random/response traces for the shared Discord gate.
use searchcord_parity::Harness;
use serde_json::{Value, json};

fn call(h: &mut Harness, mut r: Value) -> Value {
    r["op"] = json!("job");
    h.command(r)
}
fn channel(id: &str) -> Value {
    json!({"id":id,"name":"fixture","guild_id":"10","guild_name":"Fixture"})
}
fn setup(draws: Vec<f64>, slow: f64) -> (Harness, Vec<String>) {
    let mut h = Harness::new().unwrap();
    h.init();
    call(&mut h, json!({"action":"configure"}));
    call(&mut h, json!({"action":"block_fetch"}));
    let mut ids = vec![];
    for id in ["20", "21"] {
        let reply = call(
            &mut h,
            json!({"action":"http","path":"/api/scrape/start","body":{"channels":[channel(id)]}}),
        );
        assert_eq!(reply["status"], 200);
        let id = reply["body"]["job_id"].as_str().unwrap().to_owned();
        call(
            &mut h,
            json!({"action":"wait_snapshot","id":id,"fields":{"index":1}}),
        );
        ids.push(id);
    }
    call(
        &mut h,
        json!({"action":"virtual_clock","draws":draws,"bounds":[0.5,1.5],"response_seconds":slow}),
    );
    (h, ids)
}
fn saved(h: &Harness) -> Value {
    let raw: String = h
        .database()
        .query_row(
            "SELECT state_json FROM collection_jobs WHERE kind='pacing'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}
fn inspect(h: &mut Harness) -> Value {
    call(h, json!({"action":"pacing_inspect"}))
}
fn recover(h: &mut Harness) {
    call(h, json!({"action":"control","method":"suspend"}));
    call(h, json!({"action":"recover_pacing"}));
}

#[test]
fn failed_network_request_persists_the_last_attempt_deadline() {
    let (mut h, ids) = setup(vec![0.75], 0.6);
    call(
        &mut h,
        json!({"action":"configure","replies":[{"error":"fixture"}]}),
    );
    let reply = call(&mut h, json!({"action":"pacing_requests","ids":[ids[0]]}));
    assert_eq!(reply["results"], json!([{"error":"ReadTimeout"}]));
    let state = saved(&h);
    let trace = inspect(&mut h);
    assert!(state["last_scrape_request"].as_f64().unwrap() > trace["wall"].as_f64().unwrap() - 1.0);
    assert_eq!(state["scrape_confirmed_requests"], 0);
    let sent = trace["sent"][0].as_f64().unwrap();
    let finished = trace["finished"][0].as_f64().unwrap();
    assert!((finished - sent - 0.6).abs() < 0.001);
}

#[test]
fn random_intervals_retries_and_100_success_break_are_shared() {
    let mut draws = vec![];
    for i in 0..103 {
        draws.push(if i % 2 == 0 { 1.5 } else { 0.5 });
        if i == 101 {
            draws.push(60.0);
        }
    }
    let (mut h, ids) = setup(draws, 0.0);
    call(
        &mut h,
        json!({"action":"configure","replies":[{"status":429,"body":{"retry_after":0}},{"status":503}]}),
    );
    let requests: Vec<_> = (0..102).map(|i| ids[i % 2].clone()).collect();
    let reply = call(&mut h, json!({"action":"pacing_requests","ids":requests}));
    assert_eq!(reply["results"][0]["status"], 503);
    assert_eq!(reply["results"].as_array().unwrap().len(), 102);
    assert!(
        reply["results"].as_array().unwrap()[1..]
            .iter()
            .all(|r| r["status"] == 200)
    );
    let trace = inspect(&mut h);
    let draws = trace["draws"].as_array().unwrap();
    assert_eq!(draws.len(), 104);
    assert_eq!(
        draws.iter().filter(|v| **v == json!([0.5, 1.5])).count(),
        103
    );
    assert_eq!(draws[102], json!([60, 90]));
    let sent: Vec<f64> = trace["sent"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert_eq!(sent.len(), 103);
    let intervals: Vec<_> = sent[..102].windows(2).map(|w| w[1] - w[0]).collect();
    assert!(intervals.iter().all(|v| (0.499..=1.501).contains(v)));
    assert!(intervals.iter().any(|v| *v < 0.501));
    assert!(intervals.iter().any(|v| *v > 1.499));
    assert!((sent[102] - sent[101] - 60.0).abs() < 0.001);
    assert_eq!(trace["counter"], 1);
    assert!(
        reply["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["type"] == "scrape_break")
    );
    assert_eq!(saved(&h)["scrape_confirmed_requests"], 1);
}

#[test]
fn actual_random_interval_bounds_and_slow_response_are_shared() {
    let (mut h, ids) = setup(vec![0.75; 4], 0.6);
    let reply = call(
        &mut h,
        json!({"action":"pacing_requests","concurrent":true,"ids":[ids[0],ids[1],ids[0],ids[1]]}),
    );
    assert!(
        reply["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"] == 200)
    );
    let trace = inspect(&mut h);
    assert_eq!(trace["draws"], json!(vec![[0.5, 1.5]; 4]));
    for i in 0..3 {
        let interval =
            trace["sent"][i + 1].as_f64().unwrap() - trace["finished"][i].as_f64().unwrap();
        assert!((interval - 0.75).abs() < 0.001, "{trace}");
    }
    assert_eq!(trace["counter"], 4);
}

#[test]
fn counter_cooldowns_and_break_deadline_survive_restart_and_job_changes() {
    let (mut h, ids) = setup(vec![1.5, 90.0], 0.0);
    call(&mut h, json!({"action":"set_counter","value":99}));
    call(
        &mut h,
        json!({"action":"configure","replies":[{"status":200,"body":[],"headers":{
        "X-RateLimit-Remaining":"0","X-RateLimit-Reset-After":"30"}}]}),
    );
    call(&mut h, json!({"action":"pacing_requests","ids":[ids[0]]}));
    let trace = inspect(&mut h);
    assert_eq!(trace["draws"][1], json!([60, 90]));
    let state = saved(&h);
    assert!(state["scrape_break_until"].as_f64().unwrap() > trace["wall"].as_f64().unwrap() + 88.0);
    assert_eq!(state["scrape_confirmed_requests"], 0);
    recover(&mut h);
    let trace = inspect(&mut h);
    assert!(trace["break_seconds"].as_f64().unwrap() > 88.0);
    assert!(trace["cooldown_seconds"].as_f64().unwrap() > 28.0);
    assert_eq!(trace["counter"], 0);
    for id in ids {
        call(&mut h, json!({"action":"control","method":"stop","id":id}));
    }
    let new = call(
        &mut h,
        json!({"action":"http","path":"/api/scrape/start","body":{"channels":[channel("22")]}}),
    );
    assert_eq!(new["status"], 200);
    let snapshot = call(
        &mut h,
        json!({"action":"snapshot","id":new["body"]["job_id"]}),
    );
    assert!(snapshot["break_seconds"].as_f64().unwrap() > 88.0);
}

#[test]
fn nonzero_counter_survives_restart() {
    let (mut h, ids) = setup(vec![0.75; 2], 0.0);
    call(
        &mut h,
        json!({"action":"configure","replies":[{"status":200,"body":[]},{"status":503}]}),
    );
    let replies = call(&mut h, json!({"action":"pacing_requests","ids":ids}));
    assert_eq!(replies["results"], json!([{"status":200},{"status":503}]));
    recover(&mut h);
    assert_eq!(inspect(&mut h)["counter"], 1);
    assert_eq!(saved(&h)["scrape_confirmed_requests"], 1);
}
