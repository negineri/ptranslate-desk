//! PTranslate の翻訳ワーカーを Linux サーバーで無人で提供する CLI。
//! 画面を持たず、デスクトップと同じ制限付きのワーカー (bubblewrap と Landlock) を使う。

#[cfg(target_os = "linux")]
mod account;
#[cfg(target_os = "linux")]
mod cli;
#[cfg(target_os = "linux")]
mod engine;
#[cfg(target_os = "linux")]
mod http;
#[cfg(target_os = "linux")]
mod run;
#[cfg(target_os = "linux")]
mod state;

fn main() {
    // 監督が自分自身を制限付きで起動したときの、1件を処理する子。
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--worker-agent")) {
        ptranslate_core::worker_agent::entry();
        return;
    }
    #[cfg(target_os = "linux")]
    std::process::exit(cli::main());
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("ptranslate-worker は Linux だけに対応しています");
        std::process::exit(1);
    }
}
