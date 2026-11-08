//! ローカル翻訳。推論を別プロセスで動かし、原文・訳を保存しない。
mod benchmark;
mod parallel;
pub mod resources;
use crate::Message;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tauri::{ipc::Channel, AppHandle, Manager, WebviewWindow};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    #[serde(default = "default_device")]
    pub device: String,
    pub auto_release: bool,
    #[serde(default = "default_idle_minutes")]
    pub idle_minutes: u64,
    pub parallel: usize,
    pub auto_parallel: bool,
}
fn default_idle_minutes() -> u64 {
    5
}
fn default_device() -> String {
    "auto".into()
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            device: default_device(),
            auto_release: false,
            idle_minutes: default_idle_minutes(),
            parallel: 1,
            auto_parallel: true,
        }
    }
}

#[derive(Deserialize)]
struct Installation {
    engine: PathBuf,
    model: PathBuf,
}

#[derive(Clone)]
struct Endpoint {
    url: String,
    key: String,
}
struct Process {
    child: Child,
    endpoint: Endpoint,
    #[cfg(target_os = "windows")]
    _job: Job,
}
// アプリが強制終了しても推論プロセスとGPUメモリを残さない。
#[cfg(target_os = "windows")]
struct Job(isize);
#[cfg(target_os = "windows")]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0 as _);
        }
    }
}
#[cfg(target_os = "windows")]
fn attach_job(child: &Child) -> Result<Job, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::*;
    unsafe {
        let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if handle.is_null() {
            return Err("推論プロセスの管理を開始できませんでした".into());
        }
        let job = Job(handle as isize);
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as _,
            std::mem::size_of_val(&info) as u32,
        ) == 0
            || AssignProcessToJobObject(handle, child.as_raw_handle() as _) == 0
        {
            return Err("推論プロセスの終了管理を設定できませんでした".into());
        }
        Ok(job)
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct Local {
    settings: Settings,
    process: Mutex<Option<Process>>,
    starting: tokio::sync::Mutex<()>,
    ready: AtomicBool,
    client: reqwest::Client,
    configured: AtomicBool,
    loading: AtomicBool,
    error: Mutex<Option<String>>,
    activity: Mutex<(usize, Instant)>,
    measuring: tokio::sync::Mutex<()>,
    measurement_cancel: Operation,
    downloads: resources::Downloads,
    slots: parallel::Slots,
    parallel_planned: AtomicBool,
    planning: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Operation {
    canceled: Arc<AtomicBool>,
    notify: tokio::sync::Notify,
}
impl Operation {
    fn reset(&self) {
        self.canceled.store(false, Ordering::SeqCst);
    }
    fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }
    async fn canceled(&self) {
        loop {
            if self.canceled.load(Ordering::SeqCst) {
                return;
            }
            self.notify.notified().await;
        }
    }
}
struct Lease<'a>(&'a Local);
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let mut activity = self.0.activity.lock().unwrap();
        activity.0 -= 1;
        activity.1 = Instant::now();
    }
}
struct Loading<'a>(&'a Local);
impl Drop for Loading<'_> {
    fn drop(&mut self) {
        self.0.loading.store(false, Ordering::SeqCst);
        if !self.0.ready.load(Ordering::SeqCst) {
            self.0.stop();
        }
    }
}
fn should_release(active: usize, last: Instant, now: Instant, minutes: u64) -> bool {
    active == 0
        && now.saturating_duration_since(last) >= Duration::from_secs(minutes.saturating_mul(60))
}
struct ResumeAfterMeasurement(AppHandle);
impl Drop for ResumeAfterMeasurement {
    fn drop(&mut self) {
        let handle = self.0.clone();
        if !handle.state::<Local>().enabled() || handle.state::<Local>().settings.auto_release {
            return;
        }
        tauri::async_runtime::spawn(async move {
            let local = handle.state::<Local>();
            if let Err(error) = local.start(&handle).await {
                *local.error.lock().unwrap() = Some(error);
            }
        });
    }
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(feature = "smoke-test")]
    if let Some(dir) = std::env::var_os("PTRANSLATE_SMOKE_CONFIG") {
        return Ok(PathBuf::from(dir).join("local-translation.json"));
    }
    app.path()
        .app_config_dir()
        .map(|p| p.join("local-translation.json"))
        .map_err(|_| "設定の保存先を取得できませんでした".into())
}
fn home(_app: &AppHandle) -> Result<PathBuf, String> {
    #[cfg(feature = "smoke-test")]
    if std::env::var("PTRANSLATE_MOCK").as_deref() == Ok("1") {
        if let Some(dir) = std::env::var_os("PTRANSLATE_SMOKE_CONFIG") {
            return Ok(PathBuf::from(dir).join("inference"));
        }
    }
    // Windowsでは取得スクリプトと同じLOCALAPPDATAを使う。
    // アプリの識別子・インストール先によって保存先を変えない。
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("モデルの保存先を取得できませんでした")?;
    #[cfg(not(target_os = "windows"))]
    let base = _app
        .path()
        .local_data_dir()
        .map_err(|_| "モデルの保存先を取得できませんでした")?;
    Ok(base.join("com.witenp.ptranslate-desk").join("inference"))
}
fn installation(app: &AppHandle) -> Result<Installation, String> {
    let path = home(app)?.join("installation.json");
    let bytes = fs::read(&path).map_err(|_| "ローカル翻訳のモデルをダウンロードしてください")?;
    let value: Installation =
        serde_json::from_slice(&bytes).map_err(|_| "推論エンジンの設定が不正です")?;
    if !value.engine.is_file() || !value.model.is_file() {
        return Err("推論エンジンまたはモデルが見つかりません".into());
    }
    Ok(value)
}
fn command(path: &PathBuf) -> Command {
    let mut command = Command::new(path);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}
impl Local {
    pub fn load(app: &AppHandle) -> Result<Self, Box<dyn std::error::Error>> {
        let path = settings_path(app)?;
        let configured = path.exists();
        let mut settings: Settings = if path.exists() {
            serde_json::from_slice(&fs::read(path)?)?
        } else {
            Settings::default()
        };
        if std::env::var("PTRANSLATE_MOCK").as_deref() == Ok("1") {
            settings.enabled = false;
        }
        Self::new(settings, configured)
    }
    fn new(settings: Settings, configured: bool) -> Result<Self, Box<dyn std::error::Error>> {
        if !(1..=parallel::MAX_PARALLEL).contains(&settings.parallel) {
            return Err("同時に訳す数は1〜64で指定してください".into());
        }
        Ok(Self {
            slots: parallel::Slots::new(if settings.auto_parallel {
                1
            } else {
                settings.parallel
            }),
            parallel_planned: AtomicBool::new(!settings.auto_parallel),
            planning: tokio::sync::Mutex::new(()),
            settings,
            process: Mutex::new(None),
            starting: tokio::sync::Mutex::new(()),
            ready: AtomicBool::new(false),
            client: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .read_timeout(Duration::from_secs(120))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            configured: AtomicBool::new(configured),
            loading: AtomicBool::new(false),
            error: Mutex::new(None),
            activity: Mutex::new((0, Instant::now())),
            measuring: tokio::sync::Mutex::new(()),
            measurement_cancel: Operation::default(),
            downloads: resources::Downloads::default(),
        })
    }
    pub fn enabled(&self) -> bool {
        self.settings.enabled
    }
    fn lease(&self) -> Lease<'_> {
        let mut activity = self.activity.lock().unwrap();
        activity.0 += 1;
        activity.1 = Instant::now();
        Lease(self)
    }
    pub fn stop(&self) {
        self.parallel_planned
            .store(!self.settings.auto_parallel, Ordering::SeqCst);
        self.ready.store(false, Ordering::SeqCst);
        self.process.lock().unwrap().take();
    }
    fn release_if_idle(&self, now: Instant) -> bool {
        if !self.enabled() || !self.settings.auto_release {
            return false;
        }
        let Ok(_starting) = self.starting.try_lock() else {
            return false;
        };
        // 判定から停止まで同じロックを保持し、新しい要求のlease取得との隙間をなくす。
        // leaseは実行枠の待機前に取得するため、拡張を含む待機中の要求も保護する。
        let activity = self.activity.lock().unwrap();
        if !should_release(activity.0, activity.1, now, self.settings.idle_minutes) {
            return false;
        }
        self.stop();
        true
    }
    pub fn schedule(app: &AppHandle) {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let local = app.state::<Local>();
            if local.enabled() {
                local.prepare_parallel(&app).await;
            }
            if local.enabled() && !local.settings.auto_release {
                if let Err(error) = local.start(&app).await {
                    *local.error.lock().unwrap() = Some(error);
                }
            }
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                local.release_if_idle(Instant::now());
            }
        });
    }
    async fn start(&self, app: &AppHandle) -> Result<Endpoint, String> {
        let _starting = self.starting.lock().await;
        {
            let mut slot = self.process.lock().unwrap();
            if self.ready.load(Ordering::SeqCst) {
                if let Some(process) = slot.as_mut() {
                    if matches!(process.child.try_wait(), Ok(None)) {
                        return Ok(process.endpoint.clone());
                    }
                }
            }
            slot.take();
        }
        self.ready.store(false, Ordering::SeqCst);
        self.prepare_parallel(app).await;
        self.loading.store(true, Ordering::SeqCst);
        let _loading = Loading(self);
        *self.error.lock().unwrap() = None;
        let result =
            parallel::start_with_fallback(&self.slots, self.settings.auto_parallel, |count| {
                self.start_engine(app, count)
            })
            .await;
        if result.is_ok() {
            self.parallel_planned.store(true, Ordering::SeqCst);
        }
        result
    }

    async fn prepare_parallel(&self, app: &AppHandle) {
        if !self.settings.auto_parallel
            || self.parallel_planned.load(Ordering::SeqCst)
            || self.loading.load(Ordering::SeqCst)
            || self.ready.load(Ordering::SeqCst)
        {
            return;
        }
        let _planning = self.planning.lock().await;
        if self.parallel_planned.load(Ordering::SeqCst)
            || self.loading.load(Ordering::SeqCst)
            || self.ready.load(Ordering::SeqCst)
        {
            return;
        }
        let model = installation(app)
            .ok()
            .map(|installation| installation.model);
        let memory = tauri::async_runtime::spawn_blocking(move || {
            let host = parallel::available_memory();
            let size = model
                .as_ref()
                .and_then(|path| fs::metadata(path).ok())
                .map_or(0, |meta| meta.len());
            let kv = model
                .as_ref()
                .and_then(|path| parallel::model_kv_per_slot(path).ok());
            (host, size, kv)
        })
        .await
        .unwrap_or((None, 0, None));
        let cpu = self.settings.device == "none";
        let free = if cpu {
            memory.0
        } else {
            let devices = list_devices(app.clone()).await.unwrap_or_default();
            let selected: Vec<_> = devices
                .iter()
                .filter(|device| {
                    self.settings.device == "auto" || device.id == self.settings.device
                })
                .collect();
            let gpu = if selected.is_empty() {
                None
            } else {
                selected
                    .iter()
                    .map(|device| parallel::device_free_bytes(&device.name))
                    .collect::<Option<Vec<_>>>()
                    .and_then(|values| values.into_iter().min())
            };
            gpu.map(|gpu| memory.0.map_or(gpu, |host| host.min(gpu)))
        };
        let threads = cpu.then(|| std::thread::available_parallelism().map_or(1, usize::from));
        self.slots
            .resize(parallel::choose_parallel(free, memory.1, memory.2, threads));
        self.parallel_planned.store(true, Ordering::SeqCst);
    }

    async fn start_engine(
        &self,
        app: &AppHandle,
        count: usize,
    ) -> Result<Endpoint, parallel::StartupFailure> {
        let install = installation(app)?;
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|_| "推論エンジンのポートを確保できませんでした")?;
        let port = listener
            .local_addr()
            .map_err(|_| "ポートを取得できませんでした")?
            .port();
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|_| "ローカル認証キーを生成できませんでした")?;
        let endpoint = Endpoint {
            url: format!("http://127.0.0.1:{port}"),
            key: URL_SAFE_NO_PAD.encode(secret),
        };
        drop(listener);
        let mut cmd = command(&install.engine);
        cmd.args(["--model"])
            .arg(install.model)
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--ctx-size",
                &(parallel::CONTEXT_PER_SLOT * count).to_string(),
                "--parallel",
                &count.to_string(),
                "--flash-attn",
                "on",
                "--no-webui",
                "--log-disable",
            ])
            .env("LLAMA_API_KEY", &endpoint.key)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if self.settings.device == "none" {
            cmd.args(["--device", "none", "--n-gpu-layers", "0"]);
        } else {
            cmd.args(["--n-gpu-layers", "99"]);
            if self.settings.device != "auto" {
                cmd.args(["--device", &self.settings.device]);
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|_| "推論エンジンを起動できませんでした")?;
        #[cfg(target_os = "windows")]
        let job = match attach_job(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        *self.process.lock().unwrap() = Some(Process {
            child,
            endpoint: endpoint.clone(),
            #[cfg(target_os = "windows")]
            _job: job,
        });
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(120) {
            if self
                .process
                .lock()
                .unwrap()
                .as_mut()
                .is_none_or(|p| !matches!(p.child.try_wait(), Ok(None)))
            {
                self.stop();
                return Err(parallel::StartupFailure {
                    message: "推論エンジンが終了しました。CPUまたは別のGPUを選択してください"
                        .into(),
                    reduce_parallel: true,
                });
            }
            if self
                .client
                .get(format!("{}/health", endpoint.url))
                .bearer_auth(&endpoint.key)
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                let protected = self
                    .client
                    .get(format!("{}/v1/models", endpoint.url))
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await
                    .is_ok_and(|r| r.status() == 401);
                if !protected {
                    self.stop();
                    return Err("推論エンジンのローカル認証を確認できませんでした".into());
                }
                if self
                    .process
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_none_or(|p| p.endpoint.key != endpoint.key)
                {
                    return Err("モデルの読み込みを中止しました".into());
                }
                self.ready.store(true, Ordering::SeqCst);
                return Ok(endpoint);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.stop();
        Err("モデルの読み込みがタイムアウトしました".into())
    }
}

#[derive(Serialize)]
pub struct Status {
    chrome_error: Option<String>,
    enabled: bool,
    ready: bool,
    available: bool,
    device: String,
    resource_error: Option<String>,
    configured: bool,
    supported: bool,
    loading: bool,
    auto_release: bool,
    idle_minutes: u64,
    parallel: usize,
    configured_parallel: usize,
    auto_parallel: bool,
    parallel_adjusting: bool,
    error: Option<String>,
}
#[tauri::command]
pub fn local_status(app: AppHandle) -> Status {
    let local = app.state::<Local>();
    let resources = installation(&app);
    let error = local.error.lock().unwrap().clone();
    Status {
        chrome_error: crate::chrome_bridge::error(&app),
        enabled: local.enabled(),
        ready: local.ready.load(Ordering::SeqCst),
        available: resources.is_ok(),
        device: local.settings.device.clone(),
        resource_error: resources.err(),
        configured: local.configured.load(Ordering::SeqCst)
            || crate::connection::load(&app).is_ok_and(|c| !c.server_url.is_empty()),
        supported: resources::supported(),
        loading: local.loading.load(Ordering::SeqCst),
        auto_release: local.settings.auto_release,
        idle_minutes: local.settings.idle_minutes,
        parallel: local.slots.limit(),
        configured_parallel: local.settings.parallel,
        auto_parallel: local.settings.auto_parallel,
        parallel_adjusting: local.settings.auto_parallel
            && !local.parallel_planned.load(Ordering::SeqCst),
        error,
    }
}
#[derive(Clone, Serialize)]
pub struct Device {
    id: String,
    name: String,
}
fn require_main(window: &WebviewWindow) -> Result<(), String> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err("メインウィンドウで操作してください".into())
    }
}
fn persist_settings(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app)?;
    let dir = path.parent().ok_or("設定の保存先が不正です")?;
    fs::create_dir_all(dir).map_err(|_| "設定を保存できませんでした")?;
    let mut file =
        tempfile::NamedTempFile::new_in(dir).map_err(|_| "設定を保存できませんでした")?;
    serde_json::to_writer(&mut file, settings).map_err(|_| "設定を保存できませんでした")?;
    file.persist(&path)
        .map_err(|_| "設定を保存できませんでした")?;
    Ok(())
}
#[tauri::command]
pub fn select_server_mode(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    require_main(&window)?;
    let local = app.state::<Local>();
    if local.enabled() {
        return Err("モードを保存して再起動してください".into());
    }
    persist_settings(&app, &local.settings)?;
    local.configured.store(true, Ordering::SeqCst);
    Ok(())
}
#[derive(Clone, Serialize, Default)]
pub struct Measurement {
    id: String,
    name: String,
    milliseconds: Option<u64>,
    error: Option<String>,
    measuring_parallel: Option<usize>,
    recommended_parallel: Option<usize>,
    bulk_per_second: Option<f64>,
    bulk_error: Option<String>,
    parallel_samples: Vec<benchmark::Sample>,
}
#[tauri::command]
pub fn cancel_local_measurement(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    require_main(&window)?;
    app.state::<Local>().measurement_cancel.cancel();
    Ok(())
}
#[tauri::command]
pub async fn measure_local_devices(
    app: AppHandle,
    window: WebviewWindow,
    progress: Channel<Measurement>,
) -> Result<Vec<Measurement>, String> {
    require_main(&window)?;
    installation(&app)?;
    if !resources::supported() {
        return Err(
            "デバイスの計測はWindows x64とmacOS 13.3以降（Apple Silicon・Intel）に対応しています"
                .into(),
        );
    }
    let local = app.state::<Local>();
    let _measuring = local
        .measuring
        .try_lock()
        .map_err(|_| "デバイスはすでに計測中です")?;
    local.measurement_cancel.reset();
    let _starting = tokio::select! {
        _ = local.measurement_cancel.canceled() => return Err("計測を中止しました".into()),
        guard = local.starting.lock() => guard,
    };
    if local.activity.lock().unwrap().0 > 0
        || app
            .state::<crate::Backend>()
            .session
            .lock()
            .unwrap()
            .requests
            .values()
            .any(|r| r.translation && !r.task.inner().is_finished())
    {
        return Err("翻訳が終わってから計測してください".into());
    }
    local.stop();
    let _resume = ResumeAfterMeasurement(app.clone());
    let mut devices = tokio::select! {
        _ = local.measurement_cancel.canceled() => return Err("計測を中止しました。デバイスは手動でも選べます".into()),
        devices = list_devices(app.clone()) => devices.unwrap_or_default(),
    };
    devices.truncate(8);
    devices.push(Device {
        id: "none".into(),
        name: "CPU".into(),
    });
    let mut results = Vec::new();
    for device in devices {
        progress
            .send(Measurement {
                id: device.id.clone(),
                name: device.name.clone(),
                milliseconds: None,
                error: None,
                ..Measurement::default()
            })
            .map_err(|_| "計測画面が閉じられました")?;
        let trial = Local::new(
            Settings {
                enabled: true,
                device: device.id.clone(),
                auto_parallel: false,
                ..Settings::default()
            },
            true,
        )
        .map_err(|_| "計測を準備できませんでした")?;
        let result = tokio::select! {
            _ = local.measurement_cancel.canceled() => break,
            result = tokio::time::timeout(Duration::from_secs(45), async {
                let endpoint = trial.start(&app).await?;
                let (prompt, _) = prompt(&json!({"text":"This application translates text on your computer.","target_lang":"ja"}))?;
                let started = Instant::now();
                let response = trial.client.post(format!("{}/v1/completions",endpoint.url)).bearer_auth(endpoint.key)
                    .json(&json!({"prompt":prompt,"max_tokens":24,"temperature":0,"stream":false,"cache_prompt":false,"add_special_tokens":true,"stop":["<|plamo:tag|>","<|plamo:bos|>"]}))
                    .send().await.map_err(|_| "計測の応答を取得できませんでした".to_string())?;
                if !response.status().is_success() { return Err("計測の応答が不正です".into()); }
                let value: Value = response.json().await.map_err(|_| "計測の応答が不正です".to_string())?;
                if value["choices"][0]["text"].as_str().is_none_or(|t|t.trim().is_empty()) { return Err("計測の訳を取得できませんでした".into()); }
                Ok::<_,String>(started.elapsed().as_millis().max(1) as u64)
            }) => result.unwrap_or_else(|_| Err("45秒以内に計測できませんでした".into())),
        };
        trial.stop();
        let measurement = match result {
            Ok(milliseconds) => Measurement {
                id: device.id,
                name: device.name,
                milliseconds: Some(milliseconds),
                error: None,
                ..Measurement::default()
            },
            Err(error) => Measurement {
                id: device.id,
                name: device.name,
                milliseconds: None,
                error: Some(error),
                ..Measurement::default()
            },
        };
        progress
            .send(measurement.clone())
            .map_err(|_| "計測画面が閉じられました")?;
        results.push(measurement);
    }
    results.sort_by_key(|r| r.milliseconds.unwrap_or(u64::MAX));
    // CPUは並列数を計測しない。短文で最速だったGPUだけ追加計測する。
    if !local.measurement_cancel.canceled.load(Ordering::SeqCst) {
        if let Some(index) = benchmark::target(&results) {
            let gpu = &mut results[index];
            benchmark::measure(&app, &progress, gpu).await?;
            progress
                .send(gpu.clone())
                .map_err(|_| "計測画面が閉じられました")?;
        }
    }
    drop(_starting);
    if local.measurement_cancel.canceled.load(Ordering::SeqCst) {
        return Err("計測を中止しました。デバイスは手動でも選べます".into());
    }
    Ok(results)
}
fn gpu_device(id: &str) -> bool {
    id.strip_prefix(if cfg!(target_os = "macos") {
        "MTL"
    } else {
        "Vulkan"
    })
    .is_some_and(|n| !n.is_empty() && n.len() < 4 && n.chars().all(|c| c.is_ascii_digit()))
}
fn parse_devices(text: &str) -> Vec<Device> {
    text.lines()
        .filter_map(|line| {
            let (id, name) = line.trim().split_once(':')?;
            if !gpu_device(id) {
                return None;
            }
            Some(Device {
                id: id.into(),
                name: name.trim().into(),
            })
        })
        .collect()
}
async fn list_devices(app: AppHandle) -> Result<Vec<Device>, String> {
    let install = installation(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut child = command(&install.engine)
            .arg("--list-devices")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| "GPUの一覧を取得できませんでした")?;
        #[cfg(target_os = "windows")]
        let _job = match attach_job(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let started = Instant::now();
        while matches!(child.try_wait(), Ok(None)) {
            if started.elapsed() >= Duration::from_secs(10) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("GPU一覧の取得がタイムアウトしました。CPUを選択できます".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let output = child
            .wait_with_output()
            .map_err(|_| "GPUの一覧を取得できませんでした")?;
        if !output.status.success() {
            return Err("GPUの初期化に失敗しました。CPUを選択してください".into());
        }
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(parse_devices(&text))
    })
    .await
    .map_err(|_| "GPUの一覧を取得できませんでした".to_string())?
}
#[tauri::command]
pub async fn local_devices(app: AppHandle, window: WebviewWindow) -> Result<Vec<Device>, String> {
    require_main(&window)?;
    list_devices(app).await
}
#[tauri::command]
pub async fn save_local_settings(
    app: AppHandle,
    window: WebviewWindow,
    settings: Settings,
) -> Result<(), String> {
    require_main(&window)?;
    if settings.device != "auto" && settings.device != "none" && !gpu_device(&settings.device) {
        return Err("GPUの指定が不正です".into());
    }
    if settings.enabled {
        if !resources::supported() {
            return Err(
                "ローカル翻訳はWindows x64とmacOS 13.3以降（Apple Silicon・Intel）に対応しています"
                    .into(),
            );
        }
        installation(&app)?;
    }
    if !(1..=60).contains(&settings.idle_minutes) {
        return Err("メモリを解放するまでの時間は1〜60分で指定してください".into());
    }
    if !(1..=parallel::MAX_PARALLEL).contains(&settings.parallel) {
        return Err("同時に訳す数は1〜64で指定してください".into());
    }
    persist_settings(&app, &settings)?;
    crate::cancel_all(&app);
    app.state::<Local>().stop();
    app.request_restart();
    Ok(())
}
#[tauri::command]
pub async fn start_local_engine(app: AppHandle, window: WebviewWindow) -> Result<(), String> {
    require_main(&window)?;
    if !app.state::<Local>().enabled() {
        return Err("ローカルモードを有効にしてください".into());
    }
    let local = app.state::<Local>();
    let _lease = local.lease();
    let result = local.start(&app).await.map(|_| ());
    if let Err(error) = &result {
        *local.error.lock().unwrap() = Some(error.clone());
    }
    result
}

fn field(body: &Value, name: &str, limit: usize, required: bool) -> Result<String, String> {
    let value = match body.get(name) {
        None | Some(Value::Null) => "",
        Some(v) => v.as_str().ok_or("文字列の指定が不正です")?,
    }
    .replace("\r\n", "\n");
    if value.chars().count() > limit || value.contains("<|plamo:") {
        return Err("文字数の上限を超えているか、使えない文字列が含まれています".into());
    }
    if required && value.trim().is_empty() {
        return Err("原文が空です".into());
    }
    Ok(value)
}
fn languages() -> Value {
    serde_json::from_str(include_str!("../gen/local-languages.json"))
        .expect("生成した言語一覧が不正です")
}
pub fn prompt(body: &Value) -> Result<(String, usize), String> {
    let text = field(body, "text", 4000, true)?;
    let target = field(body, "target_lang", 32, true)?;
    let source = field(body, "source_lang", 32, false)?;
    let valid_lang = |v: &str| {
        languages()
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["code"] == v)
    };
    if !valid_lang(&target) || (!source.is_empty() && !valid_lang(&source)) {
        return Err("言語の指定が不正です".into());
    }
    let form = field(body, "form", 32, false)?;
    if !form.is_empty() && (target != "ja" || !["plain", "polite"].contains(&form.as_str())) {
        return Err("文体の指定が不正です".into());
    }
    let context = field(body, "context", 12000, false)?;
    let lang = |v: &str| {
        if v.is_empty() {
            String::new()
        } else {
            format!("<|plamo:key|>lang<|plamo:val|>{v}")
        }
    };
    let mut value = "<|plamo:tag|>dataset<|plamo:msg|>translation".to_string();
    if !context.trim().is_empty() {
        value += &format!("<|plamo:tag|>context<|plamo:msg|>{}", context.trim());
    }
    for (name, max, source_limit, target_limit) in
        [("history", 4, 4000, 12000), ("glossary", 200, 200, 200)]
    {
        if let Some(raw) = body.get(name).filter(|v| !v.is_null()) {
            let pairs = raw.as_array().ok_or("文脈または用語集の形式が不正です")?;
            if pairs.len() > max {
                return Err("文脈または用語集の件数が上限を超えています".into());
            }
            let mut glossary = String::new();
            for pair in pairs {
                if !pair.is_object() {
                    return Err("文脈または用語集の形式が不正です".into());
                }
                let src = field(pair, "source", source_limit, false)?;
                let dst = field(pair, "target", target_limit, false)?;
                if src.trim().is_empty() || dst.trim().is_empty() {
                    continue;
                }
                if name == "history" {
                    value += &format!("<|plamo:tag|>input{}<|plamo:msg|>{src}<|plamo:tag|>output{}<|plamo:msg|>{dst}", lang(&source), lang(&target));
                } else {
                    glossary +=
                        &format!("<|plamo:index|>{}<|plamo:item|>{}", src.trim(), dst.trim());
                }
            }
            if !glossary.is_empty() {
                value += &format!("<|plamo:tag|>glossary{glossary}");
            }
        }
    }
    value += &format!(
        "<|plamo:tag|>input{}<|plamo:msg|>{text}<|plamo:tag|>output{}",
        lang(&source),
        lang(&target)
    );
    if !form.is_empty() {
        value += &format!("<|plamo:key|>form<|plamo:val|>{form}");
    }
    value += "<|plamo:msg|>";
    if value.chars().count() > 12000 {
        return Err("文脈・前の訳・用語集を合わせて長すぎます".into());
    }
    Ok((value, (text.chars().count() * 3 / 2 + 64).clamp(256, 8000)))
}

pub type SendResult<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ()>> + Send + 'a>>;
pub trait Output: Send + Sync {
    fn send(&self, message: Message) -> SendResult<'_>;
}
impl Output for Channel<Message> {
    fn send(&self, message: Message) -> SendResult<'_> {
        Box::pin(async move { Channel::send(self, message).map_err(|_| ()) })
    }
}
async fn bytes(stream: &dyn Output, data: Value) -> Result<(), ()> {
    stream
        .send(Message::Bytes {
            bytes: format!("data: {data}\n\n").into_bytes(),
        })
        .await
        .map_err(|_| ())
}
async fn json_response(stream: &dyn Output, status: u16, data: Value) -> Result<(), ()> {
    stream
        .send(Message::Start { status })
        .await
        .map_err(|_| ())?;
    stream
        .send(Message::Bytes {
            bytes: serde_json::to_vec(&data).map_err(|_| ())?,
        })
        .await
        .map_err(|_| ())
}
fn configuration(local: &Local) -> Value {
    json!({"user":"この端末", "languages":languages(), "max_chars":4000, "max_glossary":200, "concurrency":{"max_in_flight":local.slots.limit(),"scope":"application"}})
}
pub async fn request(
    app: &AppHandle,
    route: &str,
    body: Option<&Value>,
    stream: &dyn Output,
) -> Result<(), ()> {
    let local = app.state::<Local>();
    if route == "api/config" {
        local.prepare_parallel(app).await;
        return json_response(stream, 200, configuration(&local)).await;
    }
    if route != "api/translate" {
        return json_response(stream, 404, json!({"error":"不明なAPIです"})).await;
    }
    let (prompt, limit) = match body.ok_or("原文がありません".to_string()).and_then(prompt)
    {
        Ok(value) => value,
        Err(error) => return json_response(stream, 400, json!({"error":error})).await,
    };
    let _lease = local.lease();
    let endpoint = match local.start(app).await {
        Ok(e) => e,
        Err(error) => return json_response(stream, 503, json!({"error":error})).await,
    };
    let _slot = local.slots.acquire().await.map_err(|_| ())?;
    let response = local.client.post(format!("{}/v1/completions", endpoint.url)).bearer_auth(&endpoint.key)
        .json(&json!({"prompt":prompt,"max_tokens":limit,"temperature":0,"stop":["<|plamo:tag|>","<|plamo:bos|>"],"add_special_tokens":true,"stream":true})).send().await;
    let mut response = match response {
        Ok(r) if r.status().is_success() => r,
        _ => {
            return json_response(
                stream,
                502,
                json!({"error":"ローカル推論エンジンから応答を取得できませんでした"}),
            )
            .await
        }
    };
    stream
        .send(Message::Start { status: 200 })
        .await
        .map_err(|_| ())?;
    let mut decoder = Decoder::default();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        for event in decoder.feed(&chunk).map_err(|_| ())? {
            bytes(stream, event).await?;
        }
        if decoder.done {
            return Ok(());
        }
    }
    bytes(
        stream,
        json!({"error":"ローカル翻訳の接続が途中で切れました"}),
    )
    .await
}

#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
    done: bool,
}
impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Value>, String> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > 1024 * 1024 {
            return Err("推論エンジンの応答が長すぎます".into());
        }
        let mut events = vec![];
        while let Some(end) = self.buffer.iter().position(|c| *c == b'\n') {
            let line: Vec<_> = self.buffer.drain(..=end).collect();
            let line = std::str::from_utf8(&line)
                .map_err(|_| "応答の文字コードが不正です")?
                .trim();
            let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if data == "[DONE]" {
                continue;
            }
            let value: Value = serde_json::from_str(data).map_err(|_| "応答の形式が不正です")?;
            if value.get("error").is_some() {
                return Err("ローカル推論エンジンがエラーを返しました".into());
            }
            let choice = &value["choices"][0];
            if let Some(text) = choice["text"].as_str().filter(|s| !s.is_empty()) {
                events.push(json!({"t":text}));
            }
            if let Some(finish) = choice["finish_reason"].as_str() {
                events.push(json!({"done":true,"finish":finish}));
                self.done = true;
            }
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gpu_device_matches_platform() {
        let prefix = if cfg!(target_os = "macos") {
            "MTL"
        } else {
            "Vulkan"
        };
        assert!(super::gpu_device(&format!("{prefix}0")));
        for suffix in ["", "-1", "0 extra", "1000"] {
            assert!(!super::gpu_device(&format!("{prefix}{suffix}")));
        }
        assert!(!super::gpu_device(if cfg!(target_os = "macos") {
            "Vulkan0"
        } else {
            "Metal0"
        }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn m2_device_list_uses_mtl_identifier() {
        let devices = super::parse_devices("Available devices:\n  MTL0: Apple M2 (10922 MiB, 10922 MiB free)\n  BLAS: Accelerate (0 MiB, 0 MiB free)\n");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "MTL0");
        assert!(devices[0].name.starts_with("Apple M2"));
        assert!(super::gpu_device(&devices[0].id));
        assert!(!super::gpu_device("Metal0"));
    }

    use super::*;

    #[test]
    fn config_announces_the_runtime_limit_instead_of_the_saved_manual_value() {
        let local = Local::new(
            Settings {
                auto_parallel: true,
                parallel: 1,
                ..Settings::default()
            },
            true,
        )
        .unwrap();
        local.slots.resize(12);
        assert_eq!(
            configuration(&local)["concurrency"],
            json!({"max_in_flight":12,"scope":"application"})
        );
        local.slots.resize(3);
        assert_eq!(configuration(&local)["concurrency"]["max_in_flight"], 3);
    }

    #[tokio::test]
    async fn shared_slots_limit_concurrent_work_and_release_canceled_waiters() {
        for parallel in [1, 2, 3, 4, 8, 12, 64] {
            let local = Local::new(
                Settings {
                    parallel,
                    auto_parallel: false,
                    ..Settings::default()
                },
                true,
            )
            .unwrap();
            let mut permits = Vec::new();
            for _ in 0..parallel {
                permits.push(local.slots.acquire().await.unwrap());
            }
            assert!(local.slots.try_acquire().is_err());
            assert!(
                tokio::time::timeout(Duration::from_millis(5), local.slots.acquire())
                    .await
                    .is_err()
            );
            permits.pop();
            // timeoutで破棄した待機要求が枠を占有しない。
            let permit = local.slots.try_acquire().unwrap();
            drop(permit);
            drop(permits);
            assert_eq!(local.slots.available_permits(), parallel);
        }
        for parallel in [0, 65, usize::MAX] {
            assert!(Local::new(
                Settings {
                    parallel,
                    auto_parallel: false,
                    ..Settings::default()
                },
                true
            )
            .is_err());
        }
    }

    #[tokio::test]
    async fn idle_release_preserves_running_and_queued_work_and_resets_after_cancel() {
        let local = Arc::new(
            Local::new(
                Settings {
                    enabled: true,
                    auto_release: true,
                    idle_minutes: 1,
                    ..Settings::default()
                },
                true,
            )
            .unwrap(),
        );
        local.ready.store(true, Ordering::SeqCst);
        // デスクトップが実行枠を使い、拡張が同じ枠を待っている状態を作る。
        let desktop = local.lease();
        let slot = local.slots.acquire().await.unwrap();
        let (waiting, received) = tokio::sync::oneshot::channel();
        let bridge_local = local.clone();
        let bridge = tokio::spawn(async move {
            let _lease = bridge_local.lease();
            waiting.send(()).unwrap();
            let _slot = bridge_local.slots.acquire().await.unwrap();
            std::future::pending::<()>().await;
        });
        received.await.unwrap();
        let now = Instant::now();
        local.activity.lock().unwrap().1 = now - Duration::from_secs(120);
        assert_eq!(local.activity.lock().unwrap().0, 2);
        assert!(!local.release_if_idle(now));
        assert!(local.ready.load(Ordering::SeqCst));
        drop(slot);
        drop(desktop);
        assert_eq!(local.activity.lock().unwrap().0, 1);
        assert!(!local.release_if_idle(now + Duration::from_secs(120)));
        // Port切断によるタスクの中止でもleaseと実行枠を返し、ここから未使用時間を数える。
        bridge.abort();
        assert!(bridge.await.unwrap_err().is_cancelled());
        let last = {
            let activity = local.activity.lock().unwrap();
            assert_eq!(activity.0, 0);
            assert!(activity.1 >= now);
            activity.1
        };
        assert_eq!(local.slots.available_permits(), 1);
        assert!(!local.release_if_idle(last + Duration::from_secs(59)));
        assert!(local.ready.load(Ordering::SeqCst));
        assert!(local.release_if_idle(last + Duration::from_secs(60)));
        assert!(!local.ready.load(Ordering::SeqCst));
    }

    #[test]
    fn idle_release_waits_for_model_start_and_measurement() {
        let local = Local::new(
            Settings {
                enabled: true,
                auto_release: true,
                idle_minutes: 1,
                ..Settings::default()
            },
            true,
        )
        .unwrap();
        local.ready.store(true, Ordering::SeqCst);
        let now = Instant::now();
        local.activity.lock().unwrap().1 = now - Duration::from_secs(120);
        let starting = local.starting.try_lock().unwrap();
        assert!(!local.release_if_idle(now));
        assert!(local.ready.load(Ordering::SeqCst));
        drop(starting);
        assert!(local.release_if_idle(now));
        assert!(!local.ready.load(Ordering::SeqCst));
    }

    #[test]
    fn idle_release_respects_mode_and_auto_release_setting() {
        for (enabled, auto_release) in [(false, true), (true, false)] {
            let local = Local::new(
                Settings {
                    enabled,
                    auto_release,
                    ..Settings::default()
                },
                true,
            )
            .unwrap();
            local.ready.store(true, Ordering::SeqCst);
            let now = Instant::now();
            local.activity.lock().unwrap().1 = now - Duration::from_secs(3600);
            assert!(!local.release_if_idle(now));
            assert!(local.ready.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn legacy_settings_preload_by_default_and_idle_does_not_interrupt_work() {
        let settings: Settings =
            serde_json::from_str(r#"{"enabled":true,"device":"Vulkan0"}"#).unwrap();
        assert!(!settings.auto_release);
        assert_eq!(settings.idle_minutes, 5);
        assert_eq!(settings.parallel, 1);
        assert!(settings.auto_parallel);
        let local = Local::new(settings, true).unwrap();
        let now = Instant::now();
        let old = now - Duration::from_secs(301);
        assert!(should_release(0, old, now, 5));
        assert!(!should_release(0, now - Duration::from_secs(299), now, 5));
        assert!(!should_release(1, old, now, 5));
        let lease = local.lease();
        local.activity.lock().unwrap().1 = old;
        assert_eq!(local.activity.lock().unwrap().0, 1);
        drop(lease);
        let activity = local.activity.lock().unwrap();
        assert_eq!(activity.0, 0);
        assert!(!should_release(activity.0, activity.1, Instant::now(), 5));
    }

    #[test]
    fn server_prompt_fixtures() {
        let cases: Vec<Value> = serde_json::from_str(include_str!(
            "../../../shared/local-translation-fixtures.json"
        ))
        .unwrap();
        for case in cases {
            let (actual, limit) = prompt(&case["body"]).unwrap();
            assert_eq!(actual, case["prompt"].as_str().unwrap(), "{}", case["name"]);
            assert_eq!(limit, case["max_tokens"].as_u64().unwrap() as usize);
        }
    }

    #[test]
    fn reject_invalid_requests() {
        for body in [
            json!({"text":" ","target_lang":"ja"}),
            json!({"text":"<|plamo:tag|>","target_lang":"ja"}),
            json!({"text":"x".repeat(4001),"target_lang":"ja"}),
            json!({"text":"test","target_lang":"invalid"}),
            json!({"text":"test","target_lang":"en","form":"polite"}),
            json!({"text":"test","target_lang":"ja","history":vec![json!({"source":"a","target":"b"}); 5]}),
            json!({"text":"test","target_lang":"ja","glossary":[{"source":"<|plamo:","target":"b"}]}),
        ] {
            assert!(prompt(&body).is_err());
        }
    }

    #[test]
    fn sse_handles_fragmented_unicode_and_finish() {
        let raw = ": keepalive\r\ndata: {\"choices\":[{\"text\":\"こんにちは🌸\",\"finish_reason\":null}]}\r\n\r\ndata: {\"choices\":[{\"text\":\"\",\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let mut decoder = Decoder::default();
        let mut events = vec![];
        for b in raw.bytes() {
            events.extend(decoder.feed(&[b]).unwrap());
        }
        assert_eq!(
            events,
            vec![
                json!({"t":"こんにちは🌸"}),
                json!({"done":true,"finish":"stop"})
            ]
        );
        assert!(decoder.done);
    }

    #[test]
    fn reject_malformed_engine_response() {
        assert!(Decoder::default().feed(b"data: invalid\n").is_err());
        assert!(Decoder::default()
            .feed(&vec![b'x'; 1024 * 1024 + 1])
            .is_err());
    }
}
