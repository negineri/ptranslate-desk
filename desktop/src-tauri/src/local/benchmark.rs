//! 初期設定用のGPU並列数計測。利用者の文章は使わない。
use super::*;

const SHORT_TEXTS: [&str; 3] = [
    "This application translates text on your computer.",
    "The library opens at nine in the morning. Please return your books on time.",
    "This application translates text on your computer while keeping the original links and formatting.",
];

fn adjusted_parallel(measured: usize, long_text: bool) -> usize {
    if long_text {
        measured
    } else {
        (measured * 3 / 4).max(1)
    }
}

const TEXTS: [&str; 2] = [
    "The city council approved a plan to improve public transport. The new routes will connect residential areas with schools, hospitals, and the central railway station. Construction is expected to begin next spring, although residents have asked officials to provide more detailed information about the cost and schedule.",
    "The research team published a report on energy use in public buildings. They compared schools, libraries, and hospitals over several years, taking local weather conditions into account. Better insulation reduced heating costs, while regular maintenance helped keep equipment working efficiently. The report recommends gradual improvements and careful monitoring rather than replacing every system at once. Local authorities will review the findings before deciding how to distribute next year's budget. Residents can attend a public meeting to ask questions and share their views.",
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
    long_text: bool,
) -> Result<(), String> {
    let texts: &[&str] = if long_text { &TEXTS } else { &SHORT_TEXTS };
    let text = format!("{} Reference number: {index}.", texts[index % texts.len()]);
    let (prompt, limit) = prompt(&json!({"text":text,"target_lang":"ja"}))?;
    let response = client
        .post(format!("{}/v1/completions", endpoint.url))
        .bearer_auth(&endpoint.key)
        .json(&json!({"prompt":prompt,"max_tokens":limit,"temperature":0,"stream":false,
            "cache_prompt":false,"add_special_tokens":true,"stop":["<|plamo:tag|>","<|plamo:bos|>"]}))
        .send().await.map_err(|_| "並列計測の応答を取得できませんでした")?;
    if !response.status().is_success() {
        return Err("並列計測の応答が不正です".into());
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| "並列計測の応答が不正です")?;
    if value["choices"][0]["text"]
        .as_str()
        .is_none_or(|t| t.trim().is_empty())
        || value["choices"][0]["finish_reason"] != "stop"
    {
        return Err("並列計測の訳が完了しませんでした".into());
    }
    Ok(())
}

async fn batch(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    parallel: usize,
    long_text: bool,
) -> Result<f64, String> {
    // 短文は各セッションに異なる3文を順番に流す。長文は合計96件。
    let requests = if long_text { 96 } else { parallel * 3 };
    let started = Instant::now();
    let mut workers = tokio::task::JoinSet::new();
    for worker in 0..parallel {
        let client = client.clone();
        let endpoint = endpoint.clone();
        workers.spawn(async move {
            let indexes: Vec<_> = if long_text {
                (worker..requests).step_by(parallel).collect()
            } else {
                (worker * 3..worker * 3 + 3).collect()
            };
            for index in indexes {
                translate(&client, &endpoint, index, long_text).await?;
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
            translate(&trial.client, &endpoint, 1000, measurement.long_text),
        )
        .await
        .map_err(|_| "並列計測の準備がタイムアウトしました")??;
        let speed = tokio::time::timeout(Duration::from_secs(60), async {
            let first = batch(&trial.client, &endpoint, parallel, measurement.long_text).await?;
            let second = batch(&trial.client, &endpoint, parallel, measurement.long_text).await?;
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
            auto_parallel: false,
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
        measurement.measured_parallel = Some(best.parallel);
        measurement.recommended_parallel =
            Some(adjusted_parallel(best.parallel, measurement.long_text));
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
    fn short_recommendation_is_three_quarters_rounded_down_and_long_is_unchanged() {
        for (measured, expected) in [(1, 1), (2, 1), (4, 3), (24, 18), (32, 24), (64, 48)] {
            assert_eq!(adjusted_parallel(measured, false), expected);
            assert_eq!(adjusted_parallel(measured, true), measured);
        }
    }
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
            let body = r#"{"choices":[{"text":"未完了","finish_reason":"length"}]}"#;
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
        assert!(translate(&reqwest::Client::new(), &endpoint, 0, false)
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
        for long_text in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = Endpoint {
                url: format!("http://{}", listener.local_addr().unwrap()),
                key: "test".into(),
            };
            let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (a, p, c) = (active.clone(), peak.clone(), count.clone());
            let server = tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                for _ in 0..if long_text { 96 } else { 36 } {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let (a, p, c) = (a.clone(), p.clone(), c.clone());
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
                                    let texts: &[&str] =
                                        if long_text { &TEXTS } else { &SHORT_TEXTS };
                                    assert!(texts.iter().any(|text| prompt.contains(text)));
                                    break;
                                }
                            }
                        }
                        p.fetch_max(a.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        let body = r#"{"choices":[{"text":"訳","finish_reason":"stop"}]}"#;
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
                batch(&reqwest::Client::new(), &endpoint, 12, long_text)
                    .await
                    .unwrap()
                    > 0.
            );
            server.await.unwrap();
            assert_eq!(
                count.load(Ordering::SeqCst),
                if long_text { 96 } else { 36 }
            );
            assert!(peak.load(Ordering::SeqCst) > 1);
            assert!(peak.load(Ordering::SeqCst) <= 12);
        }
    }
}
