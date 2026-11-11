//! 1件だけ動くエージェントの親側。権限制限を整えるまで画面からは起動しない。
#![allow(dead_code)]
use crate::worker_agent::{Bootstrap, ASSIGNED_LINE, READY_LINE, START_COMMAND};
use serde::Deserialize;
use std::{process::Stdio, time::Duration};
use tauri::{AppHandle, Manager};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::oneshot,
    time::timeout,
};
use zeroize::Zeroizing;

const LIFETIME: Duration = Duration::from_secs(170);
const ADMISSION_CHECK_INTERVAL: Duration = Duration::from_millis(250);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_GRACE: Duration = Duration::from_secs(6);
const MAX_LINE: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    Working,
    Completed,
    Waiting,
    Paused,
    Error,
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

/// commandのOS権限制限は呼び出し側で設定する。資格を引数や環境変数に入れない。
pub(crate) async fn run(
    app: &AppHandle,
    mut command: Command,
    config: &Bootstrap,
    stop: oneshot::Receiver<()>,
    update: impl FnMut(Status),
) -> Result<Status, ()> {
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
    let mut input = Zeroizing::new(serde_json::to_vec(config).map_err(|_| ())?);
    input.push(b'\n');
    if input.len() > 16 * 1024 {
        return Err(());
    }
    supervise(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
        input,
        stop,
        update,
        LIFETIME,
        STOP_GRACE,
        can_wait,
        || local.try_worker_slot(),
    )
    .await
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
                (None, Status::Waiting | Status::Paused | Status::Error)
                | (Some(Status::Working), Status::Completed | Status::Paused | Status::Error) => {}
                _ => return Err(()),
            }
            last = Some(event.status);
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
            ("read config; printf '%s\\n' '{\"status\":\"waiting\"}'", Ok(Status::Waiting)),
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
            mock(&script, rx, |_| powered.set(false), || powered.get()).await,
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
                |_| panic!("unexpected working status"),
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
                    assert_eq!(status, Status::Working);
                    tx.take().unwrap().send(()).unwrap();
                },
                || true,
            )
            .await;
            assert_eq!(result, expected);
        }
    }
}
