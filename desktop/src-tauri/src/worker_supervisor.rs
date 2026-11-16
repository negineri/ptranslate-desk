//! 参加中に、1件ずつ処理するエージェントを起動・回収する。
#![allow(dead_code)]
use crate::worker_agent::Bootstrap;
pub(crate) use ptranslate_core::worker_supervisor::{run_configured, Status};
use serde::Serialize;
use std::{sync::Mutex, time::Duration};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tokio::sync::{oneshot, watch, Mutex as AsyncMutex, MutexGuard};

const INITIAL_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    #[default]
    Idle,
    Starting,
    Waiting,
    Working,
    Paused,
    Quarantined,
    Error,
    Reauthorize,
}

#[derive(Clone, Copy, Default, Serialize)]
pub(crate) struct Activity {
    phase: Phase,
    completed: u64,
    enabled: bool,
}

impl Activity {
    fn finish(
        &mut self,
        result: &Result<Status, String>,
        retry: &mut Duration,
    ) -> Option<Duration> {
        let (phase, delay) = match result {
            Ok(Status::Completed) => {
                self.completed = self.completed.saturating_add(1);
                *retry = INITIAL_RETRY;
                (Phase::Idle, Duration::from_millis(250))
            }
            Ok(Status::Waiting) => {
                *retry = INITIAL_RETRY;
                (Phase::Idle, Duration::from_millis(250))
            }
            Ok(Status::Paused) => (Phase::Paused, Duration::from_secs(2)),
            Ok(Status::Quarantined) => {
                *retry = INITIAL_RETRY;
                (Phase::Quarantined, Duration::from_secs(2))
            }
            Ok(Status::Rejected | Status::PreparationFailed) => {
                self.phase = Phase::Error;
                self.enabled = false;
                return None;
            }
            _ => {
                let delay = *retry;
                *retry = (delay * 2).min(MAX_RETRY);
                (Phase::Error, delay)
            }
        };
        self.phase = phase;
        Some(delay)
    }
}

#[tauri::command]
pub(crate) fn worker_activity(app: AppHandle, window: WebviewWindow) -> Result<Activity, String> {
    if window.label() != "main" {
        return Err("メインウィンドウで操作してください".into());
    }
    Ok(app.state::<Lifecycle>().state.lock().unwrap().activity)
}

#[derive(Default)]
pub(crate) struct Lifecycle {
    changes: AsyncMutex<()>,
    state: Mutex<LifecycleState>,
}

#[derive(Default)]
struct LifecycleState {
    current: Option<Running>,
    participation: Option<Running>,
    shutting_down: bool,
    activity: Activity,
}

struct Running {
    stop: Option<oneshot::Sender<()>>,
    finished: watch::Receiver<()>,
}

impl Running {
    fn stop(&mut self) -> watch::Receiver<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.finished.clone()
    }
}

pub(crate) struct RunLease<'a> {
    lifecycle: &'a Lifecycle,
    _finished: watch::Sender<()>,
}

impl Drop for RunLease<'_> {
    fn drop(&mut self) {
        self.lifecycle.state.lock().unwrap().current.take();
    }
}

impl Lifecycle {
    pub(crate) async fn begin(&self) -> Result<(RunLease<'_>, oneshot::Receiver<()>), String> {
        let _change = self.changes.lock().await;
        let mut state = self.state.lock().unwrap();
        if state.shutting_down {
            return Err("アプリを終了しています".into());
        }
        if state.current.is_some() {
            return Err("翻訳ワーカーはすでに動いています".into());
        }
        let (stop, requested) = oneshot::channel();
        let (done, finished) = watch::channel(());
        state.current = Some(Running {
            stop: Some(stop),
            finished,
        });
        Ok((
            RunLease {
                lifecycle: self,
                _finished: done,
            },
            requested,
        ))
    }

    // 停止待ちと設定変更を同じガードで守り、途中に次の子が起動しないようにする。
    pub(crate) async fn stop_for_engine_change(&self) -> MutexGuard<'_, ()> {
        let change = self.changes.lock().await;
        let finished = Self::request_stop(&mut self.state.lock().unwrap());
        Self::wait_for_exit(finished).await;
        change
    }

    fn request_stop(state: &mut LifecycleState) -> Option<watch::Receiver<()>> {
        state.current.as_mut().map(Running::stop)
    }

    pub(crate) async fn stop_participation(&self) {
        let finished = {
            let mut state = self.state.lock().unwrap();
            let finished = state.participation.as_mut().map(Running::stop);
            Self::request_stop(&mut state);
            finished
        };
        Self::wait_for_exit(finished).await;
    }

    // 終了イベントから同期的に呼び、新しい子の登録と終了開始の競合を防ぐ。
    pub(crate) fn begin_shutdown(&self) -> (bool, Option<watch::Receiver<()>>) {
        let mut state = self.state.lock().unwrap();
        let first = !std::mem::replace(&mut state.shutting_down, true);
        let current = Self::request_stop(&mut state);
        let participation = state.participation.as_mut().map(Running::stop);
        (first, participation.or(current))
    }

    pub(crate) async fn wait_for_exit(finished: Option<watch::Receiver<()>>) {
        if let Some(mut finished) = finished {
            let _ = finished.changed().await;
        }
    }
}

fn publish(app: &AppHandle, activity: &Activity) {
    let _ = app.emit_to("main", "worker-activity", activity);
}

pub(crate) fn start_participation(
    app: AppHandle,
    mut expires_at: u64,
    config: Bootstrap,
) -> Result<(), String> {
    let (stop, mut stopped) = oneshot::channel();
    let (done, finished) = watch::channel(());
    let lifecycle = app.state::<Lifecycle>();
    {
        let mut state = lifecycle.state.lock().unwrap();
        if state.shutting_down {
            return Err("アプリを終了しています".into());
        }
        if state.participation.is_some() {
            return Ok(());
        }
        state.participation = Some(Running {
            stop: Some(stop),
            finished,
        });
        state.activity.enabled = true;
        state.activity.phase = Phase::Starting;
        publish(&app, &state.activity);
    }
    tauri::async_runtime::spawn(async move {
        let lifecycle = app.state::<Lifecycle>();
        let update = |change: &dyn Fn(&mut Activity)| {
            let mut state = lifecycle.state.lock().unwrap();
            change(&mut state.activity);
            publish(&app, &state.activity);
        };
        let mut retry = INITIAL_RETRY;
        let mut next_renewal = 0;
        let phase = loop {
            if !matches!(stopped.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
                break Phase::Idle;
            }
            let Ok(now) = crate::worker_registration::now_seconds() else {
                break Phase::Error;
            };
            if now >= next_renewal || now >= expires_at {
                use crate::worker_registration::AutoRenewal;
                match crate::worker_registration::auto_renew(&app, &config.broker).await {
                    Ok(AutoRenewal::Renewed {
                        expires_at: expiry,
                        retry_after,
                    }) => {
                        expires_at = expiry;
                        next_renewal = now + retry_after;
                    }
                    Ok(AutoRenewal::Reauthorize) => break Phase::Reauthorize,
                    Ok(AutoRenewal::Rejected) => break Phase::Error,
                    Ok(AutoRenewal::Busy) | Err(_) if now < expires_at => {
                        next_renewal = (now + 60).min(expires_at);
                    }
                    Ok(AutoRenewal::Busy) => {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                    Err(_) => break Phase::Error,
                }
            }
            let (running, stop) = tokio::select! {
                biased;
                _ = &mut stopped => break Phase::Idle,
                run = lifecycle.begin() => match run {
                    Ok(run) => run,
                    Err(_) => break Phase::Idle,
                },
            };
            // begin が停止の要求と競合しても、次の子へ資格を渡さない。
            if !matches!(stopped.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
                break Phase::Idle;
            }
            let result = run_one(&app, config.clone(), stop, |phase| {
                update(&|state| {
                    if state.phase != Phase::Quarantined || phase == Phase::Working {
                        state.phase = phase;
                    }
                });
            })
            .await;
            drop(running);
            let delay = {
                let mut state = lifecycle.state.lock().unwrap();
                let delay = state.activity.finish(&result, &mut retry);
                publish(&app, &state.activity);
                delay
            };
            let Some(delay) = delay else {
                break Phase::Error;
            };
            tokio::select! {
                biased;
                _ = &mut stopped => break Phase::Idle,
                _ = tokio::time::sleep(delay) => {}
            }
        };
        drop(config);
        let mut state = lifecycle.state.lock().unwrap();
        state.activity.enabled = false;
        state.activity.phase = phase;
        state.participation.take();
        publish(&app, &state.activity);
        drop(done);
    });
    Ok(())
}

async fn run_one(
    app: &AppHandle,
    mut config: Bootstrap,
    mut stop: oneshot::Receiver<()>,
    mut update: impl FnMut(Phase),
) -> Result<Status, String> {
    let local = app.state::<crate::local::Local>();
    let can_wait = || {
        crate::worker_power::external_power() == Some(true)
            && !local.has_translation_activity()
            && !app
                .state::<crate::Backend>()
                .session
                .lock()
                .unwrap()
                .has_pending_translation()
    };
    if !can_wait() {
        return Ok(Status::Paused);
    }
    update(Phase::Starting);
    let (engine, _standby) = tokio::select! {
        biased;
        _ = &mut stop => return Ok(Status::Paused),
        engine = local.prepare_worker_engine(app) => match engine {
            Ok(engine) => engine,
            Err(crate::local::WorkerPreparationError::Busy) => return Ok(Status::Paused),
            Err(crate::local::WorkerPreparationError::Unavailable) => return Ok(Status::PreparationFailed),
            Err(crate::local::WorkerPreparationError::Startup(error)) => return Err(error),
        },
    };
    if !can_wait() {
        return Ok(Status::Paused);
    }
    engine.configure(&mut config);
    run_configured(
        config,
        stop,
        |status| match status {
            Status::Waiting => update(Phase::Waiting),
            Status::Working => update(Phase::Working),
            _ => {}
        },
        can_wait,
        || local.try_worker_slot(&engine),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabling_participation_stops_the_child_and_waits_for_the_driver() {
        let lifecycle = Lifecycle::default();
        let (stop, stopped) = oneshot::channel();
        let (done, finished) = watch::channel(());
        lifecycle.state.lock().unwrap().participation = Some(Running {
            stop: Some(stop),
            finished,
        });
        let (child, child_stopped) = lifecycle.begin().await.unwrap();
        let disable = lifecycle.stop_participation();
        tokio::pin!(disable);
        tokio::select! {
            _ = &mut disable => panic!("子の回収前に提供を停止した"),
            result = child_stopped => result.unwrap(),
        }
        stopped.await.unwrap();
        drop(child);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut disable)
                .await
                .is_err()
        );
        drop(done);
        disable.await;
    }

    #[tokio::test]
    async fn engine_changes_wait_for_cleanup_and_block_the_next_run() {
        let lifecycle = Lifecycle::default();
        let (running, stopped) = lifecycle.begin().await.unwrap();
        assert!(lifecycle.begin().await.is_err());
        let change = lifecycle.stop_for_engine_change();
        tokio::pin!(change);
        tokio::select! {
            result = &mut change => { drop(result); panic!("回収前に変更を許可した"); },
            result = stopped => result.unwrap(),
        }
        drop(running);
        let change = change.await;
        assert!(lifecycle.changes.try_lock().is_err());
        drop(change);
        assert!(lifecycle.begin().await.is_ok());
    }

    #[tokio::test]
    async fn shutdown_stops_the_current_run_and_rejects_new_runs() {
        let lifecycle = Lifecycle::default();
        let (run, stop) = lifecycle.begin().await.unwrap();
        let (first, finished) = lifecycle.begin_shutdown();
        assert!(first);
        stop.await.unwrap();
        let waiting = Lifecycle::wait_for_exit(finished);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut waiting)
                .await
                .is_err()
        );
        assert!(!lifecycle.begin_shutdown().0);
        drop(run);
        waiting.await;
        assert!(lifecycle.begin().await.is_err());
        assert!(lifecycle.begin_shutdown().1.is_none());
    }

    #[test]
    fn activity_counts_only_completed_assignments_and_exposes_no_details() {
        let mut activity = Activity::default();
        let mut retry = INITIAL_RETRY;
        activity.finish(&Ok(Status::Paused), &mut retry);
        assert_eq!(
            serde_json::to_value(activity).unwrap(),
            serde_json::json!({"phase":"paused","completed":0,"enabled":false})
        );
        activity.finish(&Ok(Status::Completed), &mut retry);
        activity.finish(&Err("private detail".into()), &mut retry);
        assert_eq!(
            serde_json::to_value(activity).unwrap(),
            serde_json::json!({"phase":"error","completed":1,"enabled":false})
        );
    }

    #[test]
    fn retries_transient_failures_until_a_normal_result_or_rejection() {
        let mut activity = Activity {
            enabled: true,
            ..Activity::default()
        };
        let mut retry = INITIAL_RETRY;
        for normal in [Status::Completed, Status::Waiting, Status::Quarantined] {
            for seconds in [5, 10, 20, 40, 80, 160, 300, 300] {
                let result = if seconds == 5 {
                    Ok(Status::QuickWaiting)
                } else {
                    Ok(Status::Error)
                };
                assert_eq!(
                    activity.finish(&result, &mut retry),
                    Some(Duration::from_secs(seconds))
                );
                assert!(activity.enabled);
                assert!(activity.phase == Phase::Error);
            }
            assert!(activity.finish(&Ok(normal), &mut retry).is_some());
            assert_eq!(retry, INITIAL_RETRY);
        }
        for status in [Status::Rejected, Status::PreparationFailed] {
            activity.enabled = true;
            assert_eq!(activity.finish(&Ok(status), &mut retry), None);
            assert!(!activity.enabled);
            assert!(activity.phase == Phase::Error);
        }
    }
}
