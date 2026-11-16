//! tools/run_worker_local_probe.pyからだけ明示的に起動する実モデル確認用の子。
use super::*;

#[test]
#[ignore = "ローカル仲介と取得済み実モデルが必要。通常のCIでは実行しない"]
fn restricted_worker() {
    #[cfg(target_os = "linux")]
    if let Ok(ports) = std::env::var("PTRANSLATE_WORKER_PROBE_LINUX_PORTS") {
        let ports = Ports::parse(ports.split(',').map(std::ffi::OsString::from)).unwrap();
        launch_linux(ports);
        return;
    }
    #[cfg(target_os = "macos")]
    let ports = std::env::var("PTRANSLATE_WORKER_PROBE_PORTS")
        .expect("tools/run_worker_local_probe.pyから起動してください");
    #[cfg(target_os = "macos")]
    let ports = Ports::parse(ports.split(',').map(std::ffi::OsString::from)).unwrap();
    let certificate = reqwest::Certificate::from_pem(
        std::env::var("PTRANSLATE_WORKER_PROBE_CA")
            .expect("ローカル試験の証明書が必要です")
            .as_bytes(),
    )
    .unwrap();
    // 試験バイナリだけにローカル仲介の証明書を追加する。TLS検証は外さない。
    #[cfg(target_os = "macos")]
    let result = run_from_parent_using(ports, |config| {
        Agent::with_root_certificate(config, false, Some(certificate))
    });
    #[cfg(target_os = "linux")]
    let result = (|| {
        worker_sandbox::linux::apply()?;
        run_with_runtime(runtime()?, None, |config| {
            Agent::with_root_certificate(config, true, Some(certificate))
        })
    })();
    status(result.unwrap_or("error"));
}

#[cfg(target_os = "linux")]
fn launch_linux(ports: Ports) {
    assert_eq!(crate::worker_power::external_power(), Some(true));
    let certificate = std::env::var_os("PTRANSLATE_WORKER_PROBE_CA").unwrap();
    runtime().unwrap().block_on(async {
        let broker = Url::parse(&format!("https://127.0.0.1:{}", ports.relay)).unwrap();
        let relay = crate::worker_relay::linux::Relays::start(&broker, ports.engine).unwrap();
        let (broker, engine) = relay.paths();
        let status =
            worker_sandbox::linux::command(&std::env::current_exe().unwrap(), &broker, &engine)
                .args([
                    "--exact",
                    "worker_agent::real_model_probe::restricted_worker",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("PTRANSLATE_WORKER_PROBE_CA", certificate)
                .kill_on_drop(true)
                .status()
                .await
                .unwrap();
        assert!(status.success());
    });
}
