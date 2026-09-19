//! Widget registry.
//!
//! In the Python version this was a dict of dicts that the server validated
//! against and the browser built its edit form from. That worked, but the two
//! sides only agreed by convention. Here the registry is a single table in the
//! shared crate, the settings coercion is one exhaustive function, and adding a
//! widget type is a compile error until every match arm handles it.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The kinds of widget a user can add. Serialised in kebab-case so the wire
/// format stays readable and stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WidgetKind {
    Weather,
    Clock,
    Rss,
    Overview,
    Containers,
    Links,
}

impl WidgetKind {
    pub const ALL: [WidgetKind; 6] = [
        WidgetKind::Weather,
        WidgetKind::Clock,
        WidgetKind::Rss,
        WidgetKind::Overview,
        WidgetKind::Containers,
        WidgetKind::Links,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Weather => "weather",
            Self::Clock => "clock",
            Self::Rss => "rss",
            Self::Overview => "overview",
            Self::Containers => "containers",
            Self::Links => "links",
        }
    }

    pub fn spec(self) -> &'static WidgetType {
        match self {
            Self::Weather => &WEATHER,
            Self::Clock => &CLOCK,
            Self::Rss => &RSS,
            Self::Overview => &OVERVIEW,
            Self::Containers => &CONTAINERS,
            Self::Links => &LINKS,
        }
    }
}

/// A field in a widget's edit form, plus how to coerce it.
#[derive(Debug, Clone, Serialize)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    pub default: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<(&'static str, &'static str)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    Text,
    Number,
    Checkbox,
    Select,
    Textarea,
}

#[derive(Debug, Clone, Serialize)]
pub struct WidgetType {
    pub kind: WidgetKind,
    pub label: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub fields: &'static [Field],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Widget {
    pub id: String,
    pub kind: WidgetKind,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub settings: Map<String, Value>,
}

impl Widget {
    pub fn new(id: impl Into<String>, kind: WidgetKind) -> Self {
        let spec = kind.spec();
        Self {
            id: id.into(),
            kind,
            title: spec.title.to_string(),
            icon: spec.icon.to_string(),
            settings: defaults_for(kind),
        }
    }

    /// Title falls back to the type's title, icon to the type's icon.
    pub fn resolved_title(&self) -> &str {
        if self.title.is_empty() {
            self.kind.spec().title
        } else {
            &self.title
        }
    }

    pub fn resolved_icon(&self) -> &str {
        if self.icon.is_empty() {
            self.kind.spec().icon
        } else {
            &self.icon
        }
    }

    pub fn get_str(&self, key: &str) -> String {
        self.settings
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    pub fn get_i64(&self, key: &str) -> i64 {
        self.settings.get(key).and_then(Value::as_i64).unwrap_or(0)
    }

    pub fn get_bool(&self, key: &str) -> bool {
        self.settings
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// `serde_json::Value` allocates, so these tables cannot be `const`. They are
/// `LazyLock` statics, built once and shared by both the server and the browser.
macro_rules! field {
    ($key:expr, $label:expr, $kind:expr, $default:expr) => {
        Field {
            key: $key,
            label: $label,
            kind: $kind,
            default: $default,
            hint: None,
            min: None,
            max: None,
            options: Vec::new(),
        }
    };
}

/// Leak a boxed slice once to obtain the `&'static [Field]` the registry needs.
/// This is a fixed, tiny amount of memory allocated exactly once per process.
macro_rules! leak {
    ($($f:expr),* $(,)?) => {
        Box::leak(Box::new([$($f),*])) as &'static [Field]
    };
}

use std::sync::LazyLock;

pub static WEATHER: LazyLock<WidgetType> = LazyLock::new(|| WidgetType {
    kind: WidgetKind::Weather,
    label: "Weather",
    title: "Weather",
    icon: "cloud-sun",
    fields: leak![
        Field {
            hint: Some("City name, or lat,lon"),
            ..field!(
                "location",
                "Location",
                FieldKind::Text,
                Value::String("Berlin".into())
            )
        },
        Field {
            options: vec![("c", "Celsius"), ("f", "Fahrenheit")],
            ..field!(
                "units",
                "Units",
                FieldKind::Select,
                Value::String("c".into())
            )
        },
        Field {
            min: Some(0),
            max: Some(5),
            ..field!("days", "Forecast days", FieldKind::Number, Value::from(3))
        },
    ],
});

pub static CLOCK: LazyLock<WidgetType> = LazyLock::new(|| WidgetType {
    kind: WidgetKind::Clock,
    label: "Clock",
    title: "Clock",
    icon: "clock",
    fields: leak![
        field!(
            "label",
            "Label",
            FieldKind::Text,
            Value::String(String::new())
        ),
        Field {
            options: vec![("24", "24-hour"), ("12", "12-hour")],
            ..field!(
                "format",
                "Format",
                FieldKind::Select,
                Value::String("24".into())
            )
        },
        field!(
            "seconds",
            "Show seconds",
            FieldKind::Checkbox,
            Value::Bool(false)
        ),
        field!("date", "Show date", FieldKind::Checkbox, Value::Bool(true)),
    ],
});

pub static RSS: LazyLock<WidgetType> = LazyLock::new(|| WidgetType {
    kind: WidgetKind::Rss,
    label: "News (RSS/Atom)",
    title: "News",
    icon: "rss",
    fields: leak![
        Field {
            hint: Some("One URL per line"),
            ..field!(
                "feeds",
                "Feed URLs",
                FieldKind::Textarea,
                Value::String("https://news.ycombinator.com/rss".into())
            )
        },
        Field {
            min: Some(1),
            max: Some(30),
            ..field!("limit", "Items", FieldKind::Number, Value::from(8))
        },
        field!(
            "thumbs",
            "Show thumbnails",
            FieldKind::Checkbox,
            Value::Bool(true)
        ),
    ],
});

pub static OVERVIEW: LazyLock<WidgetType> = LazyLock::new(|| WidgetType {
    kind: WidgetKind::Overview,
    label: "System overview",
    title: "Overview",
    icon: "activity",
    fields: leak![
        field!(
            "show_cpu",
            "Show average CPU",
            FieldKind::Checkbox,
            Value::Bool(true)
        ),
        field!(
            "show_ram",
            "Show memory",
            FieldKind::Checkbox,
            Value::Bool(true)
        ),
        field!(
            "show_containers",
            "Show container counts",
            FieldKind::Checkbox,
            Value::Bool(true)
        ),
    ],
});

pub static CONTAINERS: LazyLock<WidgetType> = LazyLock::new(|| WidgetType {
    kind: WidgetKind::Containers,
    label: "Containers",
    title: "Containers",
    icon: "box",
    fields: leak![
        Field {
            min: Some(1),
            max: Some(50),
            ..field!("limit", "Max shown", FieldKind::Number, Value::from(10))
        },
        field!(
            "running_only",
            "Running only",
            FieldKind::Checkbox,
            Value::Bool(false)
        ),
    ],
});

pub static LINKS: LazyLock<WidgetType> = LazyLock::new(|| WidgetType {
    kind: WidgetKind::Links,
    label: "Links",
    title: "Links",
    icon: "link",
    fields: leak![Field {
        hint: Some("One per line, as Name|URL"),
        ..field!(
            "items",
            "Links",
            FieldKind::Textarea,
            Value::String("MinDash|https://example.com".into())
        )
    }],
});

pub fn all_types() -> [&'static WidgetType; 6] {
    [&WEATHER, &CLOCK, &RSS, &OVERVIEW, &CONTAINERS, &LINKS]
}

pub fn defaults_for(kind: WidgetKind) -> Map<String, Value> {
    kind.spec()
        .fields
        .iter()
        .map(|f| (f.key.to_string(), f.default.clone()))
        .collect()
}

/// Coerce arbitrary incoming settings against the registry.
///
/// Only declared keys survive, each is cast to its declared type, numbers are
/// clamped to their range, and an out-of-range select falls back to its default.
/// Unknown keys are dropped. Nothing here can fail: bad input becomes the
/// default rather than an error, which is why the server never has to trust the
/// browser for anything that reaches the renderer.
pub fn coerce_settings(kind: WidgetKind, incoming: &Value) -> Map<String, Value> {
    let obj = incoming.as_object();
    let mut out = Map::new();

    for f in kind.spec().fields {
        let raw = obj.and_then(|o| o.get(f.key));

        let value = match f.kind {
            FieldKind::Number => {
                let n = raw
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    })
                    .or_else(|| f.default.as_i64())
                    .unwrap_or(0);
                let lo = f.min.unwrap_or(i64::MIN);
                let hi = f.max.unwrap_or(i64::MAX);
                Value::from(n.clamp(lo, hi))
            }
            FieldKind::Checkbox => Value::Bool(
                raw.and_then(Value::as_bool)
                    .or_else(|| f.default.as_bool())
                    .unwrap_or(false),
            ),
            FieldKind::Select => {
                let s = raw.and_then(Value::as_str).unwrap_or_default();
                let allowed = f.options.iter().any(|(v, _)| *v == s);
                if allowed {
                    Value::String(s.to_string())
                } else {
                    f.default.clone()
                }
            }
            FieldKind::Text | FieldKind::Textarea => {
                let mut s = raw
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| f.default.as_str().map(str::to_string))
                    .unwrap_or_default();
                // Bound the growth of a stored value the same way the Python
                // side did, without splitting a UTF-8 boundary.
                const MAX: usize = 2000;
                if s.len() > MAX {
                    let mut cut = MAX;
                    while cut > 0 && !s.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    s.truncate(cut);
                }
                Value::String(s)
            }
        };
        out.insert(f.key.to_string(), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_kind_has_a_spec_with_fields() {
        for k in WidgetKind::ALL {
            let s = k.spec();
            assert!(!s.fields.is_empty(), "{:?} has no fields", k);
            assert!(!s.title.is_empty());
            assert!(!s.icon.is_empty());
        }
    }

    #[test]
    fn defaults_cover_every_field() {
        for k in WidgetKind::ALL {
            let d = defaults_for(k);
            assert_eq!(d.len(), k.spec().fields.len(), "{:?} defaults mismatch", k);
        }
    }

    #[test]
    fn unknown_keys_are_dropped() {
        let out = coerce_settings(
            WidgetKind::Weather,
            &json!({ "location": "Tokyo", "evil": "x", "extra": 1 }),
        );
        assert_eq!(out.get("location").unwrap(), "Tokyo");
        assert!(!out.contains_key("evil"));
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn numbers_are_clamped() {
        let out = coerce_settings(WidgetKind::Weather, &json!({ "days": 99 }));
        assert_eq!(out.get("days").unwrap(), 5);
        let out = coerce_settings(WidgetKind::Weather, &json!({ "days": -4 }));
        assert_eq!(out.get("days").unwrap(), 0);
    }

    #[test]
    fn numeric_strings_are_accepted() {
        // The browser sends a form value; it arrives as a string.
        let out = coerce_settings(WidgetKind::Rss, &json!({ "limit": "12" }));
        assert_eq!(out.get("limit").unwrap(), 12);
    }

    #[test]
    fn bad_select_falls_back_to_default() {
        let out = coerce_settings(WidgetKind::Weather, &json!({ "units": "kelvin" }));
        assert_eq!(out.get("units").unwrap(), "c");
    }

    #[test]
    fn checkbox_is_never_truthy_by_accident() {
        let out = coerce_settings(WidgetKind::Clock, &json!({ "seconds": "yes" }));
        assert_eq!(out.get("seconds").unwrap(), false);
    }

    #[test]
    fn huge_text_is_truncated_on_a_char_boundary() {
        let long = "é".repeat(2000);
        let out = coerce_settings(WidgetKind::Weather, &json!({ "location": long }));
        let s = out.get("location").unwrap().as_str().unwrap();
        assert!(s.len() <= 2000);
        assert!(s.chars().all(|c| c == 'é'), "no broken UTF-8");
    }

    #[test]
    fn missing_settings_become_defaults() {
        let out = coerce_settings(WidgetKind::Weather, &json!({}));
        assert_eq!(out.get("location").unwrap(), "Berlin");
        assert_eq!(out.get("units").unwrap(), "c");
        assert_eq!(out.get("days").unwrap(), 3);
    }

    #[test]
    fn widget_round_trips_through_json() {
        let w = Widget::new("weather-1", WidgetKind::Weather);
        let s = serde_json::to_string(&w).unwrap();
        let back: Widget = serde_json::from_str(&s).unwrap();
        assert_eq!(back.kind, WidgetKind::Weather);
        assert_eq!(back.get_str("location"), "Berlin");
    }
}
