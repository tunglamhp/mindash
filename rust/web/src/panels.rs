//! Control panels: devices, files, containers.
//!
//! Callbacks are held in `StoredValue` rather than captured directly. A plain
//! closure moved into a `view!` tree can only be used once, which makes any
//! component that needs it in more than one place fail to compile; `StoredValue`
//! is `Copy`, so it can be shared freely.

use leptos::prelude::*;
use serde_json::Value;

use crate::api;

/// Confirm a destructive action. The server refuses host power changes anyway;
/// this is only about not doing something irreversible on a stray click.
fn confirm(message: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.confirm_with_message(message).ok())
        .unwrap_or(false)
}

fn num_at(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

#[component]
pub fn DevicesPanel(config: RwSignal<Option<mindash_core::Config>>) -> impl IntoView {
    let statuses = RwSignal::new(Vec::<api::DeviceStatus>::new());

    let revision = Memo::new(move |_| config.get().map(|c| c.settings.revision).unwrap_or(0));

    Effect::new(move |_| {
        let _ = revision.get();
        leptos::task::spawn_local(async move {
            if let Ok(list) = api::device_statuses().await {
                statuses.set(list);
            }
        });
    });

    view! {
        <section class="section">
            <div class="section-header">
                <h2 class="section-title">"Devices"</h2>
            </div>
            <div class="card-grid">
                {move || {
                    let st = statuses.get();
                    config
                        .get()
                        .map(|c| {
                            c.devices
                                .into_iter()
                                .map(|d| {
                                    let online = d.is_host
                                        || st.iter().any(|s| s.id == d.id && s.online);
                                    view! { <DeviceCard device=d online=online /> }
                                })
                                .collect_view()
                        })
                }}
            </div>
        </section>
    }
}

#[component]
fn DeviceCard(device: mindash_core::Device, online: bool) -> impl IntoView {
    let id = device.id.clone();
    let name = device.display_name().to_string();
    let is_host = device.is_host;
    let has_wol = device.wol.is_some();
    let root = device.files_root();

    let stats = RwSignal::new(None::<Value>);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let files_open = RwSignal::new(false);

    // Callbacks are Copy handles, so the markup can reference them repeatedly.
    let load = StoredValue::new({
        let id = id.clone();
        move || {
            let id = id.clone();
            busy.set(true);
            leptos::task::spawn_local(async move {
                match api::device_stats(&id).await {
                    Ok(v) => {
                        stats.set(Some(v));
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e)),
                }
                busy.set(false);
            });
        }
    });

    let power = StoredValue::new({
        let id = id.clone();
        let name = name.clone();
        move |action: &'static str| {
            let verb = match action {
                "shutdown" => "Shut down",
                "reboot" => "Reboot",
                _ => "Suspend",
            };
            if !confirm(&format!("{verb} {name}?")) {
                return;
            }
            let id = id.clone();
            busy.set(true);
            leptos::task::spawn_local(async move {
                if let Err(e) = api::device_power(&id, action).await {
                    error.set(Some(e));
                }
                busy.set(false);
            });
        }
    });

    let wake = StoredValue::new({
        let id = id.clone();
        move || {
            let id = id.clone();
            busy.set(true);
            leptos::task::spawn_local(async move {
                if let Err(e) = api::device_wake(&id).await {
                    error.set(Some(e));
                }
                busy.set(false);
            });
        }
    });

    // The stats body is built once per data change. `content` is Fn because
    // everything it captures is Copy or was cloned before the closure.
    let content = move || {
        stats.get().map(|s| {
            let cpu = num_at(&s, "cpu");
            let total = num_at(&s, "ram_total").unwrap_or(0);
            let used = num_at(&s, "ram_used").unwrap_or(0);
            let pct = if total > 0 { used * 100 / total } else { 0 };
            let temp = num_at(&s, "temp");
            let uptime = s
                .get("uptime")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let disks: Vec<(String, u64, u64)> = s
                .get("disks")
                .and_then(Value::as_array)
                .map(|ds| {
                    ds.iter()
                        .map(|d| {
                            (
                                d.get("mount")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string(),
                                d.get("used").and_then(Value::as_u64).unwrap_or(0),
                                d.get("total").and_then(Value::as_u64).unwrap_or(0),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();

            let ram = if total > 0 {
                format!("{pct}%")
            } else {
                "—".to_string()
            };
            let disk_rows = disks
                .into_iter()
                .map(|(mount, used, total)| {
                    let pct = if total > 0 { used * 100 / total } else { 0 };
                    let used_h = api::human_size(used);
                    let total_h = api::human_size(total);
                    view! {
                        <li class="row">
                            <span class="mono row-name">{mount}</span>
                            <span class="muted row-meta">
                                {used_h} " / " {total_h} " · " {pct} "%"
                            </span>
                        </li>
                    }
                })
                .collect_view();

            view! {
                <div class="stat-row">
                    {cpu
                        .map(|c| {
                            view! {
                                <div class="stat">
                                    <div class="stat-value">{c} "%"</div>
                                    <div class="stat-label">"CPU"</div>
                                </div>
                            }
                        })}
                    <div class="stat">
                        <div class="stat-value">{ram}</div>
                        <div class="stat-label">"RAM " {api::human_size(used as u64)}</div>
                    </div>
                    {temp
                        .map(|t| {
                            view! {
                                <div class="stat">
                                    <div class="stat-value">{t} "°C"</div>
                                    <div class="stat-label">"Temp"</div>
                                </div>
                            }
                        })}
                    <div class="stat">
                        <div class="stat-value">{uptime}</div>
                        <div class="stat-label">"Uptime"</div>
                    </div>
                </div>
                <ul class="rows">{disk_rows}</ul>
            }
        })
    };

    let host_actions = (!is_host).then(|| {
        view! {
            <button class="chip" type="button" disabled=move || busy.get()
                on:click=move |_| power.with_value(|p| p("reboot"))>"Reboot"</button>
            <button class="chip" type="button" disabled=move || busy.get()
                on:click=move |_| power.with_value(|p| p("suspend"))>"Suspend"</button>
            <button class="chip chip-danger" type="button" disabled=move || busy.get()
                on:click=move |_| power.with_value(|p| p("shutdown"))>"Shut down"</button>
        }
    });
    let wake_button = has_wol.then(|| {
        view! {
            <button class="chip" type="button" disabled=move || busy.get()
                on:click=move |_| wake.with_value(|w| w())>"Wake"</button>
        }
    });

    let device_id = id.clone();
    let files_root = root.clone();
    let files_label = move || {
        if files_open.get() {
            "Hide files"
        } else {
            "Files"
        }
    };

    view! {
        <article class="card device">
            <div class="card-head">
                <span class="dot" class:online=online></span>
                <span class="card-title">{name}</span>
                {is_host.then(|| view! { <span class="pill">"host"</span> })}
            </div>

            {move || {
                error.get().map(|e| view! { <p class="card-error">{e}</p> })
            }}

            {content}

            <div class="card-tools card-tools-wide">
                <button class="chip" type="button" disabled=move || busy.get()
                    on:click=move |_| load.with_value(|f| f())>
                    {move || if stats.get().is_some() { "Refresh" } else { "Load stats" }}
                </button>
                {host_actions}
                {wake_button}
                <button
                    class="chip"
                    type="button"
                    aria-expanded=move || files_open.get().to_string()
                    on:click=move |_| files_open.update(|o| *o = !*o)
                >
                    {files_label}
                </button>
            </div>

            {move || {
                files_open
                    .get()
                    .then(|| {
                        view! { <FileBrowser device=device_id.clone() root=files_root.clone() /> }
                    })
            }}
        </article>
    }
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

#[component]
pub fn FileBrowser(device: String, root: String) -> impl IntoView {
    let listing = RwSignal::new(None::<api::Listing>);
    let error = RwSignal::new(None::<String>);
    let preview = RwSignal::new(None::<(String, String)>);
    let loading = RwSignal::new(false);

    let load = StoredValue::new({
        let device = device.clone();
        move |path: String| {
            let device = device.clone();
            loading.set(true);
            leptos::task::spawn_local(async move {
                match api::files_list(&device, &path).await {
                    Ok(l) => {
                        listing.set(Some(l));
                        preview.set(None);
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e)),
                }
                loading.set(false);
            });
        }
    });

    let open = StoredValue::new({
        let device = device.clone();
        move |entry: api::Entry| {
            if entry.is_dir {
                load.with_value(|f| f(entry.path));
            } else {
                let device = device.clone();
                leptos::task::spawn_local(async move {
                    match api::file_read(&device, &entry.path).await {
                        Ok(text) => {
                            preview.set(Some((entry.path, text)));
                            error.set(None);
                        }
                        Err(e) => error.set(Some(e)),
                    }
                });
            }
        }
    });

    // Load the root once, on mount.
    {
        let root = root.clone();
        Effect::new(move |_| {
            load.with_value(|f| f(root.clone()));
        });
    }

    let body = move || -> AnyView {
        let Some(l) = listing.get() else {
            return ().into_any();
        };
        let crumbs: Vec<(String, String)> = {
            let mut acc = String::new();
            l.path
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|seg| {
                    acc.push('/');
                    acc.push_str(seg);
                    (seg.to_string(), acc.clone())
                })
                .collect()
        };
        let parent = l.parent.clone();
        let at_root = l.at_root;
        let count = l.entries.len();

        let crumb_buttons = crumbs
            .into_iter()
            .map(|(name, full)| {
                view! {
                    <button class="crumb" type="button"
                        on:click=move |_| load.with_value(|f| f(full.clone()))>{name}</button>
                }
            })
            .collect_view();

        let rows = l
            .entries
            .into_iter()
            .map(|e| {
                let entry = e.clone();
                let is_dir = e.is_dir;
                let size = if is_dir {
                    String::new()
                } else {
                    api::human_size(e.size)
                };
                let name = e.name.clone();
                // The marker is its own element, marked aria-hidden because it
                // is decoration: a screen reader should read the name, not the
                // glyph. Every row reserves the same slot so names line up.
                let marker = is_dir
                    .then(|| view! { <span class="row-mark" aria-hidden="true">"▸"</span> });
                view! {
                    <li class="row row-click" on:click=move |_| open.with_value(|f| f(entry.clone()))>
                        <span class="row-mark-slot">{marker}</span>
                        <span class="mono row-name">{name}</span>
                        <span class="muted row-meta">{size}</span>
                    </li>
                }
            })
            .collect_view();

        let root_click = {
            let root = root.clone();
            view! {
                <button class="crumb" type="button"
                    on:click=move |_| load.with_value(|f| f(root.clone()))>"root"</button>
            }
        };

        let up = (parent.is_some() && !at_root).then(|| {
            let parent = parent.clone();
            view! {
                <button class="chip" type="button"
                    on:click=move |_| {
                        if let Some(p) = parent.clone() {
                            load.with_value(|f| f(p));
                        }
                    }>"Up"</button>
            }
        });

        // Shown so the confinement boundary is visible rather than implied: this
        // is the directory the browser cannot be navigated out of.
        let confined_to = l.root.clone();
        view! {
            <div class="crumbs">{root_click} {crumb_buttons}</div>
            <div class="browser-actions">
                {up}
                <span class="muted tiny">{count} " items"</span>
                <span class="muted tiny browser-root" title="The browser cannot go above this">
                    "confined to " {confined_to}
                </span>
            </div>
            <ul class="rows browser-list">{rows}</ul>
        }
        .into_any()
    };

    let preview_block = move || {
        preview.get().map(|(path, text)| {
            view! {
                <div class="preview">
                    <div class="preview-head">
                        <span class="mono tiny muted">{path}</span>
                        <button class="chip" type="button"
                            on:click=move |_| preview.set(None)>"Close"</button>
                    </div>
                    <pre class="preview-body">{text}</pre>
                </div>
            }
        })
    };

    view! {
        <div class="browser">
            {move || error.get().map(|e| view! { <p class="card-error">{e}</p> })}
            {body}
            {move || loading.get().then(|| view! { <p class="muted small">"Loading…"</p> })}
            {preview_block}
        </div>
    }
}

// ---------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------

#[component]
pub fn ContainersPanel(config: RwSignal<Option<mindash_core::Config>>) -> impl IntoView {
    let items = RwSignal::new(Vec::<api::Container>::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(None::<String>);

    let revision = Memo::new(move |_| config.get().map(|c| c.settings.revision).unwrap_or(0));

    let reload = StoredValue::new(move || {
        leptos::task::spawn_local(async move {
            match api::containers().await {
                Ok(list) => {
                    items.set(list);
                    error.set(None);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    Effect::new(move |_| {
        let _ = revision.get();
        reload.with_value(|f| f());
    });

    let act = StoredValue::new(move |name: String, action: &'static str| {
        if action == "stop" && !confirm(&format!("Stop {name}?")) {
            return;
        }
        busy.set(Some(name.clone()));
        leptos::task::spawn_local(async move {
            if let Err(e) = api::container_action(&name, action).await {
                error.set(Some(e));
            }
            // Re-read so the row reflects what actually happened rather than
            // what we hoped would happen.
            if let Ok(list) = api::containers().await {
                items.set(list);
            }
            busy.set(None);
        });
    });

    let body = move || -> AnyView {
        let list = items.get();
        if list.is_empty() {
            return view! {
                <p class="muted small">"No containers listed yet."</p>
            }
            .into_any();
        }
        let current_busy = busy.get();
        let rows = list
            .into_iter()
            .map(|c| {
                let name = c.name.clone();
                let running = c.running;
                let dev = c.device.clone();
                let is_busy = current_busy.as_deref() == Some(c.name.as_str());

                let buttons = if running {
                    let n1 = name.clone();
                    let n2 = name.clone();
                    view! {
                        <button class="chip chip-danger" type="button" disabled=is_busy
                            on:click=move |_| act.with_value(|f| f(n1.clone(), "stop"))>"Stop"</button>
                        <button class="chip" type="button" disabled=is_busy
                            on:click=move |_| act.with_value(|f| f(n2.clone(), "restart"))>"Restart"</button>
                    }
                    .into_any()
                } else {
                    let n = name.clone();
                    view! {
                        <button class="chip" type="button" disabled=is_busy
                            on:click=move |_| act.with_value(|f| f(n.clone(), "start"))>"Start"</button>
                    }
                    .into_any()
                };

                view! {
                    <li class="row">
                        <span class="dot" class:online=running></span>
                        <span class="mono row-name">{name}</span>
                        <span class="muted row-meta">{dev}</span>
                        <span class="row-actions">{buttons}</span>
                    </li>
                }
            })
            .collect_view();

        view! {
            <div class="card container-card">
                <ul class="rows">{rows}</ul>
            </div>
        }
        .into_any()
    };

    view! {
        <section class="section">
            <div class="section-header">
                <h2 class="section-title">"Containers"</h2>
                <button class="chip" type="button" on:click=move |_| reload.with_value(|f| f())>
                    "Refresh"
                </button>
            </div>
            {move || {
                error
                    .get()
                    .map(|e| view! { <div class="notice notice-error">{e}</div> })
            }}
            <div class="card-grid">{body}</div>
        </section>
    }
}
