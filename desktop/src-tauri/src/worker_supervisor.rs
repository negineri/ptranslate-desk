//! 参加中に、1件ずつ処理するエージェントを起動・回収する。
#![allow(dead_code)]
use crate::worker_agent::{Bootstrap, ASSIGNED_LINE, READY_LINE, START_COMMAND};
use crate::worker_sandbox::Ports;
use serde::{Deserialize, Serialize};
use std::{process::Stdio, sync::Mutex, time::Duration};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{oneshot, watch, Mutex as AsyncMutex, MutexGuard},
    time::timeout,
};
use zeroize::Zeroizing;

const LIFETIME: Duration = Duration::from_secs(170);
const ADMISSION_CHECK_INTERVAL: Duration = Duration::from_millis(250);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_GRACE: Duration = Duration::from_secs(6);
const MAX_LINE: usize = 64;
const INITIAL_RETRY: Duration = Duration::from_secs(5);
const MAX_RETRY: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Working,
    Completed,
    Waiting,
    Paused,
    Quarantined,
    Error,
    Rejected,
    #[serde(skip)]
    QuickWaiting,
    #[serde(skip)]
    PreparationFailed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    status: Status,
}

/// 外部電源を確認できないときも受付を止める。実行中の区切りは中断しない。
pub(crate) fn can_accept(
    enabled: bool,
    external_power: Option<bool>,
    own_requests: usize,
    running: bool,
) -> bool {
    enabled && external_power == Some(true) && own_requests == 0 && !running
}

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

pub(crate) fn start_participation(app: AppHandle, config: Bootstrap) -> Result<(), String> {
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
        let failed = loop {
            let (running, stop) = tokio::select! {
                biased;
                _ = &mut stopped => break false,
                run = lifecycle.begin() => match run {
                    Ok(run) => run,
                    Err(_) => break false,
                },
            };
            // begin が停止の要求と競合しても、次の子へ資格を渡さない。
            if !matches!(stopped.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
                break false;
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
            let Some(delay) = delay else { break true };
            tokio::select! {
                biased;
                _ = &mut stopped => break false,
                _ = tokio::time::sleep(delay) => {}
            }
        };
        drop(config);
        let mut state = lifecycle.state.lock().unwrap();
        state.activity.enabled = false;
        state.activity.phase = if failed { Phase::Error } else { Phase::Idle };
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
    let broker =
        crate::worker_agent::broker_origin(&config.broker).map_err(|_| "提供先のURLが不正です")?;
    let relay = crate::worker_relay::BrokerRelay::start(&broker).await?;
    config.relay_port = Some(relay.port);
    config.relay_secret = Some(relay.secret.as_str().to_owned());
    engine.configure(&mut config);
    let ports =
        Ports::new(relay.port, config.engine_port).map_err(|_| "ワーカーの接続ポートが不正です")?;
    let executable =
        std::env::current_exe().map_err(|_| "ワーカーの実行ファイルを確認できませんでした")?;
    let mut command = Command::new(executable);
    // 資格は準備完了の通知を待ってから標準入力で渡す。
    command
        .arg("--worker-agent")
        .arg(ports.relay.to_string())
        .arg(ports.engine.to_string())
        .env_clear();
    let mut input = Zeroizing::new(
        serde_json::to_vec(&config).map_err(|_| "ワーカーの起動情報を作れませんでした")?,
    );
    drop(config);
    input.push(b'\n');
    if input.len() > 16 * 1024 {
        return Err("ワーカーの起動情報が大きすぎます".into());
    }
    supervise(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
        input,
        stop,
        |status| match status {
            Status::Waiting => update(Phase::Waiting),
            Status::Working => update(Phase::Working),
            _ => {}
        },
        LIFETIME,
        STOP_GRACE,
        can_wait,
        || local.try_worker_slot(&engine),
    )
    .await
    .map_err(|_| "ワーカーの起動または処理の終了を確認できませんでした".into())
}

async fn read_line(reader: &mut (impl AsyncBufReadExt + Unpin)) -> Result<Vec<u8>, ()> {
    let mut line = Vec::new();
    reader
        .take((MAX_LINE + 1) as u64)
        .read_until(b'\n', &mut line)
        .await
        .map_err(|_| ())?;
    if !line.is_empty() && (line.len() > MAX_LINE || !line.ends_with(b"\n")) {
        return Err(());
    }
    Ok(line)
}

async fn supervise<Permit>(
    command: &mut Command,
    input: Zeroizing<Vec<u8>>,
    mut stop: oneshot::Receiver<()>,
    mut update: impl FnMut(Status),
    lifetime: Duration,
    grace: Duration,
    mut can_wait: impl FnMut() -> bool,
    reserve: impl FnOnce() -> Option<Permit>,
) -> Result<Status, ()> {
    if !matches!(stop.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
        return Ok(Status::Paused);
    }
    let started = tokio::time::Instant::now();
    let mut child = command.kill_on_drop(true).spawn().map_err(|_| ())?;
    let mut stdin = child.stdin.take().ok_or(())?;
    let stdout = child.stdout.take().ok_or(())?;
    // 子の停止・回収まで保持する。停止要求で work が中断されても先に枠を返さない。
    let mut permit = None;
    let work = async {
        let mut reader = BufReader::new(stdout);
        if timeout(STARTUP_TIMEOUT, read_line(&mut reader))
            .await
            .map_err(|_| ())??
            != READY_LINE
        {
            return Err(());
        }
        if !can_wait() {
            return Ok(Status::Paused);
        }
        stdin.write_all(&input).await.map_err(|_| ())?;
        stdin.flush().await.map_err(|_| ())?;
        drop(input);
        update(Status::Waiting);
        let mut last = None;
        let mut reserve = Some(reserve);
        let mut admission_check = tokio::time::interval_at(
            tokio::time::Instant::now() + ADMISSION_CHECK_INTERVAL,
            ADMISSION_CHECK_INTERVAL,
        );
        admission_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let line = if permit.is_none() && last.is_none() {
                // 同じ読み取りを保持し、受付条件の確認のたびに途中の通知を捨てない。
                let pending = read_line(&mut reader);
                tokio::pin!(pending);
                loop {
                    tokio::select! {
                        biased;
                        line = &mut pending => break line?,
                        _ = admission_check.tick() => {
                            if !can_wait() {
                                return Ok(Status::Paused);
                            }
                        }
                    }
                }
            } else {
                read_line(&mut reader).await?
            };
            if line.is_empty() {
                return match last {
                    Some(Status::Working) | None => Err(()),
                    Some(status) => Ok(status),
                };
            }
            if line == ASSIGNED_LINE {
                if permit.is_some() || last.is_some() {
                    return Err(());
                }
                if !can_wait() {
                    return Ok(Status::Paused);
                }
                permit = reserve.take().ok_or(())?();
                if permit.is_none() {
                    return Ok(Status::Paused);
                }
                stdin.write_all(START_COMMAND).await.map_err(|_| ())?;
                stdin.flush().await.map_err(|_| ())?;
                continue;
            }
            let event: Update = serde_json::from_slice(&line).map_err(|_| ())?;
            match (last, event.status) {
                (None, Status::Working) if permit.is_some() => update(Status::Working),
                (
                    None,
                    Status::Waiting
                    | Status::Paused
                    | Status::Quarantined
                    | Status::Error
                    | Status::Rejected,
                )
                | (
                    Some(Status::Working),
                    Status::Completed | Status::Paused | Status::Error | Status::Rejected,
                ) => {}
                _ => return Err(()),
            }
            last = Some(
                if event.status == Status::Waiting && started.elapsed() < Duration::from_secs(1) {
                    Status::QuickWaiting
                } else {
                    event.status
                },
            );
        }
    };
    let result = tokio::select! {
        biased;
        _ = &mut stop => Ok(Status::Paused),
        result = timeout(lifetime, work) => result.unwrap_or(Err(())),
    };
    // EOFで実行中のエラー返送を促し、期限内に終わらなければ強制終了して回収する。
    drop(stdin);
    let exit = match timeout(grace, child.wait()).await {
        Ok(Ok(exit)) => exit,
        _ => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(());
        }
    };
    if !exit.success() {
        return Err(());
    }
    result
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

    #[test]
    fn admission_requires_power_idle_and_one_slot() {
        assert!(can_accept(true, Some(true), 0, false));
        assert!(!can_accept(false, Some(true), 0, false));
        assert!(!can_accept(true, None, 0, false));
        assert!(!can_accept(true, Some(false), 0, false));
        assert!(!can_accept(true, Some(true), 1, false));
        assert!(!can_accept(true, Some(true), 0, true));
    }

    #[cfg(unix)]
    async fn mock(
        script: &str,
        stop: oneshot::Receiver<()>,
        update: impl FnMut(Status),
        can_wait: impl FnMut() -> bool,
    ) -> Result<Status, ()> {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        supervise(
            &mut command,
            Zeroizing::new(b"{}\n".to_vec()),
            stop,
            update,
            Duration::from_secs(2),
            Duration::from_millis(200),
            can_wait,
            || Some(()),
        )
        .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn accepts_only_bounded_status_and_successful_exit() {
        let ready = "printf '%s\\n' '{\"status\":\"ready\"}'; ";
        for (script, expected) in [
            ("read config; printf '%s\\n' '{\"status\":\"assigned\"}'; read permission; test \"$permission\" = go || exit 1; printf '%s\\n' '{\"status\":\"working\"}' '{\"status\":\"completed\"}'", Ok(Status::Completed)),
            ("read config; printf '%s\\n' '{\"status\":\"waiting\"}'", Ok(Status::QuickWaiting)),
            ("read config; printf '%s\\n' '{\"status\":\"rejected\"}'", Ok(Status::Rejected)),
            ("read config; printf '%s\\n' '{\"status\":\"assigned\"}'; read permission; test \"$permission\" = go || exit 1; printf '%s\\n' '{\"status\":\"working\"}'", Err(())),
            ("read config; printf '%s\\n' '{\"status\":\"waiting\",\"text\":\"x\"}'", Err(())),
            ("read config; printf '%s\\n' '{\"status\":\"waiting\"}'; exit 1", Err(())),
            ("read config; printf '%080d' 0", Err(())),
        ] {
            let (_tx, rx) = oneshot::channel();
            assert_eq!(mock(&format!("{ready}{script}"), rx, |_| {}, || true).await, expected);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_error_or_power_loss_does_not_receive_bootstrap() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("received");
        for (status, powered, expected) in [
            ("error", true, Err(())),
            ("ready", false, Ok(Status::Paused)),
        ] {
            let (_tx, rx) = oneshot::channel();
            let mut command = Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    "printf '{\"status\":\"%s\"}\\n' \"$1\"; if read config; then : > \"$2\"; fi",
                    "worker",
                    status,
                ])
                .arg(&marker)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            assert_eq!(
                supervise(
                    &mut command,
                    Zeroizing::new(b"{}\n".to_vec()),
                    rx,
                    |_| {},
                    Duration::from_secs(2),
                    Duration::from_millis(200),
                    || powered,
                    || Some(()),
                )
                .await,
                expected
            );
            assert!(!marker.exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn power_loss_stops_waiting_but_does_not_stop_working() {
        let ready = "printf '%s\\n' '{\"status\":\"ready\"}'; read config; ";
        let (_tx, rx) = oneshot::channel();
        let mut checks = 0;
        assert_eq!(
            mock(
                &format!("{ready}if read extra; then exit 1; fi"),
                rx,
                |_| {},
                || {
                    checks += 1;
                    checks == 1
                }
            )
            .await,
            Ok(Status::Paused)
        );

        let (_tx, rx) = oneshot::channel();
        let powered = std::cell::Cell::new(true);
        let script = format!("{ready}printf '%s\\n' '{{\"status\":\"assigned\"}}'; read permission; test \"$permission\" = go || exit 1; printf '%s\\n' '{{\"status\":\"working\"}}'; sleep 1; printf '%s\\n' '{{\"status\":\"completed\"}}'");
        assert_eq!(
            mock(
                &script,
                rx,
                |status| {
                    if status == Status::Working {
                        powered.set(false);
                    }
                },
                || powered.get()
            )
            .await,
            Ok(Status::Completed)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn assignment_is_not_started_when_own_translation_arrives_before_permission() {
        let (_tx, rx) = oneshot::channel();
        let script = "printf '%s\\n' '{\"status\":\"ready\"}'; read config; printf '%s\\n' '{\"status\":\"assigned\"}'; if read permission; then exit 1; fi";
        let mut checks = 0;
        assert_eq!(
            mock(
                script,
                rx,
                |status| assert_ne!(status, Status::Working),
                || {
                    checks += 1;
                    can_accept(true, Some(true), usize::from(checks > 1), false)
                }
            )
            .await,
            Ok(Status::Paused)
        );
        assert_eq!(checks, 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_closes_input_and_unresponsive_child_is_reaped() {
        let working = "printf '%s\\n' '{\"status\":\"ready\"}'; read config; printf '%s\\n' '{\"status\":\"assigned\"}'; read permission; test \"$permission\" = go || exit 1; printf '%s\\n' '{\"status\":\"working\"}'; ";
        for (after_work, expected) in [
            ("if read extra; then exit 1; fi", Ok(Status::Paused)),
            ("while :; do :; done", Err(())),
        ] {
            let (tx, rx) = oneshot::channel();
            let mut tx = Some(tx);
            let result = mock(
                &format!("{working}{after_work}"),
                rx,
                |status| {
                    if status == Status::Working {
                        tx.take().unwrap().send(()).unwrap();
                    }
                },
                || true,
            )
            .await;
            assert_eq!(result, expected);
        }
    }
}
