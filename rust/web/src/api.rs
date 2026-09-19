//! Browser-side data access.

use gloo_net::http::Request;
use mindash_core::{Config, Widget};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use wasm_bindgen::prelude::*;

#[derive(Deserialize)]
struct ConfigEnvelope {
    config: Config,
}

#[derive(Deserialize)]
struct WidgetEnvelope {
    data: Value,
}

#[derive(Deserialize)]
struct WidgetCreated {
    widget: Widget,
}

fn base() -> String {
    // Same origin: the server serves the bundle, so no CORS and no configured
    // host. The Python dashboard did the same, which is why it worked over
    // Tailscale without any settings.
    String::new()
}

async fn decode<T: DeserializeOwned>(resp: gloo_net::http::Response) -> Result<T, String> {
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;

    // Distinguish "not signed in" from a generic failure: the middleware returns
    // JSON 401 for API paths, and an expired session should say so rather than
    // surfacing as a parse error.
    if status == 401 {
        return Err("Session expired — reload to sign in again".into());
    }
    if !(200..300).contains(&status) {
        // Prefer the server's message when it sent one.
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if let Some(err) = v.get("error").and_then(Value::as_str) {
                return Err(err.to_string());
            }
        }
        return Err(format!("HTTP {status}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("{e}"))
}

async fn get_json<T: DeserializeOwned>(path: &str) -> Result<T, String> {
    let resp = Request::get(&format!("{}{path}", base()))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    decode(resp).await
}

async fn post_json<T: DeserializeOwned>(path: &str, body: &Value) -> Result<T, String> {
    let resp = Request::post(&format!("{}{path}", base()))
        .json(body)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    decode(resp).await
}

pub async fn get_config() -> Result<Config, String> {
    let resp = Request::get(&format!("{}/api/config", base()))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    decode::<ConfigEnvelope>(resp).await.map(|e| e.config)
}

pub async fn get_widget(id: &str) -> Result<Value, String> {
    let resp = Request::get(&format!("{}/api/widget/{id}", base()))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    decode::<WidgetEnvelope>(resp).await.map(|e| e.data)
}

pub async fn create_widget(kind: mindash_core::WidgetKind) -> Result<Widget, String> {
    let resp = Request::post(&format!("{}/api/widgets", base()))
        .json(&serde_json::json!({ "type": kind }))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    decode::<WidgetCreated>(resp).await.map(|e| e.widget)
}

pub async fn update_widget(id: &str, settings: &Value) -> Result<(), String> {
    let resp = Request::post(&format!("{}/api/widgets/{id}", base()))
        .json(&serde_json::json!({ "settings": settings }))
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    decode::<Value>(resp).await.map(|_| ())
}

pub async fn delete_widget(id: &str) -> Result<(), String> {
    let resp = Request::delete(&format!("{}/api/widgets/{id}", base()))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    decode::<Value>(resp).await.map(|_| ())
}

// ---------------------------------------------------------------------------
// Control plane
//
// Every call addresses a device by id. The server resolves the host, user and
// port from stored config, so the browser never names a machine to connect to.
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceStatus {
    pub id: String,
    #[serde(default)]
    pub online: bool,
}

#[derive(Deserialize)]
struct StatusEnvelope {
    data: Statuses,
}

#[derive(Deserialize)]
struct Statuses {
    devices: Vec<DeviceStatus>,
}

pub async fn device_statuses() -> Result<Vec<DeviceStatus>, String> {
    get_json::<StatusEnvelope>("/api/devices/status")
        .await
        .map(|e| e.data.devices)
}

pub async fn device_stats(id: &str) -> Result<Value, String> {
    #[derive(Deserialize)]
    struct Env {
        data: Value,
    }
    get_json::<Env>(&format!("/api/devices/{id}/stats"))
        .await
        .map(|e| e.data)
}

pub async fn device_power(id: &str, action: &str) -> Result<(), String> {
    post_json::<Value>(
        &format!("/api/devices/{id}/power"),
        &serde_json::json!({ "action": action }),
    )
    .await
    .map(|_| ())
}

pub async fn device_wake(id: &str) -> Result<(), String> {
    post_json::<Value>(&format!("/api/devices/{id}/wake"), &serde_json::json!({}))
        .await
        .map(|_| ())
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mode: String,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Listing {
    pub path: String,
    pub root: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub at_root: bool,
    pub entries: Vec<Entry>,
}

/// Percent-encode a path for a query string.
///
/// `/` is left alone so the readable path survives in logs; everything else that
/// is not unreserved gets escaped, which is what keeps a `&` or `#` in a
/// filename from being read as query syntax.
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub async fn files_list(id: &str, path: &str) -> Result<Listing, String> {
    get_json::<Listing>(&format!(
        "/api/devices/{id}/files?path={}",
        encode_path(path)
    ))
    .await
}

pub async fn file_read(id: &str, path: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Env {
        content: String,
    }
    get_json::<Env>(&format!(
        "/api/devices/{id}/file?path={}",
        encode_path(path)
    ))
    .await
    .map(|e| e.content)
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Container {
    pub device: String,
    pub name: String,
    pub status: String,
    pub running: bool,
}

#[derive(Deserialize)]
struct ContainerEnvelope {
    containers: Vec<Container>,
}

pub async fn containers() -> Result<Vec<Container>, String> {
    get_json::<ContainerEnvelope>("/api/containers")
        .await
        .map(|e| e.containers)
}

pub async fn container_action(name: &str, action: &str) -> Result<(), String> {
    post_json::<Value>(
        "/api/containers/action",
        &serde_json::json!({ "name": name, "action": action }),
    )
    .await
    .map(|_| ())
}

/// Human-readable byte size.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Subscribe to config changes.
///
/// One SSE connection per tab, pushing only on change. The Python dashboard ran
/// a 5 s `setInterval` that re-fetched the config, every device status and every
/// widget, forever, whether or not anything had changed.
pub fn subscribe(mut on_config: impl FnMut(Config) + 'static) {
    use wasm_bindgen::JsCast;

    let Ok(source) = web_sys::EventSource::new("/api/stream") else {
        // No SSE support. The initial fetch has already populated the view, so
        // a stale-but-working page beats an unstoppable polling loop.
        return;
    };

    let cb =
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            if let Some(text) = event.data().as_string() {
                if let Ok(cfg) = serde_json::from_str::<Config>(&text) {
                    on_config(cfg);
                }
            }
        });
    let target: &web_sys::EventTarget = source.as_ref();
    let _ = target.add_event_listener_with_callback("config", cb.as_ref().unchecked_ref());
    // Keep the closure alive for the lifetime of the page.
    cb.forget();
}
