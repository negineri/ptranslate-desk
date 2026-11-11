//! 1件だけ動くエージェントの親側。権限制限を整えるまで画面からは起動しない。
#![allow(dead_code)]
use crate::worker_agent::Bootstrap;
use serde::Deserialize;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::oneshot,
    time::timeout,
};
use zeroize::Zeroizing;

const LIFETIME: Duration = Duration::from_secs(170);
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
    mut command: Command,
    config: &Bootstrap,
    stop: oneshot::Receiver<()>,
    update: impl FnMut(Status),
) -> Result<Status, ()> {
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
    )
    .await
}

async fn supervise(
    command: &mut Command,
    input: Zeroizing<Vec<u8>>,
    mut stop: oneshot::Receiver<()>,
    mut update: impl FnMut(Status),
    lifetime: Duration,
    grace: Duration,
) -> Result<Status, ()> {
    let mut child = command.kill_on_drop(true).spawn().map_err(|_| ())?;
    let mut stdin = child.stdin.take().ok_or(())?;
    let stdout = child.stdout.take().ok_or(())?;
    let work = async {
        stdin.write_all(&input).await.map_err(|_| ())?;
        stdin.flush().await.map_err(|_| ())?;
        drop(input);
        let mut reader = BufReader::new(stdout);
        let mut last = None;
        loop {
            // 改行のない出力でも、1行の上限を超えて読み込まない。
            let mut line = Vec::new();
            let count = (&mut reader)
                .take((MAX_LINE + 1) as u64)
                .read_until(b'\n', &mut line)
                .await
                .map_err(|_| ())?;
            if count == 0 {
                return match last {
                    Some(Status::Working) | None => Err(()),
                    Some(status) => Ok(status),
                };
            }
            if count > MAX_LINE || !line.ends_with(b"\n") {
                return Err(());
            }
            let event: Update = serde_json::from_slice(&line).map_err(|_| ())?;
            match (last, event.status) {
                (None, Status::Working) => update(Status::Working),
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
        )
        .await
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn accepts_only_bounded_status_and_successful_exit() {
        for (script, expected) in [
            ("read config; printf '%s\\n' '{\"status\":\"working\"}' '{\"status\":\"completed\"}'", Ok(Status::Completed)),
            ("read config; printf '%s\\n' '{\"status\":\"waiting\"}'", Ok(Status::Waiting)),
            ("read config; printf '%s\\n' '{\"status\":\"working\"}'", Err(())),
            ("read config; printf '%s\\n' '{\"status\":\"waiting\",\"text\":\"x\"}'", Err(())),
            ("read config; printf '%s\\n' '{\"status\":\"waiting\"}'; exit 1", Err(())),
            ("read config; printf '%080d' 0", Err(())),
        ] {
            let (_tx, rx) = oneshot::channel();
            assert_eq!(mock(script, rx, |_| {}).await, expected);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_closes_input_and_unresponsive_child_is_reaped() {
        let working = "read config; printf '%s\\n' '{\"status\":\"working\"}'; ";
        for (after_work, expected) in [
            ("if read extra; then exit 1; fi", Ok(Status::Paused)),
            ("while :; do :; done", Err(())),
        ] {
            let (tx, rx) = oneshot::channel();
            let mut tx = Some(tx);
            let result = mock(&format!("{working}{after_work}"), rx, |status| {
                assert_eq!(status, Status::Working);
                tx.take().unwrap().send(()).unwrap();
            })
            .await;
            assert_eq!(result, expected);
        }
    }
}
