//! Control-plane handlers: stats, power, wake-on-LAN, containers, files.
//!
//! Every handler here resolves a device by id out of the stored config and
//! derives the SSH target from it. Nothing accepts a host, a user, a port or a
//! command from the request, so there is no route that can be pointed at an
//! arbitrary machine or made to run an arbitrary string.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use mindash_core::Device;
use serde::Deserialize;
use serde_json::json;

use crate::exec::{self, FileError, PowerAction, SshTarget};
use crate::net::{self, ContainerAction};
use crate::state::AppState;

/// How long a device status is reused before it is re-checked.
const STATUS_TTL: Duration = Duration::from_secs(15);
/// Largest file the viewer will load.
const MAX_PREVIEW_BYTES: u64 = 512 * 1024;

fn json_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "success": false, "error": message }))).into_response()
}

/// Look up a device, or produce a 404.
async fn device_by_id(state: &AppState, id: &str) -> Result<Device, Response> {
    state
        .config()
        .await
        .devices
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| json_error(StatusCode::NOT_FOUND, "Unknown device"))
}

/// Build an SSH target from stored config. Never from request input.
fn target_for(device: &Device) -> SshTarget {
    SshTarget {
        ip: device.ip.clone(),
        user: device.ssh_user(),
        port: device.ssh_port(),
        // A per-device key would go here; the local test target is wired through
        // an env var so it never has to be stored in the config.
        key: std::env::var("MINDASH_SSH_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty()),
    }
}

// ---------------------------------------------------------------------------
// Status and stats
// ---------------------------------------------------------------------------

/// Reachability for every device, in parallel.
///
/// The Python dashboard polled each host in sequence on a timer, so one
/// unresponsive machine delayed every card behind it. These run concurrently and
/// are cached briefly, because the dashboard asks for this on every render.
pub async fn devices_status(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let devices = state.config().await.devices;

    let results = state
        .cached("device-status", STATUS_TTL, || async {
            let checks = devices.iter().map(|device| async move {
                let online = if device.is_host {
                    true
                } else {
                    // A TCP probe first: it is much cheaper than an SSH handshake
                    // and answers "is it up" on machines with no SSH at all.
                    net::tcp_reachable(&device.ip, device.ssh_port(), Duration::from_millis(900))
                        .await
                };
                json!({ "id": device.id, "online": online })
            });
            let all = futures_util::future::join_all(checks).await;
            Ok(json!({ "devices": all }))
        })
        .await;

    Json(json!({ "success": true, "data": results }))
}

/// CPU, memory, temperature, uptime and disks for one device.
pub async fn device_stats(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let device = match device_by_id(&state, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    let result = if device.is_local() {
        // The local machine is read directly. Going out to an sshd and back
        // would be slower, would need SSH configured on a single-machine
        // install for no reason, and would fail on Windows where the POSIX
        // collector has no `sh`.
        match exec::local_stats().await {
            Ok(stats) => Ok(stats),
            // A local address is not proof the machine is this one -- someone may
            // have configured a loopback port-forward to a remote host -- so
            // fall back to SSH before giving up.
            Err(local_err) => match exec::remote_stats(&target_for(&device)).await {
                Ok(stats) => Ok(stats),
                Err(_) => Err(local_err),
            },
        }
    } else {
        exec::remote_stats(&target_for(&device)).await
    };

    match result {
        Ok(stats) => Json(json!({ "success": true, "data": stats })).into_response(),
        Err(e) => json_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Power
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PowerBody {
    action: PowerAction,
}

pub async fn device_power(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<PowerBody>,
) -> Response {
    let device = match device_by_id(&state, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    if device.is_host {
        // Shutting down the machine the server runs on would end the session
        // mid-request and leave the user with a dead dashboard and no answer.
        return json_error(
            StatusCode::BAD_REQUEST,
            "Refusing to change power state of the MinDash host",
        );
    }

    let target = target_for(&device);
    match exec::power(&target, body.action).await {
        Ok(()) => {
            // Invalidate the cached status so the next read reflects reality.
            state.cache.write().await.remove("device-status");
            Json(json!({ "success": true })).into_response()
        }
        Err(e) => json_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// Wake a device over the network.
pub async fn device_wake(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let device = match device_by_id(&state, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let Some(wol) = device.wol.as_ref() else {
        return json_error(StatusCode::BAD_REQUEST, "No wake-on-LAN MAC configured");
    };

    let broadcast = if wol.broadcast.trim().is_empty() {
        // Derive the subnet broadcast from the address, falling back to the
        // limited broadcast address.
        device
            .ip
            .rsplit_once('.')
            .map(|(prefix, _)| format!("{prefix}.255"))
            .unwrap_or_else(|| "255.255.255.255".to_string())
    } else {
        wol.broadcast.clone()
    };

    match net::wake_on_lan(&wol.mac, &broadcast) {
        Ok(()) => Json(json!({ "success": true, "broadcast": broadcast })).into_response(),
        Err(e) => json_error(StatusCode::BAD_REQUEST, &e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ContainerBody {
    name: String,
    action: ContainerAction,
}

pub async fn containers_list(State(state): State<Arc<AppState>>) -> Response {
    let devices = state.config().await.devices;

    // Every device that has containers configured, plus the host itself so a
    // single-machine install still shows something.
    let mut out = Vec::new();
    for device in &devices {
        let wanted: Vec<String> = device
            .docker
            .as_ref()
            .map(|d| d.containers.iter().map(|c| c.name().to_string()).collect())
            .unwrap_or_default();

        if !device.is_host && wanted.is_empty() {
            continue;
        }

        // The state map is built the same way for both paths: name -> status.
        let states: std::collections::HashMap<String, String> = if device.is_local() {
            net::container_states().await.unwrap_or_default()
        } else {
            let target = target_for(device);
            // One round trip, same format as the local path.
            match target
                .run(
                    "docker ps -a --format '{{.Names}}:{{.State}}'",
                    Duration::from_secs(10),
                )
                .await
            {
                Ok(raw) => raw
                    .lines()
                    .filter_map(|l| l.split_once(':'))
                    .map(|(n, s)| (n.to_string(), s.to_ascii_lowercase()))
                    .collect(),
                Err(_) => Default::default(),
            }
        };

        let mut containers: Vec<serde_json::Value> = states
            .iter()
            .filter(|(name, _)| wanted.is_empty() || wanted.contains(name))
            .map(|(name, status)| {
                json!({
                    "device": device.id,
                    "name": name,
                    "status": status,
                    "running": status == "running",
                })
            })
            .collect();
        containers.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        });
        out.extend(containers);
    }

    Json(json!({ "success": true, "containers": out })).into_response()
}

pub async fn container_control(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ContainerBody>,
) -> Response {
    // The device is looked up rather than supplied, and for now only the local
    // daemon is driven. A remote device needs its own container listing to be
    // useful, which the listing endpoint above already provides.
    let cfg = state.config().await;
    let known = cfg
        .devices
        .iter()
        .filter_map(|d| d.docker.as_ref())
        .flat_map(|d| d.containers.iter())
        .any(|c| c.name() == body.name);

    if !known {
        return json_error(
            StatusCode::FORBIDDEN,
            "That container is not in the configured list",
        );
    }

    match net::container_action(&body.name, body.action).await {
        Ok(()) => Json(json!({ "success": true })).into_response(),
        Err(e) => json_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

/// Available containers on the host, for the config editor to offer.
pub async fn containers_available() -> Response {
    match net::container_states().await {
        Ok(states) => {
            let mut names: Vec<String> = states.keys().cloned().collect();
            names.sort();
            Json(json!({ "success": true, "names": names })).into_response()
        }
        Err(e) => {
            Json(json!({ "success": true, "names": [], "note": e.to_string() })).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PathQuery {
    #[serde(default)]
    path: String,
}

pub async fn files_list(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> Response {
    let device = match device_by_id(&state, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let root = device.files_root();

    // The host is read locally; everything else over SSH. See the note in
    // `device_stats` for why.
    let result = if device.is_local() {
        exec::list_dir_local(&q.path, &root).await
    } else {
        exec::list_dir(&target_for(&device), &q.path, &root).await
    };

    match result {
        Ok((resolved, entries)) => {
            // No parent above the configured root: the browser must not offer a
            // way to walk out of the confinement, and at the root there is
            // nowhere legal to go up to.
            let at_root = resolved == root || resolved == "/";
            let parent = if at_root {
                None
            } else {
                resolved
                    .rsplit_once('/')
                    .map(|(p, _)| {
                        if p.is_empty() {
                            "/".to_string()
                        } else {
                            p.to_string()
                        }
                    })
                    .filter(|p| p != &resolved)
            };

            Json(json!({
                "success": true,
                "root": root,
                "path": resolved,
                "at_root": at_root,
                "parent": parent,
                "entries": entries,
            }))
            .into_response()
        }
        Err(FileError::Escape) => json_error(
            StatusCode::FORBIDDEN,
            "That path is outside the configured root",
        ),
        Err(e) => json_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}

pub async fn files_read(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<PathQuery>,
) -> Response {
    let device = match device_by_id(&state, &id).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let root = device.files_root();

    let result = if device.is_local() {
        exec::read_file_local(&q.path, &root, MAX_PREVIEW_BYTES).await
    } else {
        exec::read_file(&target_for(&device), &q.path, &root, MAX_PREVIEW_BYTES).await
    };

    match result {
        Ok(content) => {
            // Binary files would render as replacement characters and pollute the
            // JSON, so say so instead of shipping a broken preview.
            let binary = content.as_bytes().iter().take(4096).any(|b| *b == 0);
            if binary {
                return json_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Binary file");
            }
            Json(json!({ "success": true, "content": content })).into_response()
        }
        Err(FileError::Escape) => json_error(
            StatusCode::FORBIDDEN,
            "That path is outside the configured root",
        ),
        Err(e) => json_error(StatusCode::BAD_GATEWAY, &e.to_string()),
    }
}
