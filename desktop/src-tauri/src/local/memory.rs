//! 計測用モデルを起動する前に、推論エンジンの見積もりと空き容量を比較する。
use super::*;

const MIB: u64 = 1024 * 1024;

fn estimate(text: &str, device: &str) -> Result<(u64, u64), String> {
    let mut host = None;
    let mut gpu = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let target = match fields.next() {
            Some("Host") => &mut host,
            Some(name) if name == device => &mut gpu,
            _ => continue,
        };
        let sizes: Vec<_> = fields
            .map(str::parse::<u64>)
            .collect::<Result<_, _>>()
            .map_err(|_| "推論エンジンのメモリ見積もりを読み取れませんでした")?;
        if sizes.len() != 3 || target.is_some() {
            return Err("推論エンジンのメモリ見積もりが不正です".into());
        }
        *target = Some(sizes.into_iter().fold(0u64, u64::saturating_add));
    }
    let host = host.ok_or("システムメモリの必要量を確認できませんでした")?;
    let gpu = if device == "none" {
        0
    } else {
        gpu.ok_or("GPUメモリの必要量を確認できませんでした")?
    };
    Ok((host, gpu))
}

fn ensure_capacity(kind: &str, estimate_mib: u64, available_mib: u64) -> Result<(), String> {
    if estimate_mib == 0 {
        return Ok(());
    }
    // モデル・KVキャッシュ・作業領域以外の割り当てと、計測直前の変動に備える。
    let needed = estimate_mib.saturating_add((estimate_mib / 10).max(512));
    if available_mib < needed {
        return Err(format!(
            "{kind}が不足するため計測を省略しました（必要見込み {needed} MiB、空き {available_mib} MiB。必要量には余裕分を含みます）"
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn available_host_mib() -> Result<u64, String> {
    let output = probe_output(&mut Command::new("/usr/bin/vm_stat"))?;
    macos_available_mib(&output)
}

#[cfg(any(target_os = "macos", test))]
fn macos_available_mib(output: &str) -> Result<u64, String> {
    let parse = || {
        let page_size: u64 = output
            .lines()
            .next()?
            .split_once("page size of ")?
            .1
            .split_whitespace()
            .next()?
            .parse()
            .ok()?;
        // すぐに使える空きページだけを数え、スワップや非アクティブ領域を見込まない。
        let free: u64 = output
            .lines()
            .find_map(|line| line.strip_prefix("Pages free:"))?
            .trim()
            .trim_end_matches('.')
            .parse()
            .ok()?;
        Some(free.checked_mul(page_size)? / MIB)
    };
    parse().ok_or_else(|| "空きシステムメモリを確認できませんでした".into())
}

#[cfg(target_os = "windows")]
fn available_host_mib() -> Result<u64, String> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // MEMORYSTATUSEXの各フィールドは整数。
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // dwLengthを設定した書き込み可能な構造体を渡す。
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err("空きシステムメモリを確認できませんでした".into());
    }
    Ok(status.ullAvailPhys / MIB)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn available_host_mib() -> Result<u64, String> {
    Err("空きシステムメモリを確認できませんでした".into())
}

fn gpu_free_mib(name: &str) -> Option<u64> {
    name.strip_suffix(" MiB free)")?
        .rsplit_once(", ")?
        .1
        .parse()
        .ok()
}

#[derive(Clone, Default, Serialize)]
pub struct Estimate {
    system_mib: u64,
    gpu_mib: u64,
    shared: bool,
}

pub(super) async fn check(
    app: &AppHandle,
    device: &str,
    parallel: usize,
) -> Result<Estimate, String> {
    let install = installation(app)?;
    let device = device.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let tool = install.engine.with_file_name(if cfg!(windows) {
            "llama-fit-params.exe"
        } else {
            "llama-fit-params"
        });
        let output = probe_output(command(&tool).arg("--model").arg(&install.model).args([
            "--device",
            &device,
            "--n-gpu-layers",
            if device == "none" { "0" } else { "99" },
            "--ctx-size",
            &(parallel::CONTEXT_PER_SLOT * parallel).to_string(),
            "--parallel",
            &parallel.to_string(),
            "--flash-attn",
            "on",
            "--fit-print",
            "on",
            "--log-disable",
        ]))
        .map_err(|error| format!("計測前のメモリ見積もりを取得できませんでした。{error}"))?;
        let (host, gpu) = estimate(&output, &device)?;
        let available = available_host_mib()?;
        let shared = cfg!(target_os = "macos");
        let system_mib = if shared {
            host.saturating_add(gpu)
        } else {
            host
        };
        ensure_capacity("システムメモリ", system_mib, available)?;
        if gpu > 0 {
            let devices = probe_output(command(&install.engine).arg("--list-devices"))?;
            let available = parse_devices(&devices)
                .into_iter()
                .find(|candidate| candidate.id == device)
                .and_then(|candidate| gpu_free_mib(&candidate.name))
                .ok_or("GPUの空きメモリを確認できないため計測を省略しました")?;
            ensure_capacity("GPUメモリ", gpu, available)?;
        }
        Ok(Estimate {
            system_mib,
            gpu_mib: gpu,
            shared,
        })
    })
    .await
    .map_err(|_| "計測前のメモリ確認が終了しました".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_host_and_gpu_without_using_another_gpu() {
        let report = "MTL0 1573 14256 3365 \nMTL1 0 0 0\nHost 147 0 112 \n";
        assert_eq!(estimate(report, "MTL0").unwrap(), (259, 19194));
        assert_eq!(estimate(report, "none").unwrap(), (259, 0));
        assert!(estimate(report, "MTL2").is_err());
        assert!(ensure_capacity("GPUメモリ", 19194, 20000).is_err());
        assert!(ensure_capacity("GPUメモリ", 19194, 22000).is_ok());
        assert_eq!(
            gpu_free_mib("Apple M5 Pro (38338 MiB, 38337 MiB free)"),
            Some(38337)
        );
        assert_eq!(macos_available_mib("Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free: 65536.\nPages inactive: 999999.").unwrap(), 1024);
    }
}
