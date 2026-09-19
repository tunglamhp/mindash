//! Widget rendering and the widget editor.

use leptos::prelude::*;
use serde_json::Value;

use mindash_core::widgets::{all_types, Field, FieldKind, WidgetKind};
use mindash_core::Widget;

use crate::api;

/// One widget card.
///
/// Data is re-fetched when the config revision changes rather than on a timer:
/// the server pushes a new revision over SSE whenever anything is edited, so an
/// idle dashboard makes no requests at all.
#[component]
pub fn WidgetCard(
    widget: Widget,
    config: RwSignal<Option<mindash_core::Config>>,
    edit_mode: RwSignal<bool>,
) -> impl IntoView {
    let id = widget.id.clone();
    let kind = widget.kind;
    let title = widget.resolved_title().to_string();
    let settings = widget.settings.clone();

    let data = RwSignal::new(None::<Value>);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let editor_open = RwSignal::new(false);

    let revision = Memo::new(move |_| config.get().map(|c| c.settings.revision).unwrap_or(0));

    let fetch_id = id.clone();
    Effect::new(move |_| {
        // Depends on the revision, which a server-side edit bumps.
        let _ = revision.get();
        let id = fetch_id.clone();
        leptos::task::spawn_local(async move {
            match api::get_widget(&id).await {
                Ok(v) => {
                    data.set(Some(v));
                    error.set(None);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    // Local clock: no server round trip for a ticking display.
    let clock_tick = RwSignal::new(0_u32);
    if kind == WidgetKind::Clock {
        leptos::task::spawn_local(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(1000).await;
                clock_tick.update(|n| *n = n.wrapping_add(1));
            }
        });
    }

    let on_save = Callback::new({
        let id = id.clone();
        move |next: Value| {
            let id = id.clone();
            busy.set(true);
            leptos::task::spawn_local(async move {
                match api::update_widget(&id, &next).await {
                    Ok(()) => {
                        editor_open.set(false);
                        refresh(config).await;
                    }
                    Err(e) => error.set(Some(e)),
                }
                busy.set(false);
            });
        }
    });

    let on_delete = {
        let id = id.clone();
        move |_| {
            let id = id.clone();
            busy.set(true);
            leptos::task::spawn_local(async move {
                if let Err(e) = api::delete_widget(&id).await {
                    error.set(Some(e));
                }
                busy.set(false);
                refresh(config).await;
            });
        }
    };

    let spec = kind.spec();

    view! {
        <article class="card widget" data-kind=kind.as_str()>
            <div class="card-head">
                <span class="card-title">{title}</span>
                <Show when=move || edit_mode.get()>
                    <div class="card-tools">
                        <button
                            class="chip"
                            type="button"
                            aria-label="Configure this widget"
                            on:click=move |_| editor_open.update(|o| *o = !*o)
                        >
                            {move || if editor_open.get() { "Close" } else { "Configure" }}
                        </button>
                        <button
                            class="chip chip-danger"
                            type="button"
                            aria-label="Remove this widget"
                            on:click=on_delete.clone()
                        >
                            "Remove"
                        </button>
                    </div>
                </Show>
            </div>
            <Show when=move || error.get().is_some()>
                <p class="card-error">{move || error.get().unwrap_or_default()}</p>
            </Show>
            <Show when=move || editor_open.get()>
                <WidgetForm
                    fields=spec.fields
                    initial=Value::Object(settings.clone())
                    on_save=on_save
                />
            </Show>
            <Show when=move || data.get().is_some() && !editor_open.get()>
                <div class="widget-body">
                    {move || {
                        data.get()
                            .map(|d| render_body(kind, &d, clock_tick.get()))
                    }}
                </div>
            </Show>
        </article>
    }
}

/// Re-read the config from the server.
pub async fn refresh(config: RwSignal<Option<mindash_core::Config>>) {
    if let Ok(cfg) = api::get_config().await {
        config.set(Some(cfg));
    }
}

/// Widget picker, shown from the header while in edit mode.
#[component]
pub fn WidgetPicker(
    open: RwSignal<bool>,
    config: RwSignal<Option<mindash_core::Config>>,
    error: RwSignal<Option<String>>,
) -> impl IntoView {
    let busy = RwSignal::new(None::<WidgetKind>);

    let add = move |kind: WidgetKind| {
        if busy.get().is_some() {
            return;
        }
        busy.set(Some(kind));
        leptos::task::spawn_local(async move {
            match api::create_widget(kind).await {
                Ok(_) => {
                    open.set(false);
                    refresh(config).await;
                }
                Err(e) => error.set(Some(e)),
            }
            busy.set(None);
        });
    };

    view! {
        <Show when=move || open.get()>
            <div class="modal-scrim" on:click=move |_| open.set(false)></div>
            <div class="modal" role="dialog" aria-modal="true" aria-label="Add a widget">
                <div class="modal-head">
                    <h2 class="modal-title">"Add a widget"</h2>
                    <button
                        class="chip"
                        type="button"
                        aria-label="Close"
                        on:click=move |_| open.set(false)
                    >
                        "Close"
                    </button>
                </div>
                <p class="muted small">
                    "Widgets live in your config — nothing to install, no restart."
                </p>
                <div class="picker-grid">
                    {all_types()
                        .iter()
                        .map(|t| {
                            let kind = t.kind;
                            let label = t.label;
                            let count = t.fields.len();
                            view! {
                                <button
                                    class="picker-item"
                                    type="button"
                                    disabled=move || busy.get().is_some()
                                    on:click=move |_| add(kind)
                                >
                                    <span class="picker-text">
                                        <span class="picker-name">{label}</span>
                                        <span class="muted small">{count} " settings"</span>
                                    </span>
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </div>
        </Show>
    }
}

fn put(values: RwSignal<Value>, key: &'static str, value: Value) {
    values.update(|v| {
        if !v.is_object() {
            *v = Value::Object(Default::default());
        }
        if let Some(o) = v.as_object_mut() {
            o.insert(key.to_string(), value);
        }
    });
}

fn render_body(kind: WidgetKind, data: &Value, _tick: u32) -> AnyView {
    if let Some(err) = data.get("error").and_then(Value::as_str) {
        return view! { <p class="card-error">{err.to_string()}</p> }.into_any();
    }
    match kind {
        WidgetKind::Weather => weather_view(data),
        WidgetKind::Overview => overview_view(data),
        WidgetKind::Containers => containers_view(data),
        WidgetKind::Links => links_view(data),
        WidgetKind::Rss => rss_view(data),
        WidgetKind::Clock => clock_view(),
    }
}

fn clock_view() -> AnyView {
    // Read the browser clock directly; this is a client-rendered app, so there
    // is no server clock to reconcile with.
    let now = js_sys::Date::new_0();
    let stamp = format!(
        "{:02}:{:02}:{:02}",
        now.get_hours(),
        now.get_minutes(),
        now.get_seconds()
    );
    view! {
        <div class="stat-row">
            <div class="stat">
                <div class="stat-value mono">{stamp}</div>
                <div class="stat-label">"Local time"</div>
            </div>
        </div>
    }
    .into_any()
}

fn str_at(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn weather_view(data: &Value) -> AnyView {
    let temp = str_at(data, "temp");
    let units = str_at(data, "units");
    let desc = str_at(data, "desc");
    let location = str_at(data, "location");
    let feels = str_at(data, "feels");
    let humidity = str_at(data, "humidity");
    let wind = str_at(data, "wind");
    let days: Vec<(String, String, String)> = data
        .get("days")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|d| (str_at(d, "day"), str_at(d, "max"), str_at(d, "min")))
                .collect()
        })
        .unwrap_or_default();

    view! {
        <div class="weather">
            <div class="weather-now">
                <span class="weather-temp">
                    {temp}
                    <span class="weather-unit">"°" {units}</span>
                </span>
                <span class="weather-meta">
                    <span>{desc}</span>
                    <span class="muted">{location}</span>
                </span>
            </div>
            <div class="weather-facts muted">
                <span>"Feels " {feels} "°"</span>
                <span>"Humidity " {humidity} "%"</span>
                <span>"Wind " {wind} " km/h"</span>
            </div>
            {(!days.is_empty())
                .then(|| {
                    view! {
                        <ul class="forecast">
                            {days
                                .iter()
                                .map(|(d, max, min)| {
                                    let d = d.clone();
                                    let (max, min) = (max.clone(), min.clone());
                                    view! {
                                        <li>
                                            <span>{d}</span>
                                            <span class="mono muted">{max} "° / " {min} "°"</span>
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                })}
        </div>
    }
    .into_any()
}

fn overview_view(data: &Value) -> AnyView {
    let num = |k: &str| {
        data.get(k)
            .map(|v| match v {
                Value::Null => "—".to_string(),
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.clone(),
                _ => "—".to_string(),
            })
            .unwrap_or_else(|| "—".to_string())
    };
    let cells = vec![
        (num("online"), "Online"),
        (num("offline"), "Offline"),
        (num("avg_cpu"), "Avg CPU"),
        (num("containers"), "Containers"),
    ];
    view! {
        <div class="stat-row">
            {cells
                .into_iter()
                .map(|(v, label)| {
                    view! {
                        <div class="stat">
                            <div class="stat-value">{v}</div>
                            <div class="stat-label">{label}</div>
                        </div>
                    }
                })
                .collect_view()}
        </div>
    }
    .into_any()
}

fn containers_view(data: &Value) -> AnyView {
    let items: Vec<(String, String, String)> = data
        .get("items")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|c| (str_at(c, "name"), str_at(c, "status"), str_at(c, "device")))
                .collect()
        })
        .unwrap_or_default();

    if items.is_empty() {
        return view! { <p class="muted">"No containers to show"</p> }.into_any();
    }

    view! {
        <ul class="rows">
            {items
                .into_iter()
                .map(|(name, status, device)| {
                    let running = status == "running";
                    view! {
                        <li class="row">
                            <span class="dot" class:online=running></span>
                            <span class="mono row-name">{name}</span>
                            <span class="muted row-meta">{device}</span>
                        </li>
                    }
                })
                .collect_view()}
        </ul>
    }
    .into_any()
}

fn links_view(data: &Value) -> AnyView {
    let items: Vec<(String, String)> = data
        .get("items")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|l| (str_at(l, "name"), str_at(l, "url")))
                .collect()
        })
        .unwrap_or_default();

    view! {
        <ul class="rows">
            {items
                .into_iter()
                .map(|(name, url)| {
                    view! {
                        <li class="row">
                            <a href=url target="_blank" rel="noopener noreferrer">{name}</a>
                        </li>
                    }
                })
                .collect_view()}
        </ul>
    }
    .into_any()
}

fn rss_view(data: &Value) -> AnyView {
    let items: Vec<(String, String, String)> = data
        .get("items")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|i| (str_at(i, "title"), str_at(i, "link"), str_at(i, "feed")))
                .collect()
        })
        .unwrap_or_default();

    if items.is_empty() {
        return view! { <p class="muted">"No items — check the feed URLs"</p> }.into_any();
    }

    view! {
        <ul class="rows">
            {items
                .into_iter()
                .map(|(title, link, feed)| {
                    view! {
                        <li class="row row-stack">
                            <a href=link target="_blank" rel="noopener noreferrer">{title}</a>
                            <span class="muted tiny">{feed}</span>
                        </li>
                    }
                })
                .collect_view()}
        </ul>
    }
    .into_any()
}

/// The widget editor. Fields are generated from the registry, so a new widget
/// type needs no UI work.
#[component]
pub fn WidgetForm(
    fields: &'static [Field],
    initial: Value,
    on_save: Callback<Value>,
) -> impl IntoView {
    let values = RwSignal::new(initial);

    view! {
        <form
            class="form"
            on:submit=move |ev| {
                ev.prevent_default();
                on_save.run(values.get());
            }
        >
            {fields
                .iter()
                .map(|f| field_view(f, values))
                .collect_view()}
            <div class="form-actions">
                <button class="btn" type="submit">"Save"</button>
            </div>
        </form>
    }
}

fn field_view(field: &'static Field, values: RwSignal<Value>) -> AnyView {
    let key = field.key;
    let label = field.label;
    let id = format!("wf-{key}");

    let current = move || {
        values
            .get()
            .get(key)
            .cloned()
            .unwrap_or_else(|| field.default.clone())
    };

    let input = match field.kind {
        FieldKind::Checkbox => {
            let id2 = id.clone();
            view! {
                <input
                    type="checkbox"
                    id=id2
                    prop:checked=move || current().as_bool().unwrap_or(false)
                    on:change=move |ev| {
                        let checked = event_target_checked(&ev);
                        put(values, key, Value::Bool(checked));
                    }
                />
            }
            .into_any()
        }
        FieldKind::Select => {
            let id2 = id.clone();
            let options: Vec<(String, String)> = field
                .options
                .iter()
                .map(|(v, l)| (v.to_string(), l.to_string()))
                .collect();
            view! {
                <select
                    id=id2
                    on:change=move |ev| {
                        let val = event_target_value(&ev);
                        put(values, key, Value::String(val));
                    }
                >
                    {options
                        .into_iter()
                        .map(|(v, l)| {
                            let selected = v == current().as_str().unwrap_or("");
                            view! {
                                <option value=v.clone() selected=selected>
                                    {l}
                                </option>
                            }
                        })
                        .collect_view()}
                </select>
            }
            .into_any()
        }
        FieldKind::Number => {
            let id2 = id.clone();
            let min = field.min.unwrap_or(0);
            let max = field.max.unwrap_or(100);
            view! {
                <input
                    type="number"
                    id=id2
                    min=min
                    max=max
                    prop:value=move || current().as_i64().unwrap_or(0).to_string()
                    on:input=move |ev| {
                        let val = event_target_value(&ev);
                        let n = val.trim().parse::<i64>().unwrap_or(min);
                        put(values, key, Value::from(n.clamp(min, max)));
                    }
                />
            }
            .into_any()
        }
        FieldKind::Textarea => {
            let id2 = id.clone();
            view! {
                <textarea
                    id=id2
                    rows="4"
                    prop:value=move || current().as_str().unwrap_or("").to_string()
                    on:input=move |ev| {
                        let val = event_target_value(&ev);
                        put(values, key, Value::String(val));
                    }
                ></textarea>
            }
            .into_any()
        }
        FieldKind::Text => {
            let id2 = id.clone();
            let placeholder = field.hint.unwrap_or("").to_string();
            view! {
                <input
                    type="text"
                    id=id2
                    placeholder=placeholder
                    prop:value=move || current().as_str().unwrap_or("").to_string()
                    on:input=move |ev| {
                        let val = event_target_value(&ev);
                        put(values, key, Value::String(val));
                    }
                />
            }
            .into_any()
        }
    };

    view! {
        <div class="field">
            <label for=id.clone()>{label}</label>
            {input}
        </div>
    }
    .into_any()
}
