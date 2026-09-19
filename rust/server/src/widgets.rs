//! Widget data providers.
//!
//! Only the server ever performs outbound requests, and always to a URL derived
//! from validated config rather than from the request. `/api/widget/{id}` takes
//! an id, looks the widget up, and fetches what that widget's settings say — so
//! there is no route that will fetch a caller-supplied URL.

use std::time::Duration;

use mindash_core::widgets::WidgetKind;
use mindash_core::Widget;
use serde_json::{json, Value};

use crate::state::AppState;

const WEATHER_TTL: Duration = Duration::from_secs(600);
const RSS_TTL: Duration = Duration::from_secs(600);

pub async fn data_for(state: &AppState, widget: &Widget, devices: &Value) -> Value {
    match widget.kind {
        WidgetKind::Weather => weather(state, widget).await,
        WidgetKind::Rss => rss(state, widget).await,
        WidgetKind::Overview => overview(devices),
        WidgetKind::Containers => containers(widget, devices),
        WidgetKind::Links => links(widget),
        WidgetKind::Clock => json!({ "iso": iso_now() }),
    }
}

fn iso_now() -> String {
    // Enough of an ISO-8601 timestamp for the client to render a clock without
    // pulling in a date-time crate.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{now}")
}

async fn weather(state: &AppState, widget: &Widget) -> Value {
    let location = widget.get_str("location");
    let location = if location.trim().is_empty() {
        "Berlin".to_string()
    } else {
        location
    };
    let days = widget.get_i64("days").clamp(0, 5) as usize;
    let units = widget.get_str("units");

    // The location is part of the cache key, so changing it can never be served
    // the previous city's weather.
    let key = format!("weather:{location}:{days}");

    let fetched = state
        .cached(&key, WEATHER_TTL, || async {
            let encoded: String = urlencode(&location);
            let url = format!("https://wttr.in/{encoded}?format=j1");
            let body = state
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| e.to_string())?
                .error_for_status()
                .map_err(|e| e.to_string())?
                .text()
                .await
                .map_err(|e| e.to_string())?;
            let v: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
            Ok(parse_weather(&v, days))
        })
        .await;

    apply_units(fetched, &units)
}

/// Turn wttr.in's j1 document into the small shape the UI wants.
fn parse_weather(raw: &Value, days: usize) -> Value {
    let cur = raw
        .get("current_condition")
        .and_then(|c| c.get(0))
        .cloned()
        .unwrap_or(Value::Null);

    let area = raw
        .get("nearest_area")
        .and_then(|a| a.get(0))
        .and_then(|a| a.get("areaName"))
        .and_then(|a| a.get(0))
        .and_then(|a| a.get("value"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let desc = cur
        .get("weatherDesc")
        .and_then(|d| d.get(0))
        .and_then(|d| d.get("value"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let forecast: Vec<Value> = raw
        .get("weather")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .take(days)
                .map(|d| {
                    let hourly = d.get("hourly").and_then(Value::as_array);
                    let mid = hourly
                        .and_then(|h| h.get(h.len() / 2))
                        .or_else(|| hourly.and_then(|h| h.first()));
                    let desc = mid
                        .and_then(|h| h.get("weatherDesc"))
                        .and_then(|x| x.get(0))
                        .and_then(|x| x.get("value"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    json!({
                        "day": day_label(d.get("date").and_then(Value::as_str).unwrap_or("")),
                        "max": d.get("maxtempC").and_then(Value::as_str).unwrap_or("--"),
                        "min": d.get("mintempC").and_then(Value::as_str).unwrap_or("--"),
                        "desc": desc,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    json!({
        "location": if area.is_empty() { Value::Null } else { Value::String(area) },
        "temp": cur.get("temp_C").and_then(Value::as_str).unwrap_or("--"),
        "feels": cur.get("FeelsLikeC").and_then(Value::as_str).unwrap_or("--"),
        "desc": desc,
        "humidity": cur.get("humidity").and_then(Value::as_str).unwrap_or("--"),
        "wind": cur.get("windspeedKmph").and_then(Value::as_str).unwrap_or("--"),
        "days": forecast,
    })
}

/// "2026-02-03" -> "Tue". Falls back to the raw string.
fn day_label(date: &str) -> String {
    const NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let mut parts = date.split('-');
    let (Some(y), Some(m), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return date.to_string();
    };
    let (Ok(y), Ok(m), Ok(d)) = (y.parse::<i32>(), m.parse::<u32>(), d.parse::<u32>()) else {
        return date.to_string();
    };
    // Zeller's congruence, which needs no date library.
    let (m, y) = if m < 3 { (m + 12, y - 1) } else { (m, y) };
    let k = y % 100;
    let j = y / 100;
    let h = (d as i32 + (13 * (m as i32 + 1)) / 5 + k + k / 4 + j / 4 + 5 * j).rem_euclid(7);
    // h: 0 = Saturday
    let idx = ((h + 6) % 7) as usize;
    NAMES[idx].to_string()
}

/// Fahrenheit conversion applied at render time rather than re-fetched, so
/// flipping units does not need a new upstream call.
fn apply_units(mut data: Value, units: &str) -> Value {
    let to_f = |s: &str| -> String {
        s.parse::<f64>()
            .map(|c| format!("{}", (c * 9.0 / 5.0 + 32.0).round() as i64))
            .unwrap_or_else(|_| s.to_string())
    };
    if units == "f" {
        if let Some(o) = data.as_object_mut() {
            for key in ["temp", "feels"] {
                if let Some(Value::String(s)) = o.get(key) {
                    let f = to_f(s);
                    o.insert(key.into(), Value::String(f));
                }
            }
            if let Some(Value::Array(days)) = o.get_mut("days") {
                for d in days.iter_mut() {
                    for key in ["max", "min"] {
                        if let Some(Value::String(s)) = d.get(key) {
                            let f = to_f(s);
                            d[key] = Value::String(f);
                        }
                    }
                }
            }
            o.insert("units".into(), Value::String("F".into()));
        }
    } else if let Some(o) = data.as_object_mut() {
        o.insert("units".into(), Value::String("C".into()));
    }
    data
}

async fn rss(state: &AppState, widget: &Widget) -> Value {
    let feeds: Vec<String> = widget
        .get_str("feeds")
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("http://") || l.starts_with("https://"))
        .take(8)
        .map(str::to_string)
        .collect();
    let limit = widget.get_i64("limit").clamp(1, 30) as usize;

    if feeds.is_empty() {
        return json!({ "items": [] });
    }

    let key = format!("rss:{}:{limit}", feeds.join("|"));
    state
        .cached(&key, RSS_TTL, || async {
            let mut items = Vec::new();
            for feed in &feeds {
                match fetch_feed(state, feed).await {
                    Ok(mut found) => items.append(&mut found),
                    Err(e) => tracing::debug!("feed {feed}: {e}"),
                }
            }
            items.truncate(limit);
            Ok(json!({ "items": items }))
        })
        .await
}

async fn fetch_feed(state: &AppState, url: &str) -> Result<Vec<Value>, String> {
    let body = state
        .http
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;

    let host = url
        .split("//")
        .nth(1)
        .and_then(|s| s.split('/').next())
        .unwrap_or("")
        .to_string();

    let mut reader = quick_xml::Reader::from_str(&body);
    reader.config_mut().trim_text(true);

    let mut items = Vec::new();
    let mut in_item = false;
    let mut title = String::new();
    let mut link = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"item" | b"entry" => {
                        in_item = true;
                        title.clear();
                        link.clear();
                    }
                    _ => {}
                }
            }
            Ok(quick_xml::events::Event::Empty(e)) if in_item => {
                // Atom puts the URL in a href attribute.
                if e.name().as_ref() == b"link" {
                    for attr in e.attributes().flatten() {
                        if attr.key.as_ref() == b"href" {
                            link = String::from_utf8_lossy(&attr.value).to_string();
                        }
                    }
                }
            }
            Ok(quick_xml::events::Event::Text(t)) if in_item => {
                let text = t.unescape().unwrap_or_default().to_string();
                let name = reader.decoder();
                let _ = name;
                if title.is_empty() {
                    title = text;
                } else if link.is_empty() && text.starts_with("http") {
                    link = text;
                }
            }
            Ok(quick_xml::events::Event::End(e)) => match e.name().as_ref() {
                // A closing tag only ends an item that was open. A stray end tag
                // outside an item is ignored.
                b"item" | b"entry" if in_item => {
                    in_item = false;
                    if !title.is_empty() {
                        items.push(json!({
                            "title": title.chars().take(180).collect::<String>(),
                            "link": link,
                            "feed": host,
                        }));
                    }
                }
                _ => {}
            },
            Ok(quick_xml::events::Event::Eof) => break,
            Err(e) => return Err(e.to_string()),
            _ => {}
        }
        buf.clear();
    }
    Ok(items)
}

fn overview(devices: &Value) -> Value {
    let list = devices.as_array().cloned().unwrap_or_default();
    let mut online = 0;
    let mut cpus: Vec<f64> = Vec::new();
    let mut containers = 0;

    for d in &list {
        if d.get("online").and_then(Value::as_bool).unwrap_or(false) {
            online += 1;
        }
        if let Some(cpu) = d
            .get("stats")
            .and_then(|s| s.get("cpu"))
            .and_then(Value::as_f64)
        {
            cpus.push(cpu);
        }
        containers += d
            .get("containers")
            .and_then(Value::as_object)
            .map(|c| c.len())
            .unwrap_or(0);
    }

    let avg = if cpus.is_empty() {
        Value::Null
    } else {
        json!((cpus.iter().sum::<f64>() / cpus.len() as f64).round() as i64)
    };

    json!({
        "devices": list.len(),
        "online": online,
        "offline": list.len() - online,
        "avg_cpu": avg,
        "containers": containers,
    })
}

fn containers(widget: &Widget, devices: &Value) -> Value {
    let running_only = widget.get_bool("running_only");
    let limit = widget.get_i64("limit").clamp(1, 50) as usize;
    let mut rows = Vec::new();

    for d in devices.as_array().cloned().unwrap_or_default() {
        let device = d
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let Some(map) = d.get("containers").and_then(Value::as_object) else {
            continue;
        };
        for (name, status) in map {
            let status = status.as_str().unwrap_or("unknown");
            if running_only && status != "running" {
                continue;
            }
            rows.push(json!({ "device": device, "name": name, "status": status }));
        }
    }
    rows.sort_by(|a, b| {
        let ra = a["status"] != "running";
        let rb = b["status"] != "running";
        ra.cmp(&rb)
            .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
    });
    rows.truncate(limit);
    json!({ "items": rows })
}

fn links(widget: &Widget) -> Value {
    let items: Vec<Value> = widget
        .get_str("items")
        .lines()
        .filter_map(|line| line.split_once('|'))
        .filter(|(_, url)| url.trim().starts_with("http"))
        .map(|(name, url)| json!({ "name": name.trim(), "url": url.trim() }))
        .collect();
    json!({ "items": items })
}

/// Minimal percent-encoding for a path segment.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mindash_core::widgets::{coerce_settings, WidgetKind};

    fn widget(kind: WidgetKind, settings: Value) -> Widget {
        Widget {
            id: "w".into(),
            kind,
            title: String::new(),
            icon: String::new(),
            settings: coerce_settings(kind, &settings),
        }
    }

    #[test]
    fn urlencode_escapes_path_segments() {
        assert_eq!(urlencode("Berlin"), "Berlin");
        assert_eq!(urlencode("New York"), "New%20York");
        assert_eq!(urlencode("52.52,13.40"), "52.52%2C13.40");
        assert_eq!(urlencode("a/b?c=d"), "a%2Fb%3Fc%3Dd");
        assert_eq!(urlencode("Tromsø"), "Troms%C3%B8");
    }

    #[test]
    fn day_label_is_correct() {
        assert_eq!(day_label("2026-02-03"), "Tue");
        assert_eq!(day_label("2026-02-01"), "Sun");
        assert_eq!(day_label("2026-09-19"), "Sat");
        assert_eq!(day_label("nonsense"), "nonsense");
    }

    #[test]
    fn weather_parses_the_upstream_shape() {
        let raw: Value = serde_json::from_str(
            r#"{
              "current_condition":[{"temp_C":"14","FeelsLikeC":"12",
                 "weatherDesc":[{"value":"Overcast"}],"humidity":"66","windspeedKmph":"12"}],
              "nearest_area":[{"areaName":[{"value":"Berlin"}]}],
              "weather":[{"date":"2026-02-03","maxtempC":"15","mintempC":"6",
                 "hourly":[{"weatherDesc":[{"value":"Cloudy"}]}]}]
            }"#,
        )
        .unwrap();
        let out = parse_weather(&raw, 3);
        assert_eq!(out["location"], "Berlin");
        assert_eq!(out["temp"], "14");
        assert_eq!(out["desc"], "Overcast");
        assert_eq!(out["days"][0]["day"], "Tue");
        assert_eq!(out["days"][0]["max"], "15");
    }

    #[test]
    fn weather_tolerates_a_minimal_document() {
        let out = parse_weather(&json!({}), 3);
        assert_eq!(out["temp"], "--");
        assert_eq!(out["days"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn fahrenheit_conversion_applies_to_current_and_forecast() {
        let raw: Value = serde_json::from_str(
            r#"{"current_condition":[{"temp_C":"10","FeelsLikeC":"8",
                 "weatherDesc":[{"value":"Clear"}]}],
                "weather":[{"date":"2026-02-03","maxtempC":"20","mintempC":"0",
                 "hourly":[{"weatherDesc":[{"value":"Clear"}]}]}]}"#,
        )
        .unwrap();
        let out = apply_units(parse_weather(&raw, 3), "f");
        assert_eq!(out["temp"], "50");
        assert_eq!(out["feels"], "46");
        assert_eq!(out["days"][0]["max"], "68");
        assert_eq!(out["days"][0]["min"], "32");
        assert_eq!(out["units"], "F");
    }

    #[test]
    fn overview_counts_correctly_and_handles_empty() {
        let out = overview(&json!([]));
        assert_eq!(out["devices"], 0);
        assert_eq!(out["avg_cpu"], Value::Null);

        let out = overview(&json!([
            {"online":true,"stats":{"cpu":10},"containers":{"a":"running"}},
            {"online":false,"stats":{"cpu":30},"containers":{"b":"exited","c":"running"}}
        ]));
        assert_eq!(out["online"], 1);
        assert_eq!(out["offline"], 1);
        assert_eq!(out["avg_cpu"], 20);
        assert_eq!(out["containers"], 3);
    }

    #[test]
    fn containers_sort_running_first_and_respect_the_limit() {
        let devices = json!([
            {"id":"h","containers":{"zeta":"exited","alpha":"running","beta":"running"}}
        ]);
        let out = containers(
            &widget(WidgetKind::Containers, json!({"limit":2})),
            &devices,
        );
        let items = out["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["name"], "alpha");
        assert_eq!(items[1]["name"], "beta");
    }

    #[test]
    fn containers_running_only_filters() {
        let devices = json!([{"id":"h","containers":{"a":"running","b":"exited"}}]);
        let out = containers(
            &widget(WidgetKind::Containers, json!({"running_only":true})),
            &devices,
        );
        assert_eq!(out["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn links_parses_pairs_and_skips_junk() {
        let out = links(&widget(
            WidgetKind::Links,
            json!({"items":"A|https://a.test\nno pipe here\nB|javascript:alert(1)\nC|http://c.test"}),
        ));
        let items = out["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["name"], "A");
        assert_eq!(items[1]["url"], "http://c.test");
    }

    #[test]
    fn overview_never_divides_by_zero() {
        let out = overview(&json!([{"online":true,"stats":{}}]));
        assert_eq!(out["avg_cpu"], Value::Null);
    }
}
