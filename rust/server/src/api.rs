//! HTTP handlers.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use mindash_core::widgets::{all_types, coerce_settings, WidgetKind};
use mindash_core::Widget;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::AppState;

pub async fn version() -> Json<Value> {
    Json(json!({
        "name": "MinDash",
        "version": env!("CARGO_PKG_VERSION"),
        "runtime": "rust",
    }))
}

pub async fn get_config(State(state): State<Arc<AppState>>) -> Json<Value> {
    let mut cfg = state.config().await;
    // The revision is how the dashboard knows to re-fetch widget data. Carrying
    // it in the payload means a client that missed an SSE frame still converges.
    cfg.settings.revision = state.revision();
    Json(json!({
        "success": true,
        "auth_enabled": state.auth_enabled(),
        "config": cfg,
        "widget_types": all_types(),
    }))
}

pub async fn put_config(
    State(state): State<Arc<AppState>>,
    Json(incoming): Json<mindash_core::Config>,
) -> Json<Value> {
    // The whole document is replaced, but widgets are preserved when the caller
    // omits them: the dashboard saves its whole config on every edit and an
    // older client would otherwise wipe the widget list.
    let result = state
        .update_config(|cfg| {
            let widgets = std::mem::take(&mut cfg.widgets);
            let had_widgets = incoming.widgets.is_empty() && !widgets.is_empty();
            *cfg = incoming;
            if had_widgets {
                cfg.widgets = widgets;
            }
        })
        .await;

    match result {
        Ok(()) => Json(json!({ "success": true })),
        Err(e) => Json(json!({ "success": false, "error": e.to_string() })),
    }
}

pub async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let cfg = state.config().await;
    Json(json!({
        "success": true,
        "uptime_secs": state.started.elapsed().as_secs(),
        "devices": cfg.devices.len(),
        "widgets": cfg.widgets.len(),
        "revision": state.revision(),
    }))
}

pub async fn widget_types() -> Json<Value> {
    Json(json!({ "success": true, "types": all_types() }))
}

#[derive(Deserialize)]
pub struct NewWidget {
    #[serde(rename = "type")]
    kind: WidgetKind,
    #[serde(default)]
    settings: Value,
}

pub async fn create_widget(
    State(state): State<Arc<AppState>>,
    Json(body): Json<NewWidget>,
) -> Response {
    let id = format!(
        "{}-{}",
        body.kind.as_str(),
        // Monotonic-ish suffix without pulling in a uuid crate.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0)
    );
    let mut widget = Widget::new(id, body.kind);
    if !body.settings.is_null() {
        widget.settings = coerce_settings(body.kind, &body.settings);
    }

    let created = widget.clone();
    if let Err(e) = state
        .update_config(move |cfg| cfg.widgets.push(widget))
        .await
    {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    (
        StatusCode::CREATED,
        Json(json!({ "success": true, "widget": created })),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct WidgetPatch {
    #[serde(default)]
    settings: Option<Value>,
    #[serde(default)]
    title: Option<String>,
}

pub async fn update_widget(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<WidgetPatch>,
) -> Response {
    let mut found = false;
    let mut updated: Option<Widget> = None;
    let mut error: Option<String> = None;

    let result = state
        .update_config(|cfg| match cfg.widgets.iter_mut().find(|w| w.id == id) {
            None => {}
            Some(w) => {
                found = true;
                if let Some(s) = &body.settings {
                    // Coerced against the registry, so unknown keys cannot reach
                    // the renderer and numbers are clamped.
                    w.settings = coerce_settings(w.kind, s);
                }
                if let Some(t) = &body.title {
                    let t = t.trim();
                    if !t.is_empty() {
                        w.title = t.chars().take(60).collect();
                    }
                }
                updated = Some(w.clone());
            }
        })
        .await;

    if let Err(e) = result {
        error = Some(e.to_string());
    }
    if let Some(e) = error {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, &e);
    }
    if !found {
        return json_error(StatusCode::NOT_FOUND, "Widget not found");
    }
    Json(json!({ "success": true, "widget": updated })).into_response()
}

pub async fn delete_widget(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let mut removed = false;
    let result = state
        .update_config(|cfg| {
            let before = cfg.widgets.len();
            cfg.widgets.retain(|w| w.id != id);
            removed = cfg.widgets.len() != before;
            if removed {
                let key = format!("w-{id}");
                cfg.settings.section_order.retain(|s| s != &key);
            }
        })
        .await;

    match result {
        Err(e) => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        Ok(()) if !removed => json_error(StatusCode::NOT_FOUND, "Widget not found"),
        Ok(()) => Json(json!({ "success": true })).into_response(),
    }
}

/// Live data for one widget.
///
/// Takes an id, never a URL: what gets fetched is decided entirely by stored
/// config, so this endpoint cannot be turned into an SSRF primitive.
pub async fn widget_data(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let cfg = state.config().await;
    let Some(widget) = cfg.widgets.iter().find(|w| w.id == id) else {
        return json_error(StatusCode::NOT_FOUND, "Widget not found");
    };

    let devices = device_statuses(&cfg).await;
    let data = crate::widgets::data_for(&state, widget, &devices).await;
    Json(json!({ "success": true, "type": widget.kind, "data": data })).into_response()
}

/// Collect a status snapshot for every device.
async fn device_statuses(cfg: &mindash_core::Config) -> Value {
    let mut out = Vec::new();
    for device in &cfg.devices {
        let (online, containers) = if device.is_host {
            let states = crate::net::container_states().await.unwrap_or_default();
            (!states.is_empty(), states)
        } else {
            (
                crate::net::tcp_reachable(&device.ip, 22, Duration::from_millis(600)).await,
                Default::default(),
            )
        };
        out.push(json!({
            "id": device.id,
            "online": online,
            "containers": containers,
            "stats": Value::Null,
        }));
    }
    Value::Array(out)
}

/// Server-sent events.
///
/// The Python dashboard polled every 5 seconds from every open tab. This keeps
/// one connection per client and pushes only when the config revision changes,
/// so an idle dashboard costs nothing.
pub async fn stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let stream = async_stream::stream! {
        let mut last_revision = 0;
        let mut ticker = tokio::time::interval(Duration::from_secs(2));
        loop {
            ticker.tick().await;
            let revision = state.revision();
            if revision == last_revision {
                continue;
            }
            last_revision = revision;
            let mut cfg = state.config().await;
            cfg.settings.revision = revision;
            let payload = serde_json::to_string(&cfg).unwrap_or_else(|_| "{}".into());
            yield Ok(Event::default().event("config").data(payload));
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub async fn index(State(state): State<Arc<AppState>>) -> Response {
    serve_static(&state, "index.html", "text/html; charset=utf-8").await
}

pub async fn login_page(State(state): State<Arc<AppState>>) -> Response {
    serve_static(&state, "login.html", "text/html; charset=utf-8").await
}

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "success": false, "error": message }))).into_response()
}

async fn serve_static(state: &AppState, name: &str, content_type: &str) -> Response {
    serve_static_path(state, &[name], content_type).await
}

async fn serve_static_path(state: &AppState, parts: &[&str], content_type: &str) -> Response {
    // Every component is reduced to a file name; nothing can climb out.
    let mut path = state.static_dir.clone();
    for part in parts {
        let Some(name) = std::path::Path::new(part).file_name() else {
            return json_error(StatusCode::NOT_FOUND, "Not found");
        };
        path.push(name);
    }

    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "public, max-age=3600"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => json_error(StatusCode::NOT_FOUND, "Not found"),
    }
}
