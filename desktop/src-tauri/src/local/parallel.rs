//! ローカル推論のメモリ予算と、変更可能な共通実行枠。
use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::Mutex,
};
use tokio::sync::{Mutex as AsyncMutex, Notify};

pub const MAX_PARALLEL: usize = 64;
pub const CONTEXT_PER_SLOT: usize = 32768;
const GIB: u64 = 1024 * 1024 * 1024;

struct State {
    limit: usize,
    active: usize,
}
pub struct Slots {
    state: Mutex<State>,
    turn: AsyncMutex<()>,
    changed: Notify,
}
pub struct Permit<'a>(&'a Slots);
impl Slots {
    pub fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(State { limit, active: 0 }),
            turn: AsyncMutex::new(()),
            changed: Notify::new(),
        }
    }
    pub fn limit(&self) -> usize {
        self.state.lock().unwrap().limit
    }
    pub fn resize(&self, limit: usize) {
        assert!((1..=MAX_PARALLEL).contains(&limit));
        self.state.lock().unwrap().limit = limit;
        self.changed.notify_one();
    }
    pub async fn acquire(&self) -> Result<Permit<'_>, ()> {
        // 待つ人の順番はTokioのFIFO mutexで保持する。待機futureの破棄で次へ渡る。
        let _turn = self.turn.lock().await;
        loop {
            let changed = self.changed.notified();
            {
                let mut state = self.state.lock().unwrap();
                if state.active < state.limit {
                    state.active += 1;
                    return Ok(Permit(self));
                }
            }
            changed.await;
        }
    }
    #[cfg(test)]
    pub fn try_acquire(&self) -> Result<Permit<'_>, ()> {
        let _turn = self.turn.try_lock().map_err(|_| ())?;
        let mut state = self.state.lock().unwrap();
        if state.active >= state.limit {
            return Err(());
        }
        state.active += 1;
        Ok(Permit(self))
    }
    #[cfg(test)]
    pub fn available_permits(&self) -> usize {
        let state = self.state.lock().unwrap();
        state.limit.saturating_sub(state.active)
    }
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().active -= 1;
        self.0.changed.notify_one();
    }
}

pub struct StartupFailure {
    pub message: String,
    pub reduce_parallel: bool,
}
impl From<String> for StartupFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            reduce_parallel: false,
        }
    }
}
impl From<&str> for StartupFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
pub async fn start_with_fallback<T, F, Fut>(
    slots: &Slots,
    automatic: bool,
    mut attempt: F,
) -> Result<T, String>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<T, StartupFailure>>,
{
    loop {
        let parallel = slots.limit();
        match attempt(parallel).await {
            Ok(result) => return Ok(result),
            Err(error) if automatic && parallel > 1 && error.reduce_parallel => {
                slots.resize((parallel / 2).max(1))
            }
            Err(error) => return Err(error.message),
        }
    }
}

pub fn device_free_bytes(name: &str) -> Option<u64> {
    let prefix = name.split_once("MiB free")?.0.trim_end();
    prefix
        .rsplit(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024 * 1024)
}

pub fn choose_parallel(
    free: Option<u64>,
    model_bytes: u64,
    kv_per_slot: Option<u64>,
    cpu_threads: Option<usize>,
) -> usize {
    let Some((free, kv)) = free.zip(kv_per_slot).filter(|(_, kv)| *kv > 0) else {
        return 1;
    };
    // OS・他のアプリ向けに20%以上を残し、作業領域1GiBとKVの10%を別に見込む。
    let reserve = (free / 5).max(GIB);
    let budget = free
        .saturating_sub(reserve)
        .saturating_sub(model_bytes)
        .saturating_sub(GIB);
    let per_slot = kv.saturating_add(kv / 10);
    let mut count = (budget / per_slot).clamp(1, MAX_PARALLEL as u64) as usize;
    if let Some(threads) = cpu_threads {
        count = count.min(threads.max(1));
    }
    count
}

#[cfg(target_os = "windows")]
pub fn available_memory() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullAvailPhys)
}
#[cfg(target_os = "macos")]
pub fn available_memory() -> Option<u64> {
    let output = std::process::Command::new("/usr/bin/vm_stat")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = std::str::from_utf8(&output.stdout).ok()?;
    let size = text
        .split_once("page size of ")?
        .1
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    let mut pages = 0u64;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if matches!(key, "Pages free" | "Pages inactive" | "Pages speculative") {
                pages =
                    pages.checked_add(value.trim().trim_end_matches('.').parse::<u64>().ok()?)?;
            }
        }
    }
    pages.checked_mul(size)
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn available_memory() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

fn invalid() -> io::Error {
    io::Error::other("モデルのメタデータを取得できませんでした")
}
fn u32_value(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn u64_value(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn string(r: &mut impl Read) -> io::Result<String> {
    let len = u64_value(r)?;
    if len > 4096 {
        return Err(invalid());
    }
    let mut bytes = vec![0; len as usize];
    r.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| invalid())
}
fn skip(r: &mut (impl Read + Seek), kind: u32, end: u64, depth: u8) -> io::Result<()> {
    if depth > 4 || r.stream_position()? > end {
        return Err(invalid());
    }
    let size = match kind {
        0 | 1 | 7 => 1,
        2 | 3 => 2,
        4..=6 => 4,
        10..=12 => 8,
        8 => u64_value(r)?,
        9 => {
            let item = u32_value(r)?;
            let count = u64_value(r)?;
            if count > 1_000_000 || r.stream_position()? > end {
                return Err(invalid());
            }
            for _ in 0..count {
                skip(r, item, end, depth + 1)?;
            }
            return Ok(());
        }
        _ => return Err(invalid()),
    };
    if r.stream_position()?
        .checked_add(size)
        .is_none_or(|position| position > end)
    {
        return Err(invalid());
    }
    r.seek(SeekFrom::Current(size as i64))?;
    Ok(())
}
fn kv_bytes(architecture: &str, values: &HashMap<String, u64>) -> Option<u64> {
    let value = |suffix| values.get(&format!("{architecture}.{suffix}")).copied();
    let layers = value("block_count")?;
    let heads = value("attention.head_count_kv")?;
    let width = || {
        let size = value("embedding_length")?;
        let heads = value("attention.head_count")?;
        if heads == 0 || size % heads != 0 {
            return None;
        }
        Some(size / heads)
    };
    let key = value("attention.key_length").or_else(width)?;
    let val = value("attention.value_length").or_else(width)?;
    if [layers, heads, key, val].contains(&0) {
        return None;
    }
    // 既定のf16のK/Vキャッシュ。SWAの削減分は当てにせず上側に見積もる。
    (CONTEXT_PER_SLOT as u64)
        .checked_mul(layers)?
        .checked_mul(heads)?
        .checked_mul(key.checked_add(val)?)?
        .checked_mul(2)
}

pub fn model_kv_per_slot(path: &Path) -> io::Result<u64> {
    let file = File::open(path)?;
    let end = file.metadata()?.len().min(64 * 1024 * 1024);
    let mut r = BufReader::new(file);
    let mut magic = [0; 4];
    r.read_exact(&mut magic)?;
    if &magic != b"GGUF" || !matches!(u32_value(&mut r)?, 2 | 3) {
        return Err(invalid());
    }
    let _tensors = u64_value(&mut r)?;
    let count = u64_value(&mut r)?;
    if count > 8192 {
        return Err(invalid());
    }
    let mut architecture = String::new();
    let mut values = HashMap::new();
    for _ in 0..count {
        let key = string(&mut r)?;
        let kind = u32_value(&mut r)?;
        if key == "general.architecture" && kind == 8 {
            architecture = string(&mut r)?;
        } else if [
            ".block_count",
            ".embedding_length",
            ".attention.head_count",
            ".attention.head_count_kv",
            ".attention.key_length",
            ".attention.value_length",
        ]
        .iter()
        .any(|suffix| key.ends_with(suffix))
            && matches!(kind, 4 | 10)
        {
            values.insert(
                key,
                if kind == 4 {
                    u32_value(&mut r)? as u64
                } else {
                    u64_value(&mut r)?
                },
            );
        } else {
            skip(&mut r, kind, end, 0)?;
        }
        if r.stream_position()? > end {
            return Err(invalid());
        }
        if values.contains_key(&format!("{architecture}.attention.key_length"))
            && values.contains_key(&format!("{architecture}.attention.value_length"))
        {
            if let Some(bytes) = kv_bytes(&architecture, &values) {
                return Ok(bytes);
            }
        }
    }
    kv_bytes(&architecture, &values).ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};
    #[tokio::test]
    async fn decreasing_limit_waits_for_existing_work_and_canceled_waiter_leaks_nothing() {
        let slots = Arc::new(Slots::new(8));
        let mut held = Vec::new();
        for _ in 0..8 {
            held.push(slots.acquire().await.unwrap());
        }
        slots.resize(2);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), slots.acquire())
                .await
                .is_err()
        );
        for _ in 0..6 {
            held.pop();
        }
        assert!(slots.try_acquire().is_err());
        held.pop();
        let permit = slots.acquire().await.unwrap();
        assert_eq!(slots.available_permits(), 0);
        drop(permit);
        drop(held);
        assert_eq!(slots.available_permits(), 2);
        slots.resize(12);
        let mut held = Vec::new();
        for _ in 0..12 {
            held.push(slots.acquire().await.unwrap());
        }
        assert!(slots.try_acquire().is_err());
    }
    #[tokio::test]
    async fn waiting_requests_are_fifo_and_canceling_the_head_wakes_the_next() {
        let slots = Arc::new(Slots::new(1));
        let held = slots.acquire().await.unwrap();
        let first = {
            let slots = slots.clone();
            tokio::spawn(async move {
                let _permit = slots.acquire().await.unwrap();
                std::future::pending::<()>().await;
            })
        };
        tokio::task::yield_now().await;
        let second = {
            let slots = slots.clone();
            tokio::spawn(async move {
                let _permit = slots.acquire().await.unwrap();
                2
            })
        };
        tokio::task::yield_now().await;
        first.abort();
        let _ = first.await;
        drop(held);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), second)
                .await
                .unwrap()
                .unwrap(),
            2
        );
        assert_eq!(slots.available_permits(), 1);
    }
    #[tokio::test]
    async fn engine_start_failure_reduces_auto_parallel_but_not_manual_or_auth_errors() {
        for (auto, reduce, expected) in [
            (true, true, vec![12, 6, 3, 1]),
            (false, true, vec![12]),
            (true, false, vec![12]),
        ] {
            let slots = Slots::new(12);
            let calls = Mutex::new(Vec::new());
            let result = start_with_fallback(&slots, auto, |count| {
                calls.lock().unwrap().push(count);
                async move {
                    if count == 1 {
                        Ok(())
                    } else {
                        Err(StartupFailure {
                            message: "mock".into(),
                            reduce_parallel: reduce,
                        })
                    }
                }
            })
            .await;
            assert_eq!(*calls.lock().unwrap(), expected);
            assert_eq!(result.is_ok(), auto && reduce);
        }
    }
    #[test]
    fn capacity_uses_memory_and_cpu_budget_and_can_exceed_four() {
        let kv = GIB + GIB / 2;
        let count = choose_parallel(Some(24 * GIB), 2 * GIB, Some(kv), None);
        assert!(count > 4 && count < 16);
        assert_eq!(
            choose_parallel(Some(24 * GIB), 2 * GIB, Some(kv), Some(2)),
            2
        );
        assert_eq!(choose_parallel(None, 2 * GIB, Some(kv), None), 1);
        assert_eq!(choose_parallel(Some(GIB), 2 * GIB, Some(kv), None), 1);
        assert_eq!(
            choose_parallel(Some(u64::MAX), 0, Some(1), None),
            MAX_PARALLEL
        );
        assert_eq!(choose_parallel(Some(24 * GIB), 2 * GIB, None, None), 1);
        assert_eq!(
            device_free_bytes("GPU (24564 MiB, 20000 MiB free)"),
            Some(20000 * 1024 * 1024)
        );
        assert_eq!(device_free_bytes("GPU (memory unavailable)"), None);
    }
    fn model() -> Vec<u8> {
        let mut data = b"GGUF".to_vec();
        data.extend(3u32.to_le_bytes());
        data.extend(0u64.to_le_bytes());
        data.extend(5u64.to_le_bytes());
        let mut string = |s: &str| {
            data.extend((s.len() as u64).to_le_bytes());
            data.extend(s.as_bytes());
        };
        string("general.architecture");
        data.extend(8u32.to_le_bytes());
        data.extend(6u64.to_le_bytes());
        data.extend(b"plamo3");
        for (key, value) in [
            ("block_count", 24u32),
            ("attention.head_count_kv", 4),
            ("attention.key_length", 128),
            ("attention.value_length", 128),
        ] {
            let key = format!("plamo3.{key}");
            data.extend((key.len() as u64).to_le_bytes());
            data.extend(key.as_bytes());
            data.extend(4u32.to_le_bytes());
            data.extend(value.to_le_bytes());
        }
        data
    }
    #[test]
    fn explicit_key_and_value_dimensions_override_embedding_width() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model.gguf");
        let mut data = model();
        data[16..24].copy_from_slice(&7u64.to_le_bytes());
        let mut extra = Vec::new();
        for (key, value) in [
            ("plamo3.embedding_length", 1536u32),
            ("plamo3.attention.head_count", 24),
        ] {
            extra.extend((key.len() as u64).to_le_bytes());
            extra.extend(key.as_bytes());
            extra.extend(4u32.to_le_bytes());
            extra.extend(value.to_le_bytes());
        }
        let at = 24 + 8 + "general.architecture".len() + 4 + 8 + "plamo3".len();
        data.splice(at..at, extra);
        std::fs::write(&path, data).unwrap();
        assert_eq!(model_kv_per_slot(&path).unwrap(), GIB + GIB / 2);
    }
    #[test]
    fn reads_only_model_metadata_and_rejects_truncated_or_unbounded_headers() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model.gguf");
        let data = model();
        std::fs::write(&path, &data).unwrap();
        assert_eq!(model_kv_per_slot(&path).unwrap(), GIB + GIB / 2);
        for len in [0, 4, 12, data.len() - 1] {
            std::fs::write(&path, &data[..len]).unwrap();
            assert!(model_kv_per_slot(&path).is_err());
        }
        let mut bad = data;
        bad[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&path, &bad).unwrap();
        assert!(model_kv_per_slot(&path).is_err());
    }
}
