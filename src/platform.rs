pub fn now_epoch() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() / 1000.0) as u64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

pub fn now_nanos_seed() -> u128 {
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() * 1_000_000.0) as u128
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(1)
    }
}

#[cfg(target_arch = "wasm32")]
fn browser_local_storage() -> Result<web_sys::Storage, String> {
    web_sys::window()
        .ok_or_else(|| "browser window unavailable".to_string())?
        .local_storage()
        .map_err(|_| "localStorage access failed".to_string())?
        .ok_or_else(|| "localStorage unavailable".to_string())
}

#[cfg(target_arch = "wasm32")]
pub fn browser_storage_get(key: &str) -> Option<String> {
    browser_local_storage().ok()?.get_item(key).ok().flatten()
}

#[cfg(target_arch = "wasm32")]
pub fn browser_storage_set(key: &str, value: &str) -> Result<(), String> {
    browser_local_storage()?
        .set_item(key, value)
        .map_err(|_| format!("localStorage write failed for {key}"))
}

#[cfg(target_arch = "wasm32")]
pub fn browser_storage_remove(key: &str) -> Result<(), String> {
    browser_local_storage()?
        .remove_item(key)
        .map_err(|_| format!("localStorage remove failed for {key}"))
}
