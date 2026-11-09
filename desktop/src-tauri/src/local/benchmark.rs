//! 初期設定用のGPU並列数計測。利用者の文章は使わない。
use super::*;

// 1リクエストあたり約1,500字の合成英文。同じ12件を全候補で使う。
const TOPICS: [(&str, &str, &str); 12] = [
    (
        "the coastal library",
        "older residents",
        "the reading rooms",
    ),
    (
        "the railway museum",
        "school groups",
        "the exhibition halls",
    ),
    (
        "the public garden",
        "neighborhood volunteers",
        "the outdoor paths",
    ),
    (
        "the municipal archive",
        "local historians",
        "the reference desks",
    ),
    (
        "the arts center",
        "independent artists",
        "the rehearsal studios",
    ),
    (
        "the ferry terminal",
        "island commuters",
        "the boarding areas",
    ),
    (
        "the community market",
        "small vendors",
        "the covered stalls",
    ),
    (
        "the science center",
        "visiting families",
        "the demonstration spaces",
    ),
    (
        "the city observatory",
        "student researchers",
        "the viewing platforms",
    ),
    ("the sports center", "youth teams", "the practice courts"),
    (
        "the concert hall",
        "visiting musicians",
        "the rehearsal rooms",
    ),
    (
        "the public workshop",
        "local craftspeople",
        "the work benches",
    ),
];

fn passages() -> Vec<String> {
    TOPICS.iter().map(|(place, people, spaces)| format!(
        "The planning team at {place} began a six month review after a series of meetings with {people}. \
         The first report described how {spaces} were used on weekdays and how demand changed when the weather was poor. \
         Staff members recorded the hours when {people} arrived and noted which services were most frequently requested. \
         Several respondents explained that a small improvement in the signs would make it easier to find the correct entrance without asking for help. \
         The committee considered moving some activities out of {spaces} so that maintenance could take place during quieter periods. \
         It also asked the finance team to compare the cost of short term repairs with the expense of a more substantial renovation. \
         Instead of accepting the first estimate, managers invited {people} to review a second proposal during an open meeting. \
         Participants pointed out that different users often need the same facilities at different times and that a single schedule would not suit everyone. \
         The next draft sets aside additional time for {people} and introduces a simple system for reporting delays or cancelled sessions. \
         An independent reviewer will check the results after the first month and publish a short account of what worked and what still needs attention. \
         The team will then decide whether to extend the arrangement at {place} or revise the plan before the next season begins."
    )).collect()
}

const WARMUP: &str = "Please place the blue cup near the window.";

pub(super) fn target(results: &[Measurement]) -> Option<usize> {
    results
        .iter()
        .enumerate()
        .filter(|(_, r)| r.id != "none" && r.milliseconds.is_some())
        .min_by_key(|(_, r)| r.milliseconds)
        .map(|(index, _)| index)
}

#[derive(Clone, Serialize)]
pub struct Sample {
    parallel: usize,
    requests_per_second: f64,
    memory: memory::Estimate,
}

// 最高速度の95%以上を出せる最小値を選び、わずかな差のために枠を増やさない。
fn recommendation(samples: &[Sample]) -> Option<&Sample> {
    let best = samples
        .iter()
        .map(|s| s.requests_per_second)
        .fold(0., f64::max);
    samples
        .iter()
        .filter(|s| s.requests_per_second.is_finite() && s.requests_per_second > 0.)
        .filter(|s| s.requests_per_second >= best * 0.95)
        .min_by_key(|s| s.parallel)
}

async fn translate(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    text: &str,
) -> Result<String, String> {
    let (prompt, limit) = prompt(&json!({"text":text,"target_lang":"ja"}))?;
    let mut response = completion_request(client, endpoint, prompt, limit)
        .send()
        .await
        .map_err(|_| "並列計測の応答を取得できませんでした")?;
    if !response.status().is_success() {
        return Err("並列計測の応答が不正です".into());
    }
    let mut decoder = Decoder::default();
    let mut translated = String::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "並列計測の接続が切れました")?
    {
        for event in decoder.feed(&chunk)? {
            if let Some(part) = event["t"].as_str() {
                translated.push_str(part);
            }
            if event["done"] == true {
                if event["finish"] != "stop" || translated.trim().is_empty() {
                    return Err("並列計測の訳が完了しませんでした".into());
                }
                return Ok(translated.trim().to_owned());
            }
        }
    }
    Err("並列計測の接続が途中で切れました".into())
}

async fn batch(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    parallel: usize,
    texts: Arc<Vec<String>>,
    on_progress: impl Fn(usize) -> Result<(), String>,
) -> Result<f64, String> {
    use std::sync::atomic::AtomicUsize;
    let next = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let total = texts.len();
    let started = Instant::now();
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..parallel {
        let client = client.clone();
        let endpoint = endpoint.clone();
        let next = next.clone();
        let completed = completed.clone();
        let texts = texts.clone();
        workers.spawn(async move {
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(text) = texts.get(index) else { break };
                translate(&client, &endpoint, text).await?;
                completed.fetch_add(1, Ordering::Relaxed);
            }
            Ok::<_, String>(())
        });
    }
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    while !workers.is_empty() {
        tokio::select! {
            result = workers.join_next() => {
                if let Some(result) = result {
                    result.map_err(|_| "並列計測が終了しました")??;
                }
            }
            _ = interval.tick() => on_progress(completed.load(Ordering::Relaxed))?,
        }
    }
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    on_progress(total)?;
    Ok(total as f64 / elapsed)
}

async fn sweep(
    samples: &mut Vec<Sample>,
    trial: &Local,
    app: &AppHandle,
    progress: &Channel<MeasurementProgress>,
    measurement: &Measurement,
) -> Result<(), String> {
    let mut stagnant = 0;
    let candidates = [1, 2, 4, 8, 12];
    let texts = Arc::new(passages());
    for (index, parallel) in candidates.into_iter().enumerate() {
        let report = |completed, total| {
            progress
                .send(MeasurementProgress::Parallel {
                    name: measurement.name.clone(),
                    parallel,
                    candidate: index + 1,
                    candidates: candidates.len(),
                    completed,
                    total,
                })
                .map_err(|_| "計測画面が閉じられました".to_owned())
        };
        report(0, 0)?;
        trial.stop();
        let memory = memory::check(app, &measurement.id, parallel)
            .await
            .map_err(|error| format!("{parallel}並列: {error}"))?;
        trial.slots.resize(parallel);
        // 保存値を変えず、候補の枠数だけを変えて起動する。
        let endpoint = tokio::time::timeout(Duration::from_secs(45), trial.start(app))
            .await
            .map_err(|_| "並列計測のモデル読み込みがタイムアウトしました")??;
        // 起動直後の初回推論を計測から除外。準備完了後の異常終了もここで検出する。
        tokio::time::timeout(
            Duration::from_secs(15),
            translate(&trial.client, &endpoint, WARMUP),
        )
        .await
        .map_err(|_| "並列計測の準備がタイムアウトしました")??;
        let speed = tokio::time::timeout(Duration::from_secs(90), async {
            let total = texts.len() * 2;
            let first = batch(
                &trial.client,
                &endpoint,
                parallel,
                texts.clone(),
                |completed| report(completed, total),
            )
            .await?;
            let second = batch(
                &trial.client,
                &endpoint,
                parallel,
                texts.clone(),
                |completed| report(texts.len() + completed, total),
            )
            .await?;
            Ok::<_, String>(2. / (1. / first + 1. / second))
        })
        .await
        .map_err(|_| "90秒以内に並列計測を完了できませんでした")??;
        let best = samples
            .iter()
            .map(|s| s.requests_per_second)
            .fold(0., f64::max);
        stagnant = if speed < best * 1.05 { stagnant + 1 } else { 0 };
        samples.push(Sample {
            parallel,
            requests_per_second: speed,
            memory,
        });
        // 小さい並列数での一時的な揺れを許し、伸びない候補が2回続いたら打ち切る。
        if parallel >= 8 && stagnant >= 2 {
            break;
        }
    }
    Ok(())
}

pub async fn measure(
    app: &AppHandle,
    progress: &Channel<MeasurementProgress>,
    measurement: &mut Measurement,
) -> Result<(), String> {
    let local = app.state::<Local>();
    let trial = Local::new(
        Settings {
            enabled: true,
            device: measurement.id.clone(),
            ..Settings::default()
        },
        true,
    )
    .map_err(|_| "並列計測を準備できませんでした")?;
    let mut samples = Vec::new();
    let result = tokio::select! {
        _ = local.measurement_cancel.canceled() => Err("計測を中止しました".into()),
        result = tokio::time::timeout(Duration::from_secs(240), sweep(&mut samples, &trial, app, progress, measurement)) =>
            result.unwrap_or_else(|_| Err("並列計測の時間上限に達しました".into())),
    };
    // 中止・タイムアウト・エラーでも残った要求ごと計測用エンジンを停止する。
    trial.stop();
    if local.measurement_cancel.canceled.load(Ordering::SeqCst) {
        return Err("計測を中止しました".into());
    }
    if let Some(best) = recommendation(&samples) {
        measurement.recommended_parallel = Some(best.parallel);
    }
    measurement.bulk_error = result.err();
    measurement.parallel_samples = samples;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fastest_successful_gpu_is_measured_even_if_cpu_is_faster() {
        let results = [
            Measurement {
                id: "none".into(),
                milliseconds: Some(1),
                ..Measurement::default()
            },
            Measurement {
                id: "Vulkan0".into(),
                milliseconds: None,
                ..Measurement::default()
            },
            Measurement {
                id: "Vulkan1".into(),
                milliseconds: Some(30),
                ..Measurement::default()
            },
            Measurement {
                id: "Vulkan2".into(),
                milliseconds: Some(10),
                ..Measurement::default()
            },
        ];
        assert_eq!(target(&results), Some(3));
        assert_eq!(target(&results[..2]), None);
    }
    #[tokio::test]
    async fn truncated_translation_is_not_a_valid_speed_sample() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Endpoint {
            url: format!("http://{}", listener.local_addr().unwrap()),
            key: "test".into(),
        };
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = [0; 8192];
            stream.read(&mut data).await.unwrap();
            let body =
                "data: {\"choices\":[{\"text\":\"未完了\",\"finish_reason\":\"length\"}]}\n\n";
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        assert!(translate(&reqwest::Client::new(), &endpoint, WARMUP)
            .await
            .is_err());
        server.await.unwrap();
    }
    #[test]
    fn chooses_smallest_within_five_percent_of_best() {
        let samples =
            [(8, 18.), (12, 20.), (24, 30.), (32, 31.)].map(|(parallel, requests_per_second)| {
                Sample {
                    parallel,
                    requests_per_second,
                    memory: memory::Estimate::default(),
                }
            });
        assert_eq!(recommendation(&samples).unwrap().parallel, 24);
        assert!(recommendation(&[]).is_none());
    }
    #[test]
    fn incomplete_candidates_do_not_replace_successful_results() {
        let samples = [(1, 5.), (2, 9.), (4, 12.)].map(|(parallel, requests_per_second)| Sample {
            parallel,
            requests_per_second,
            memory: memory::Estimate::default(),
        });
        assert_eq!(recommendation(&samples).unwrap().parallel, 4);
    }
    #[tokio::test]
    async fn batch_runs_real_concurrent_requests_against_a_local_mock() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for parallel in [1, 4, 12] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = Endpoint {
                url: format!("http://{}", listener.local_addr().unwrap()),
                key: "test".into(),
            };
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let seen = Arc::new(Mutex::new(std::collections::HashMap::<String, usize>::new()));
            let seen_server = seen.clone();
            let texts = Arc::new(passages());
            assert_eq!(texts.len(), 12);
            assert!(texts
                .iter()
                .all(|text| (1_400..=1_550).contains(&text.len())));
            let server_texts = texts.clone();
            let (a, p, c) = (active.clone(), peak.clone(), count.clone());
            let server = tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                for _ in 0..server_texts.len() {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let (a, p, c) = (a.clone(), p.clone(), c.clone());
                    let seen = seen_server.clone();
                    let texts = server_texts.clone();
                    connections.spawn(async move {
                        let mut data = Vec::new();
                        let mut buffer = [0; 4096];
                        loop {
                            let n = stream.read(&mut buffer).await.unwrap();
                            assert!(n > 0);
                            data.extend_from_slice(&buffer[..n]);
                            if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                                let headers = String::from_utf8_lossy(&data[..end]);
                                let length: usize = headers
                                    .lines()
                                    .find_map(|l| {
                                        l.to_ascii_lowercase()
                                            .strip_prefix("content-length: ")
                                            .map(str::to_owned)
                                    })
                                    .unwrap()
                                    .parse()
                                    .unwrap();
                                if data.len() >= end + 4 + length {
                                    let body: Value =
                                        serde_json::from_slice(&data[end + 4..end + 4 + length])
                                            .unwrap();
                                    let prompt = body["prompt"].as_str().unwrap();
                                    assert_eq!(body["stream"], true);
                                    assert!(body.get("cache_prompt").is_none());
                                    assert!(texts.iter().any(|text| prompt.contains(text.as_str())));
                                    let current = prompt.rsplit("<|plamo:tag|>input<|plamo:msg|>").next().unwrap()
                                        .split("<|plamo:tag|>output").next().unwrap();
                                    *seen.lock().unwrap().entry(current.to_owned()).or_default() += 1;
                                    break;
                                }
                            }
                        }
                        p.fetch_max(a.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        let body = "data: {\"choices\":[{\"text\":\"訳\",\"finish_reason\":\"stop\"}]}\n\n";
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        a.fetch_sub(1, Ordering::SeqCst);
                        stream.write_all(response.as_bytes()).await.unwrap();
                        c.fetch_add(1, Ordering::SeqCst);
                    });
                }
                while let Some(result) = connections.join_next().await {
                    result.unwrap();
                }
            });
            assert!(
                batch(
                    &reqwest::Client::new(),
                    &endpoint,
                    parallel,
                    texts.clone(),
                    |_| Ok(())
                )
                .await
                .unwrap()
                    > 0.
            );
            server.await.unwrap();
            assert_eq!(count.load(Ordering::SeqCst), texts.len());
            assert!(peak.load(Ordering::SeqCst) >= 1);
            if parallel > 1 {
                assert!(peak.load(Ordering::SeqCst) > 1);
            }
            assert!(peak.load(Ordering::SeqCst) <= parallel);
            let counts = seen.lock().unwrap();
            assert_eq!(counts.len(), texts.len());
            for text in texts.iter() {
                assert_eq!(counts.get(text), Some(&1));
            }
        }
    }
}
