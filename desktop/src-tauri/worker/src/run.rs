//! 提供を続ける。資格を自動で更新し、枠ごとに制限付きのワーカーを起動する。
use crate::{
    account::{now, refresh, Renewal},
    engine::{Installation, Managed, Remote, Target},
    http::{Failure, Result},
    state::{State, StateDir},
};
use ptranslate_core::{
    worker_agent::Bootstrap,
    worker_supervisor::{run_configured, Status},
};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{oneshot, watch},
    task::JoinSet,
    time::Instant,
};

const INITIAL_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(300);

pub enum Engine {
    /// 自動で取得した推論エンジンを、このプロセスが起動する。
    Managed {
        install: Installation,
        threads: Option<usize>,
    },
    /// 既存のローカルの llama-server。
    Local(Target),
    /// 別のサーバーの llama-server。ループバックの中継を通す。
    Remote(Arc<Remote>),
}

/// 提供のバージョン。配布のビルドでは `PTRANSLATE_VERSION` で渡す。
pub fn version() -> &'static str {
    option_env!("PTRANSLATE_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

fn agent_version() -> String {
    let version = version().trim_start_matches('v');
    version.split('-').next().unwrap_or(version).to_owned()
}

/// 停止の要求 (SIGTERM・SIGINT) を受け取る。
pub fn stop_signal() -> Result<watch::Receiver<bool>> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate())
        .map_err(|_| Failure::new("停止の要求を受け取れませんでした"))?;
    let mut interrupt = signal(SignalKind::interrupt())
        .map_err(|_| Failure::new("停止の要求を受け取れませんでした"))?;
    let (sender, receiver) = watch::channel(false);
    tokio::spawn(async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = interrupt.recv() => {}
        }
        let _ = sender.send(true);
    });
    Ok(receiver)
}

pub async fn run(
    client: &reqwest::Client,
    dir: &StateDir,
    mut state: State,
    engine: Engine,
    slots: u16,
    mut stop: watch::Receiver<bool>,
    log: impl Fn(&str),
) -> Result<()> {
    if state.refresh_token.is_none() || state.worker_id.is_none() {
        return Err(Failure::new("authorize で提供先を認可してください"));
    }
    let mut managed = None;
    let (mut target, remote) = match engine {
        Engine::Managed { install, threads } => {
            log("推論エンジンを起動しています");
            let engine = Managed::start(&install, threads, slots).await?;
            let target = clone_target(&engine.target);
            managed = Some((engine, install, threads));
            (target, None)
        }
        Engine::Local(target) => (target, None),
        Engine::Remote(remote) => {
            remote.verify().await?;
            let target = remote.clone().relay().await?;
            (target, Some(remote))
        }
    };
    let mut next_attempt = 0;
    let mut workers = JoinSet::new();
    let mut retry = vec![INITIAL_RETRY; usize::from(slots)];
    let mut ready_at = vec![Some(Instant::now()); usize::from(slots)];
    log("提供を開始しました");
    while !*stop.borrow() {
        let now = now();
        if now >= next_attempt || now >= state.expires_at {
            match refresh(client, dir, &mut state).await {
                Ok(Renewal::Renewed) => next_attempt = state.renew_at,
                Ok(Renewal::Reauthorize) => {
                    return Err(Failure::new(
                        "提供先の再認可が必要です。authorize を実行してください",
                    ))
                }
                Ok(Renewal::Rejected) => {
                    return Err(Failure::new("提供先が資格の更新を拒否しました"))
                }
                Err(_) if now >= state.expires_at => {
                    return Err(Failure::new("ワーカー資格が期限切れで更新できません"))
                }
                Err(_) => {
                    next_attempt = now + 60;
                    log("資格を更新できませんでした。再試行します。");
                }
            }
        }
        if *stop.borrow() {
            break;
        }
        let idle = ready_at
            .iter()
            .enumerate()
            .filter_map(|(slot, ready)| ready.filter(|time| *time <= Instant::now()).map(|_| slot))
            .collect::<Vec<_>>();
        if !idle.is_empty() {
            // 推論先が使えないときは、ジョブを取りに行かない。
            let ready = if let Some(remote) = &remote {
                match remote.verify().await {
                    Ok(()) => true,
                    Err(e) if e.fatal => return Err(e),
                    Err(_) => false,
                }
            } else if let Some((engine, install, threads)) = &mut managed {
                if engine.alive() {
                    true
                } else {
                    log("推論エンジンが終了したため、起動し直します");
                    match Managed::start(install, *threads, slots).await {
                        Ok(restarted) => {
                            target = clone_target(&restarted.target);
                            *engine = restarted;
                            true
                        }
                        Err(e) if e.fatal => return Err(e),
                        Err(_) => false,
                    }
                }
            } else {
                true
            };
            for slot in idle {
                ready_at[slot] = if ready {
                    let config = bootstrap(&state, &target, slot as u16);
                    let mut worker_stop = stop.clone();
                    workers.spawn(async move { (slot, run_once(config, &mut worker_stop).await) });
                    None
                } else {
                    let delay = retry[slot];
                    retry[slot] = (delay * 2).min(MAX_RETRY);
                    Some(Instant::now() + delay)
                };
            }
            if !ready {
                log("推論先を確認できないため、ジョブの受付を待機します。");
            }
        }
        let (slot, result) = tokio::select! {
            _ = stop.wait_for(|stopped| *stopped) => break,
            _ = tokio::time::sleep(Duration::from_millis(250)) => continue,
            completed = workers.join_next(), if !workers.is_empty() => {
                completed
                    .expect("実行中のワーカーがある")
                    .map_err(|_| Failure::new("ワーカーの終了を確認できませんでした"))?
            }
        };
        let retry = &mut retry[slot];
        let delay = match result {
            Ok(Status::Rejected) => return Err(Failure::new("提供先がワーカー資格を拒否しました")),
            Ok(Status::Completed | Status::Waiting) => {
                *retry = INITIAL_RETRY;
                Duration::from_millis(250)
            }
            Ok(Status::Quarantined | Status::Paused) => {
                *retry = INITIAL_RETRY;
                Duration::from_secs(2)
            }
            other => {
                if !*stop.borrow() {
                    log(&match other {
                        Ok(Status::QuickWaiting) => {
                            "提供先が混み合っています。間をおいて再試行します。".to_owned()
                        }
                        Err(reason) if !reason.is_empty() => {
                            format!("ワーカーの処理を続けられませんでした ({reason})。再試行します。")
                        }
                        Ok(Status::Error) => {
                            "ワーカーが提供先または推論エンジンと通信できませんでした。再試行します。"
                                .to_owned()
                        }
                        _ => "ワーカーの処理を続けられませんでした。再試行します。".to_owned(),
                    });
                }
                let delay = *retry;
                *retry = (delay * 2).min(MAX_RETRY);
                delay
            }
        };
        ready_at[slot] = Some(Instant::now() + delay);
    }
    while workers.join_next().await.is_some() {}
    log("提供を停止しました");
    Ok(())
}

fn clone_target(target: &Target) -> Target {
    Target {
        port: target.port,
        secret: target.secret.clone(),
        model_sha256: target.model_sha256.clone(),
    }
}

fn bootstrap(state: &State, target: &Target, slot: u16) -> Bootstrap {
    Bootstrap {
        slot,
        broker: state.broker.clone(),
        worker_secret: state.worker_secret.clone().unwrap_or_default(),
        model_sha256: target.model_sha256.clone(),
        agent_version: agent_version(),
        engine_port: target.port,
        engine_secret: target.secret.as_str().to_owned(),
        relay_port: None,
        relay_secret: None,
    }
}

async fn run_once(
    config: Bootstrap,
    stop: &mut watch::Receiver<bool>,
) -> std::result::Result<Status, String> {
    let (sender, receiver) = oneshot::channel();
    let run = run_configured(config, receiver, |_| {}, || true, || Some(()));
    tokio::pin!(run);
    let mut sender = Some(sender);
    loop {
        tokio::select! {
            result = &mut run => return result,
            _ = stop.wait_for(|stopped| *stopped), if sender.is_some() => {
                if let Some(sender) = sender.take() {
                    let _ = sender.send(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_version_has_three_numbers() {
        let version = agent_version();
        assert_eq!(version.split('.').count(), 3, "{version}");
        assert!(version.split('.').all(|n| n.parse::<u32>().is_ok()));
    }
}
