//! tools/run_worker_local_probe.pyからだけ明示的に起動する実モデル確認用の子。
use super::*;

#[test]
#[ignore = "ローカル仲介と取得済み実モデルが必要。通常のCIでは実行しない"]
fn restricted_worker() {
    let ports = std::env::var("PTRANSLATE_WORKER_PROBE_PORTS")
        .expect("tools/run_worker_local_probe.pyから起動してください");
    let ports = Ports::parse(ports.split(',').map(std::ffi::OsString::from)).unwrap();
    let certificate = reqwest::Certificate::from_pem(
        std::env::var("PTRANSLATE_WORKER_PROBE_CA")
            .expect("ローカル試験の証明書が必要です")
            .as_bytes(),
    )
    .unwrap();
    let result = run_from_parent_using(ports, |config| {
        let mut agent = Agent::new(config)?;
        // 試験バイナリだけにローカル仲介の証明書を追加する。TLS検証は外さない。
        let proxy = reqwest::Proxy::https(format!("http://127.0.0.1:{}", ports.relay))
            .map_err(|_| ())?
            .basic_auth(
                crate::worker_relay::PROXY_USER,
                agent.config.relay_secret.as_deref().ok_or(())?,
            );
        agent.client = Client::builder()
            .no_proxy()
            .proxy(proxy)
            .add_root_certificate(certificate)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(POST_TIMEOUT)
            .read_timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| ())?;
        Ok(agent)
    });
    status(result.unwrap_or("error"));
}
