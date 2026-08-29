use std::{
    sync::Mutex,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use sysinfo::{Components, Networks, System};

#[derive(Default)]
pub struct SystemStatusState(Mutex<Option<SystemStatusCache>>);

struct SystemStatusCache {
    system: System,
    networks: Networks,
    components: Components,
    sampled_at: Instant,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerStatus {
    timestamp_ms: u128,
    upload_bytes_per_sec: Option<u64>,
    download_bytes_per_sec: Option<u64>,
    cpu_usage_percent: Option<f32>,
    cpu_temperature_c: Option<f32>,
    gpu_usage_percent: Option<f32>,
    gpu_temperature_c: Option<f32>,
    memory_used_bytes: u64,
    memory_total_bytes: u64,
    memory_usage_percent: Option<f32>,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn round_one(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}

fn sane_temperature(value: Option<f32>) -> Option<f32> {
    value
        .filter(|temperature| {
            temperature.is_finite() && *temperature > -50.0 && *temperature < 130.0
        })
        .map(round_one)
}

fn select_temperature(
    components: &Components,
    needles: &[&str],
    allow_fallback: bool,
) -> Option<f32> {
    let mut fallback = None;
    let mut matched = None;
    for component in components.list() {
        let Some(temperature) = sane_temperature(component.temperature()) else {
            continue;
        };
        fallback = Some(fallback.map_or(temperature, |current: f32| current.max(temperature)));
        let label = component.label().to_ascii_lowercase();
        if needles.iter().any(|needle| label.contains(needle)) {
            matched = Some(matched.map_or(temperature, |current: f32| current.max(temperature)));
        }
    }
    matched.or_else(|| allow_fallback.then_some(fallback).flatten())
}

#[tauri::command]
pub fn computer_status(
    state: tauri::State<'_, SystemStatusState>,
) -> Result<ComputerStatus, String> {
    let mut cache_guard = state
        .0
        .lock()
        .map_err(|_| "电脑状态读取锁已损坏".to_string())?;
    let cache = cache_guard.get_or_insert_with(|| SystemStatusCache {
        system: System::new_all(),
        networks: Networks::new_with_refreshed_list(),
        components: Components::new_with_refreshed_list(),
        sampled_at: Instant::now(),
    });

    let now = Instant::now();
    let elapsed = now.duration_since(cache.sampled_at).as_secs_f64();
    cache.system.refresh_cpu_all();
    cache.system.refresh_memory();
    cache.networks.refresh(true);
    cache.components.refresh(true);

    let downloaded_bytes = cache
        .networks
        .iter()
        .map(|(_, data)| data.received())
        .sum::<u64>();
    let uploaded_bytes = cache
        .networks
        .iter()
        .map(|(_, data)| data.transmitted())
        .sum::<u64>();
    cache.sampled_at = now;

    let network_rate = |bytes: u64| {
        if elapsed >= 0.2 {
            Some((bytes as f64 / elapsed).max(0.0).round() as u64)
        } else {
            None
        }
    };

    let memory_total_bytes = cache.system.total_memory();
    let memory_used_bytes = cache.system.used_memory();
    let memory_usage_percent = if memory_total_bytes > 0 {
        Some(round_one(
            memory_used_bytes as f32 / memory_total_bytes as f32 * 100.0,
        ))
    } else {
        None
    };

    Ok(ComputerStatus {
        timestamp_ms: now_ms(),
        upload_bytes_per_sec: network_rate(uploaded_bytes),
        download_bytes_per_sec: network_rate(downloaded_bytes),
        cpu_usage_percent: Some(round_one(cache.system.global_cpu_usage())),
        cpu_temperature_c: select_temperature(
            &cache.components,
            &["cpu", "package", "die", "core", "soc"],
            true,
        ),
        gpu_usage_percent: None,
        gpu_temperature_c: select_temperature(&cache.components, &["gpu", "graphics"], false),
        memory_used_bytes,
        memory_total_bytes,
        memory_usage_percent,
    })
}
