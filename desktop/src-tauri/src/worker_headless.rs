//! Linux の常駐サービスから既存の制限付きワーカーを1回動かす。
use crate::{worker_agent::Bootstrap, worker_supervisor};
use std::io::{self, BufRead, Read};

pub(crate) fn entry() -> Result<(), String> {
    let mut input = zeroize::Zeroizing::new(Vec::new());
    io::stdin()
        .lock()
        .take(16 * 1024)
        .read_until(b'\n', &mut input)
        .map_err(|_| "起動情報を読み込めませんでした")?;
    if !input.ends_with(b"\n") {
        return Err("起動情報が不正です".into());
    }
    let mut config: Bootstrap = serde_json::from_slice(&input).map_err(|_| "起動情報が不正です")?;
    let version = env!("CARGO_PKG_VERSION");
    config.agent_version = version.split('-').next().unwrap_or(version).into();
    config.relay_port = None;
    config.relay_secret = None;
    drop(input);

    let (stop, stopped) = tokio::sync::oneshot::channel();
    // 親の終了や停止要求をパイプの閉鎖で受け取る。
    std::thread::spawn(move || {
        let _ = io::stdin().read(&mut [0]);
        let _ = stop.send(());
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "ワーカーの実行環境を起動できませんでした")?;
    let status = runtime.block_on(worker_supervisor::run_configured(
        config,
        stopped,
        |_| {},
        || true,
        || Some(()),
    ))?;
    println!(
        "{}",
        serde_json::to_string(&status).map_err(|_| "状態を返せませんでした")?
    );
    Ok(())
}
