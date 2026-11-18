//! 引数の読み取りと各コマンド。
use crate::{
    account,
    engine::{self, Remote, Target},
    http::{Failure, Result},
    run::{self, Engine},
    state::StateDir,
};
use std::{ffi::OsString, path::PathBuf, sync::Arc, time::Duration};

const USAGE: &str = "\
使い方:
  ptranslate-worker [--state DIR] authorize <提供先のURL> --accept-terms
  ptranslate-worker [--state DIR] [--data DIR] prepare
  ptranslate-worker [--state DIR] [--data DIR] run [--threads N] [--slots N]
  ptranslate-worker [--state DIR] run --engine-url <URL> --engine-key-file <FILE>
                                      [--allow-engine-revision-mismatch]
  ptranslate-worker [--state DIR] run --engine-port <PORT> --engine-key-file <FILE> --model <FILE>
  ptranslate-worker --version

authorize  提供先の規約に同意し、別の端末のブラウザーで認可してワーカーを登録する
prepare    推論エンジンと承認済みモデルを取得して検証する (run も自動で行う)
run        提供を始める。SIGTERM・SIGINT で止まる

--state    refresh token とワーカー資格の保存先 (既定 $XDG_STATE_HOME/ptranslate-worker)
--data     推論エンジンとモデルの保存先 (既定 $XDG_DATA_HOME/ptranslate-worker)
--slots    同時に提供する翻訳枠 (既定1)
--threads  推論エンジンの CPU スレッド数 (既定は llama.cpp の自動)
--engine-url       自分で用意した別のサーバーの llama-server (HTTPS) を使う
--engine-port      自分で用意したローカルの llama-server を使う
";

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    pub command: String,
    pub state: Option<PathBuf>,
    pub data: Option<PathBuf>,
    pub server: Option<String>,
    pub accept_terms: bool,
    pub threads: Option<usize>,
    pub slots: Option<u16>,
    pub engine_url: Option<String>,
    pub engine_port: Option<u16>,
    pub engine_key_file: Option<PathBuf>,
    pub model: Option<PathBuf>,
    pub allow_revision_mismatch: bool,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> std::result::Result<Args, String> {
    let mut parsed = Args::default();
    let mut args = args.into_iter();
    let text = |value: Option<OsString>, name: &str| {
        value
            .and_then(|v| v.into_string().ok())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{name} の値がありません"))
    };
    while let Some(arg) = args.next() {
        let arg = arg
            .into_string()
            .map_err(|_| "引数が不正です".to_string())?;
        match arg.as_str() {
            "--help" | "-h" => parsed.command = "help".into(),
            "--version" | "-V" => parsed.command = "version".into(),
            "--state" => parsed.state = Some(text(args.next(), "--state")?.into()),
            "--data" => parsed.data = Some(text(args.next(), "--data")?.into()),
            "--accept-terms" => parsed.accept_terms = true,
            "--allow-engine-revision-mismatch" => parsed.allow_revision_mismatch = true,
            "--slots" => {
                parsed.slots = Some(
                    text(args.next(), "--slots")?
                        .parse()
                        .ok()
                        .filter(|n| (1..=1000).contains(n))
                        .ok_or("--slots は1〜1000の整数にしてください")?,
                )
            }
            "--threads" => {
                parsed.threads = Some(
                    text(args.next(), "--threads")?
                        .parse()
                        .ok()
                        .filter(|n| (1..=1024).contains(n))
                        .ok_or("--threads が不正です")?,
                )
            }
            "--engine-url" => parsed.engine_url = Some(text(args.next(), "--engine-url")?),
            "--engine-port" => {
                parsed.engine_port = Some(
                    text(args.next(), "--engine-port")?
                        .parse()
                        .ok()
                        .filter(|p| *p != 0)
                        .ok_or("推論エンジンのポートが不正です")?,
                )
            }
            "--engine-key-file" => {
                parsed.engine_key_file = Some(text(args.next(), "--engine-key-file")?.into())
            }
            "--model" => parsed.model = Some(text(args.next(), "--model")?.into()),
            command @ ("authorize" | "prepare" | "run") if parsed.command.is_empty() => {
                parsed.command = command.into()
            }
            value
                if parsed.command == "authorize"
                    && parsed.server.is_none()
                    && !value.starts_with('-') =>
            {
                parsed.server = Some(value.into())
            }
            other => return Err(format!("不明な引数です: {other}")),
        }
    }
    match parsed.command.as_str() {
        "help" | "version" => {}
        "authorize" => {
            if parsed.server.is_none() {
                return Err("提供先の URL を指定してください".into());
            }
            if !parsed.accept_terms {
                return Err(
                    "提供先の規約を読んで同意したら --accept-terms を付けてください".into(),
                );
            }
        }
        "prepare" => {}
        "run" => {
            let remote = parsed.engine_url.is_some();
            let local = parsed.engine_port.is_some();
            if remote && local {
                return Err("--engine-url と --engine-port は同時に指定できません".into());
            }
            if (remote || local) != parsed.engine_key_file.is_some() {
                return Err(
                    "--engine-key-file は --engine-url か --engine-port と一緒に指定してください"
                        .into(),
                );
            }
            if local != parsed.model.is_some() {
                return Err("--model は --engine-port と一緒に指定してください".into());
            }
            if parsed.allow_revision_mismatch && !remote {
                return Err("--allow-engine-revision-mismatch には --engine-url が必要です".into());
            }
            if parsed.threads.is_some() && (remote || local) {
                return Err("--threads は自動で起動する推論エンジンだけに指定できます".into());
            }
        }
        "" => return Err("コマンドを指定してください (authorize・prepare・run)".into()),
        _ => unreachable!(),
    }
    if parsed.slots.is_some() && parsed.command != "run" {
        return Err("--slots は run で指定してください".into());
    }
    Ok(parsed)
}

fn default_dir(variable: &str, fallback: &str) -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(variable).filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir).join("ptranslate-worker"));
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Failure::new("HOME がないため --state と --data を指定してください"))?;
    Ok(PathBuf::from(home).join(fallback).join("ptranslate-worker"))
}

fn log(message: &str) {
    eprintln!("{message}");
}

pub fn main() -> i32 {
    let args = match parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            return 2;
        }
    };
    match args.command.as_str() {
        "help" => {
            print!("{USAGE}");
            return 0;
        }
        "version" => {
            println!("ptranslate-worker {}", run::version());
            return 0;
        }
        _ => {}
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("実行環境を起動できませんでした");
            return 1;
        }
    };
    match runtime.block_on(execute(args)) {
        Ok(()) => 0,
        Err(failure) => {
            eprintln!("ptranslate-worker: {failure}");
            1
        }
    }
}

async fn execute(args: Args) -> Result<()> {
    let state_path = match &args.state {
        Some(path) => path.clone(),
        None => default_dir("XDG_STATE_HOME", ".local/state")?,
    };
    let data = match &args.data {
        Some(path) => path.clone(),
        None => default_dir("XDG_DATA_HOME", ".local/share")?,
    };
    let client = crate::http::client(Duration::from_secs(15));
    match args.command.as_str() {
        "authorize" => {
            let dir = StateDir::open(&state_path)?;
            let server = args.server.as_deref().unwrap_or_default();
            account::authorize(&client, &dir, server, |uri, code| {
                println!("ブラウザーで {uri} を開き、コード {code} を入力してください。");
            })
            .await?;
            println!("提供先の認可と登録を保存しました。");
            Ok(())
        }
        "prepare" => {
            engine::prepare(&data, &log).await?;
            println!("推論エンジンとモデルを確認しました。");
            Ok(())
        }
        "run" => {
            let dir = StateDir::open(&state_path)?;
            let state = dir.load()?;
            if state.refresh_token.is_none() || state.worker_id.is_none() {
                return Err(Failure::new("authorize で提供先を認可してください"));
            }
            let stop = run::stop_signal()?;
            let engine = if let Some(url) = &args.engine_url {
                let key = engine::read_key(&args.engine_key_file.clone().unwrap_or_default())?;
                Engine::Remote(Arc::new(Remote::new(
                    url,
                    key,
                    args.allow_revision_mismatch,
                )?))
            } else if let Some(port) = args.engine_port {
                let key = engine::read_key(&args.engine_key_file.clone().unwrap_or_default())?;
                let model = args.model.clone().unwrap_or_default();
                let model_sha256 =
                    tokio::task::spawn_blocking(move || engine::model_digest(&model))
                        .await
                        .map_err(|_| Failure::new("モデルを読み込めませんでした"))??;
                Engine::Local(Target {
                    port,
                    secret: key,
                    model_sha256,
                })
            } else {
                Engine::Managed {
                    install: engine::prepare(&data, &log).await?,
                    threads: args.threads,
                }
            };
            run::run(
                &client,
                &dir,
                state,
                engine,
                args.slots.unwrap_or(1),
                stop,
                log,
            )
            .await
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> std::result::Result<Args, String> {
        parse(list.iter().map(OsString::from))
    }

    #[test]
    fn parses_the_simple_forms() {
        let parsed = args(&["authorize", "https://t.example", "--accept-terms"]).unwrap();
        assert_eq!(parsed.server.as_deref(), Some("https://t.example"));
        let parsed = args(&["--state", "/s", "run", "--threads", "4", "--slots", "2"]).unwrap();
        assert_eq!(parsed.slots, Some(2));
        assert_eq!((parsed.state, parsed.threads), (Some("/s".into()), Some(4)));
        let parsed = args(&[
            "run",
            "--engine-url",
            "https://gpu.example",
            "--engine-key-file",
            "/k",
            "--allow-engine-revision-mismatch",
        ])
        .unwrap();
        assert!(parsed.allow_revision_mismatch);
        assert!(args(&[
            "run",
            "--engine-port",
            "8080",
            "--engine-key-file",
            "/k",
            "--model",
            "/m"
        ])
        .is_ok());
        assert_eq!(args(&["--version"]).unwrap().command, "version");
    }

    #[test]
    fn rejects_incomplete_or_conflicting_options() {
        for list in [
            &[][..],
            &["authorize", "https://t.example"],
            &["authorize", "--accept-terms"],
            &["run", "--engine-url", "https://g.example"],
            &["run", "--engine-port", "8080", "--engine-key-file", "/k"],
            &[
                "run",
                "--engine-url",
                "https://g",
                "--engine-port",
                "1",
                "--engine-key-file",
                "/k",
            ],
            &["run", "--allow-engine-revision-mismatch"],
            &["run", "--model", "/m"],
            &["run", "--threads", "0"],
            &["run", "--slots", "0"],
            &["run", "--slots", "1001"],
            &["prepare", "--slots", "2"],
            &[
                "run",
                "--engine-port",
                "0",
                "--engine-key-file",
                "/k",
                "--model",
                "/m",
            ],
            &["run", "extra"],
            &["prepare", "--unknown"],
        ] {
            assert!(args(list).is_err(), "{list:?}");
        }
    }
}
