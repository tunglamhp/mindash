//! MinDash web dashboard — Leptos, client side rendered.

use leptos::prelude::*;

mod api;
mod panels;
mod theme;
mod widgets;

use panels::{ContainersPanel, DevicesPanel};
use widgets::{WidgetCard, WidgetPicker};

/// Which control panel is showing. The dashboard is one screen with a tab strip
/// rather than separate routes: everything shares the same config, and a full
/// navigation would throw away the live SSE connection each time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Dashboard,
    Devices,
    Containers,
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
pub fn App() -> impl IntoView {
    // `None` while the first fetch is in flight, so the shell can show a
    // skeleton instead of a flash of empty sections.
    let config = RwSignal::new(None::<mindash_core::Config>);
    let error = RwSignal::new(None::<String>);
    let edit_mode = RwSignal::new(false);
    let picker_open = RwSignal::new(false);
    let tab = RwSignal::new(Tab::Dashboard);

    // Initial load, then live updates over SSE. The Python dashboard polled
    // every 5 s from every tab; here an idle dashboard costs no requests.
    leptos::task::spawn_local(async move {
        match api::get_config().await {
            Ok(cfg) => config.set(Some(cfg)),
            Err(e) => error.set(Some(e)),
        }
        api::subscribe(move |cfg| {
            config.set(Some(cfg));
        });
    });

    // Theme is applied by writing custom properties onto :root, so switching a
    // preset or dragging a slider repaints without rebuilding the tree.
    Effect::new(move |_| {
        let t = config.get().map(|c| c.settings.theme).unwrap_or_default();
        theme::apply(&t);
    });

    view! {
        <div class="shell">
            <Header edit_mode=edit_mode picker_open=picker_open tab=tab />
            <main class="content">
                <Show when=move || error.get().is_some()>
                    <div class="notice notice-error">
                        {move || error.get().unwrap_or_default()}
                    </div>
                </Show>
                <Show
                    when=move || config.get().is_some()
                    fallback=|| view! { <Skeleton /> }
                >
                    {move || {
                        config
                            .get()
                            .map(|cfg| {
                                match tab.get() {
                                    Tab::Dashboard => {
                                        view! {
                                            <Dashboard
                                                config=cfg
                                                config_signal=config
                                                edit_mode=edit_mode
                                            />
                                        }
                                            .into_any()
                                    }
                                    Tab::Devices => view! { <DevicesPanel config=config /> }.into_any(),
                                    Tab::Containers => {
                                        view! { <ContainersPanel config=config /> }.into_any()
                                    }
                                }
                            })
                    }}
                </Show>
            </main>
            <WidgetPicker open=picker_open config=config error=error />
            <Footer />
        </div>
    }
}

/// The MinDash logo: the dashboard glyph beside the wordmark.
///
/// The glyph is the same three panels as `static/icon.svg`, drawn inline and
/// filled from theme tokens so it follows the active theme instead of carrying
/// its own palette. `role="img"` with a title means a screen reader announces
/// "MinDash" once, from the label, rather than spelling out the shapes.
#[component]
fn BrandMark() -> impl IntoView {
    view! {
        <span class="brand-lockup" role="img" aria-label="MinDash">
            <svg class="brand-glyph" viewBox="0 0 64 64" aria-hidden="true" focusable="false">
                <rect class="glyph-main" x="4" y="10" width="18" height="44" rx="6"/>
                <rect class="glyph-sub" x="27" y="10" width="33" height="20" rx="6"/>
                <rect class="glyph-sub glyph-sub-2" x="27" y="35" width="33" height="19" rx="6"/>
            </svg>
            <span class="brand-word" aria-hidden="true">"MinDash"</span>
        </span>
    }
}

#[component]
fn Header(
    edit_mode: RwSignal<bool>,
    picker_open: RwSignal<bool>,
    tab: RwSignal<Tab>,
) -> impl IntoView {
    view! {
        <header class="header">
            <div class="brand">
                <BrandMark />
            </div>

            <nav class="tabs" role="tablist" aria-label="Sections">
                {[
                    (Tab::Dashboard, "Dashboard"),
                    (Tab::Devices, "Devices"),
                    (Tab::Containers, "Containers"),
                ]
                    .into_iter()
                    .map(|(value, label)| {
                        view! {
                            <button
                                class="tab"
                                type="button"
                                role="tab"
                                aria-selected=move || (tab.get() == value).to_string()
                                class:active=move || tab.get() == value
                                on:click=move |_| tab.set(value)
                            >
                                {label}
                            </button>
                        }
                    })
                    .collect_view()}
            </nav>

            <div class="header-actions">
                <Show when=move || edit_mode.get() && tab.get() == Tab::Dashboard>
                    <button
                        class="btn"
                        type="button"
                        on:click=move |_| picker_open.set(true)
                    >
                        "Add widget"
                    </button>
                </Show>
                <Show when=move || tab.get() == Tab::Dashboard>
                    <button
                        class="btn btn-ghost"
                        type="button"
                        aria-pressed=move || edit_mode.get().to_string()
                        aria-label="Toggle edit mode"
                        on:click=move |_| edit_mode.update(|m| *m = !*m)
                    >
                        {move || if edit_mode.get() { "Done" } else { "Edit" }}
                    </button>
                </Show>
            </div>
        </header>
        <Show when=move || edit_mode.get() && tab.get() == Tab::Dashboard>
            <div class="edit-banner" role="status">
                "Edit mode — configure or remove widgets, add new ones from the header"
            </div>
        </Show>
    }
}

#[component]
fn Dashboard(
    config: mindash_core::Config,
    // The signal, not just the snapshot: the widget editor writes a fresh config
    // back after a save, and the SSE stream refreshes it on any server change.
    config_signal: RwSignal<Option<mindash_core::Config>>,
    edit_mode: RwSignal<bool>,
) -> impl IntoView {
    // Each section is its own component so it can decide whether to render at
    // all. Show is not used here because it moves its children and the
    // collections are needed for both the emptiness test and the markup.
    let widgets_block = view! {
        <WidgetSection widgets=config.widgets.clone() config=config_signal edit_mode=edit_mode />
    };
    let devices_block = view! { <DeviceSection devices=config.devices.clone() /> };
    let links_block = view! { <LinkSection links=config.links.clone() /> };
    let actions_block = view! { <ActionSection actions=config.quick_actions.clone() /> };

    view! {
        {widgets_block}
        {devices_block}
        {links_block}
        {actions_block}
        <Show when=move || edit_mode.get()>
            <section class="section">
                <SectionHeader title="Theme" count=0 />
                <ThemePanel />
            </section>
        </Show>
    }
}

#[component]
fn WidgetSection(
    widgets: Vec<mindash_core::Widget>,
    config: RwSignal<Option<mindash_core::Config>>,
    edit_mode: RwSignal<bool>,
) -> impl IntoView {
    if widgets.is_empty() {
        return None;
    }
    let count = widgets.len();
    let cards = widgets
        .into_iter()
        .map(|w| view! { <WidgetCard widget=w config=config edit_mode=edit_mode /> })
        .collect_view();
    Some(view! {
        <section class="section">
            <SectionHeader title="Widgets" count=count />
            <div class="widget-grid">{cards}</div>
        </section>
    })
}

#[component]
fn DeviceSection(devices: Vec<mindash_core::Device>) -> impl IntoView {
    if devices.is_empty() {
        return None;
    }
    let count = devices.len();
    let cards = devices
        .into_iter()
        .map(|d| {
            let name = if d.name.is_empty() {
                d.id.clone()
            } else {
                d.name.clone()
            };
            view! { <DeviceCard name=name host=d.is_host /> }
        })
        .collect_view();
    Some(view! {
        <section class="section">
            <SectionHeader title="Devices" count=count />
            <div class="card-grid">{cards}</div>
        </section>
    })
}

#[component]
fn LinkSection(links: Vec<mindash_core::Link>) -> impl IntoView {
    if links.is_empty() {
        return None;
    }
    let count = links.len();
    let cards = links
        .into_iter()
        .map(|l| view! { <LinkCard name=l.name url=l.url note=l.note /> })
        .collect_view();
    Some(view! {
        <section class="section">
            <SectionHeader title="Links" count=count />
            <div class="card-grid">{cards}</div>
        </section>
    })
}

#[component]
fn ActionSection(actions: Vec<mindash_core::QuickAction>) -> impl IntoView {
    if actions.is_empty() {
        return None;
    }
    let count = actions.len();
    let cards = actions
        .into_iter()
        .map(|a| view! { <SimpleCard name=a.name /> })
        .collect_view();
    Some(view! {
        <section class="section">
            <SectionHeader title="Scripts" count=count />
            <div class="card-grid">{cards}</div>
        </section>
    })
}

#[component]
fn SectionHeader(title: &'static str, count: usize) -> impl IntoView {
    view! {
        <div class="section-header">
            <h2 class="section-title">{title}</h2>
            <span class="section-count">{count}</span>
        </div>
    }
}

/// Theme controls. Every control writes a CSS custom property, so a change
/// repaints immediately and nothing in the tree needs re-rendering.
#[component]
fn ThemePanel() -> impl IntoView {
    // A local mirror so the inputs are responsive before the round trip lands.
    let theme = RwSignal::new(mindash_core::ThemeColors::default());

    view! {
        <div class="panel">
            <div class="panel-grid">
                <ColorField label="Background" key="bg" theme=theme />
                <ColorField label="Cards" key="cards" theme=theme />
                <ColorField label="Border" key="border" theme=theme />
                <ColorField label="Text" key="text" theme=theme />
                <ColorField label="Accent" key="accent" theme=theme />
                <ColorField label="Danger" key="danger" theme=theme />
                <ColorField label="Warning" key="warning" theme=theme />
            </div>
            <div class="panel-row">
                <RangeField label="Radius" key="radius" min=0 max=25 theme=theme />
                <RangeField label="Max width" key="width" min=30 max=100 theme=theme />
            </div>
        </div>
    }
}

#[component]
fn ColorField(
    label: &'static str,
    key: &'static str,
    theme: RwSignal<mindash_core::ThemeColors>,
) -> impl IntoView {
    view! {
        <label class="field">
            <span>{label}</span>
            <input
                type="color"
                prop:value=move || match key {
                    "bg" => theme.get().bg,
                    "cards" => theme.get().cards,
                    "border" => theme.get().border,
                    "text" => theme.get().text,
                    "accent" => theme.get().accent,
                    "danger" => theme.get().danger,
                    _ => theme.get().warning,
                }
                on:input=move |ev| {
                    let value = event_target_value(&ev);
                    theme.update(|t| match key {
                        "bg" => t.bg = value,
                        "cards" => t.cards = value,
                        "border" => t.border = value,
                        "text" => t.text = value,
                        "accent" => t.accent = value,
                        "danger" => t.danger = value,
                        _ => t.warning = value,
                    });
                }
            />
        </label>
    }
}

#[component]
fn RangeField(
    label: &'static str,
    key: &'static str,
    min: i32,
    max: i32,
    theme: RwSignal<mindash_core::ThemeColors>,
) -> impl IntoView {
    view! {
        <label class="field">
            <span>{label}</span>
            <input
                type="range"
                min=min
                max=max
                prop:value=move || match key {
                    "radius" => theme.get().border_radius as i32,
                    _ => theme.get().max_width_desktop as i32,
                }
                on:input=move |ev| {
                    let n = event_target_value(&ev).parse::<u8>().unwrap_or(0);
                    theme.update(|t| match key {
                        "radius" => t.border_radius = n,
                        _ => t.max_width_desktop = n,
                    });
                }
            />
        </label>
    }
}

#[component]
fn DeviceCard(name: String, host: bool) -> impl IntoView {
    view! {
        <article class="card">
            <div class="card-head">
                <span class="dot" class:online=true></span>
                <span class="card-title">{name}</span>
                <Show when=move || host>
                    <span class="pill">"host"</span>
                </Show>
            </div>
        </article>
    }
}

#[component]
fn LinkCard(name: String, url: String, note: String) -> impl IntoView {
    let has_note = !note.is_empty();
    view! {
        <a class="card card-link" href=url target="_blank" rel="noopener noreferrer">
            <span class="card-title">{name}</span>
            {has_note.then(|| view! { <span class="card-note">{note}</span> })}
        </a>
    }
}

#[component]
fn SimpleCard(name: String) -> impl IntoView {
    view! {
        <article class="card">
            <span class="card-title">{name}</span>
        </article>
    }
}

/// Placeholder shown while the first config fetch is in flight.
#[component]
fn Skeleton() -> impl IntoView {
    view! {
        <div class="section">
            <div class="skel skel-title"></div>
            <div class="card-grid">
                {(0..4).map(|_| view! { <div class="skel skel-card"></div> }).collect_view()}
            </div>
        </div>
    }
}

#[component]
fn Footer() -> impl IntoView {
    view! {
        <footer class="footer">
            <span>"MinDash " {env!("CARGO_PKG_VERSION")}</span>
        </footer>
    }
}
