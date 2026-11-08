//! 初期設定用のGPU並列数計測。利用者の文章は使わない。
use super::*;

// ページの短い項目・見出し・短い段落を混ぜた合成データ。外部サイトの本文は保存しない。
const SHORT_TEXTS: &[&str] = &[
    "Home",
    "Search",
    "Sign in",
    "Create account",
    "Help",
    "Contact us",
    "About this site",
    "Latest updates",
    "Read more",
    "Next page",
    "Previous page",
    "Contents",
    "Discussion",
    "Page history",
    "Related articles",
    "Privacy policy",
    "Terms of use",
    "Share this page",
    "Print version",
    "Download",
    "Edit page",
    "View source",
    "Back to top",
    "All categories",
    "Recent changes",
    "Community",
    "Support",
    "Settings",
    "Language",
    "Accessibility",
    "Overview",
    "Introduction",
    "Background",
    "Summary",
    "References",
    "Further reading",
    "New research",
    "Local news",
    "Public transport",
    "Education",
    "Science",
    "Technology",
    "Open today",
    "Updated yesterday",
    "Published this week",
    "More information",
    "Frequently asked questions",
    "Return to the article",
    "Library extends its opening hours",
    "New bus routes will connect local schools",
    "Researchers publish their latest findings",
    "Community meeting scheduled for next month",
    "Public gardens reopen after maintenance",
    "Weather conditions may affect weekend services",
    "The library opens at nine in the morning. Please return your books on time.",
    "This application translates text on your computer while keeping the original links and formatting.",
    "Residents can attend the public meeting to ask questions and share their views.",
    "The research team compared energy use in schools, libraries, and hospitals over several years.",
    "The new routes will connect residential areas with schools and the central railway station.",
    "Please check the revised schedule before planning your journey to the city centre.",
    "The report recommends gradual improvements and regular maintenance of existing equipment.",
    "Readers can follow the links below to find the original report and supporting documents.",
    "Construction is expected to begin next spring after the public consultation has finished.",
    "Local authorities will review the findings before deciding how to distribute the budget.",
];

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
    index: usize,
    history: Option<(String, String)>,
) -> Result<String, String> {
    let texts = SHORT_TEXTS;
    let text = texts[index % texts.len()];
    let history: Vec<_> = history
        .into_iter()
        .map(|(source, target)| json!({"source":source,"target":target}))
        .collect();
    let (prompt, limit) = prompt(&json!({"text":text,"target_lang":"ja","history":history}))?;
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
) -> Result<f64, String> {
    // 短文は3文×64セッション。同じ192件をすべての並列数で処理する。
    let requests = 192;
    let session_size = 3;
    let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let previous = Arc::new(Mutex::new(None));
    let started = Instant::now();
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..parallel {
        let client = client.clone();
        let endpoint = endpoint.clone();
        let next = next.clone();
        let previous = previous.clone();
        workers.spawn(async move {
            loop {
                let start = next.fetch_add(session_size, Ordering::SeqCst);
                if start >= requests {
                    break;
                }
                for index in start..start + session_size {
                    // page-task.mjsと同様に、送信時点で最後に完了した訳を全ワーカーで共有。
                    let history = previous.lock().unwrap().clone();
                    let translated = translate(&client, &endpoint, index, history).await?;
                    let texts = SHORT_TEXTS;
                    *previous.lock().unwrap() =
                        Some((texts[index % texts.len()].to_owned(), translated));
                }
            }
            Ok::<_, String>(())
        });
    }
    while let Some(result) = workers.join_next().await {
        result.map_err(|_| "並列計測が終了しました")??;
    }
    Ok(requests as f64 / started.elapsed().as_secs_f64().max(0.001))
}

async fn sweep(
    samples: &mut Vec<Sample>,
    trial: &Local,
    app: &AppHandle,
    progress: &Channel<Measurement>,
    measurement: &Measurement,
) -> Result<(), String> {
    let mut stagnant = 0;
    for parallel in [1, 2, 4, 8, 12, 16, 24, 32, 48, 64] {
        let mut update = measurement.clone();
        update.measuring_parallel = Some(parallel);
        progress
            .send(update)
            .map_err(|_| "計測画面が閉じられました")?;
        trial.stop();
        trial.slots.resize(parallel);
        // 保存値を変えず、候補の枠数だけを変えて起動する。
        let endpoint = tokio::time::timeout(Duration::from_secs(45), trial.start(app))
            .await
            .map_err(|_| "並列計測のモデル読み込みがタイムアウトしました")??;
        // 起動直後の初回推論を計測から除外。準備完了後の異常終了もここで検出する。
        tokio::time::timeout(
            Duration::from_secs(15),
            translate(&trial.client, &endpoint, 1000, None),
        )
        .await
        .map_err(|_| "並列計測の準備がタイムアウトしました")??;
        let speed = tokio::time::timeout(Duration::from_secs(60), async {
            let first = batch(&trial.client, &endpoint, parallel).await?;
            let second = batch(&trial.client, &endpoint, parallel).await?;
            Ok::<_, String>(2. / (1. / first + 1. / second))
        })
        .await
        .map_err(|_| "60秒以内に並列計測を完了できませんでした")??;
        let best = samples
            .iter()
            .map(|s| s.requests_per_second)
            .fold(0., f64::max);
        stagnant = if speed < best * 1.05 { stagnant + 1 } else { 0 };
        samples.push(Sample {
            parallel,
            requests_per_second: speed,
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
    progress: &Channel<Measurement>,
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
        measurement.bulk_per_second = Some(best.requests_per_second);
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
        assert!(translate(&reqwest::Client::new(), &endpoint, 0, None)
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
        });
        assert_eq!(recommendation(&samples).unwrap().parallel, 4);
    }
    #[tokio::test]
    async fn batch_runs_real_concurrent_requests_against_a_local_mock() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for parallel in [1, 12, 64] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = Endpoint {
                url: format!("http://{}", listener.local_addr().unwrap()),
                key: "test".into(),
            };
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let seen = Arc::new(Mutex::new(std::collections::HashMap::<String, usize>::new()));
            let history_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (seen_server, history_server) = (seen.clone(), history_count.clone());
            let (a, p, c) = (active.clone(), peak.clone(), count.clone());
            let server = tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                for _ in 0..192 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let (a, p, c) = (a.clone(), p.clone(), c.clone());
                    let (seen, history_count) = (seen_server.clone(), history_server.clone());
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
                                    let texts = SHORT_TEXTS;
                                    assert_eq!(body["stream"], true);
                                    assert!(body.get("cache_prompt").is_none());
                                    assert!(texts.iter().any(|text| prompt.contains(text)));
                                    if prompt.matches("<|plamo:tag|>input").count() > 1 {
                                        history_count.fetch_add(1, Ordering::SeqCst);
                                    }
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
                batch(&reqwest::Client::new(), &endpoint, parallel)
                    .await
                    .unwrap()
                    > 0.
            );
            server.await.unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 192);
            assert!(peak.load(Ordering::SeqCst) >= 1);
            if parallel > 1 {
                assert!(peak.load(Ordering::SeqCst) > 1);
            }
            assert!(peak.load(Ordering::SeqCst) <= parallel);
            let texts = SHORT_TEXTS;
            let counts = seen.lock().unwrap();
            assert_eq!(counts.len(), texts.len());
            for text in texts {
                assert_eq!(counts.get(*text), Some(&3));
            }
            assert!(history_count.load(Ordering::SeqCst) >= (192) - parallel);
        }
    }
}
