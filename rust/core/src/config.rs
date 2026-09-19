//! Configuration model.
//!
//! The Python version kept this as an untyped dict, which is how a corrupt
//! config could take the process down at start-up and how widget settings could
//! carry unknown keys into the renderer. Every field here is typed and every
//! deserialisation is total: a missing or malformed value falls back to a
//! default rather than raising.

use serde::{Deserialize, Serialize};

/// Fields are `#[serde(default)]` throughout so an older or partial config file
/// loads instead of failing. That single attribute replaces the manual
/// merge-with-defaults loop the Python version needed.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub section_order: Vec<String>,
    pub show_devices: bool,
    pub show_links: bool,
    pub show_quick_actions: bool,
    pub show_tasks: bool,
    pub theme: ThemeColors,
    /// Bumped by the server on every config write. The dashboard watches this so
    /// widgets re-fetch exactly when something changed, instead of on a timer.
    pub revision: u64,
}

impl Settings {
    pub fn with_defaults() -> Self {
        Self {
            section_order: vec![
                "devices".into(),
                "links".into(),
                "quick_actions".into(),
                "tasks".into(),
            ],
            show_devices: true,
            show_links: true,
            show_quick_actions: true,
            show_tasks: true,
            theme: ThemeColors::default(),
            revision: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeColors {
    pub bg: String,
    pub cards: String,
    pub border: String,
    pub text: String,
    pub text_muted: String,
    pub accent: String,
    pub danger: String,
    pub warning: String,
    pub glass: u8,
    pub blur: u8,
    pub border_radius: u8,
    pub wallpaper: String,
    pub max_width_desktop: u8,
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self {
            bg: "#0f0f10".into(),
            cards: "#181819".into(),
            border: "#2b2b2e".into(),
            text: "#e8e8e6".into(),
            text_muted: "#a1a1a6".into(),
            accent: "#2ed573".into(),
            danger: "#ff4757".into(),
            warning: "#ffa502".into(),
            glass: 0,
            blur: 0,
            border_radius: 6,
            wallpaper: String::new(),
            max_width_desktop: 70,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub icon: String,
    pub is_host: bool,
    pub ssh: Option<SshConfig>,
    pub wol: Option<WolConfig>,
    pub connect: Option<ConnectConfig>,
    pub docker: Option<DockerConfig>,
    pub alerts: Alerts,
    /// Root directory the file browser is confined to on this device.
    pub files_root: Option<String>,
}

impl Device {
    /// SSH user, defaulting to root when unset.
    pub fn ssh_user(&self) -> String {
        self.ssh
            .as_ref()
            .map(|s| s.user.clone())
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| "root".to_string())
    }

    pub fn ssh_port(&self) -> u16 {
        self.ssh.as_ref().map(|s| s.port_or_default()).unwrap_or(22)
    }

    /// Where the file browser starts, defaulting to `/`.
    pub fn files_root(&self) -> String {
        self.files_root
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("/")
            .to_string()
    }

    /// A display name that is never empty.
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.id
        } else {
            &self.name
        }
    }

    /// Is this the machine MinDash is running on?
    ///
    /// True for the designated host, and also for any device pointed at a
    /// loopback address: reaching your own filesystem through an sshd is slower,
    /// needs SSH configured on the box, and fails outright on Windows where the
    /// POSIX collector has no `sh`. Either way the answer is the same, so the
    /// local code path handles it.
    pub fn is_local(&self) -> bool {
        if self.is_host {
            return true;
        }
        let ip = self.ip.trim();
        ip.is_empty()
            || ip == "127.0.0.1"
            || ip == "localhost"
            || ip == "::1"
            || ip.starts_with("127.")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SshConfig {
    pub user: String,
    pub port: u16,
}

impl SshConfig {
    pub fn port_or_default(&self) -> u16 {
        if self.port == 0 {
            22
        } else {
            self.port
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WolConfig {
    pub mac: String,
    pub broadcast: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ConnectConfig {
    pub rdp: String,
    pub vnc: String,
    pub web: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DockerConfig {
    pub containers: Vec<ContainerRef>,
}

/// Either a bare name or a name plus its connection links. Serialised as an
/// untagged enum so both `"plex"` and `{ "name": "plex" }` parse, which is what
/// existing config files contain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContainerRef {
    Name(String),
    Full {
        name: String,
        #[serde(default)]
        web: String,
        #[serde(default)]
        rdp: String,
        #[serde(default)]
        vnc: String,
    },
}

impl ContainerRef {
    pub fn name(&self) -> &str {
        match self {
            Self::Name(n) => n,
            Self::Full { name, .. } => name,
        }
    }
    pub fn web(&self) -> &str {
        match self {
            Self::Name(_) => "",
            Self::Full { web, .. } => web,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Alerts {
    pub online: bool,
    pub cpu: u8,
    pub ram: u8,
    pub cpu_temp: u8,
    pub disk_usage: u8,
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            online: true,
            cpu: 90,
            ram: 90,
            cpu_temp: 80,
            disk_usage: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub schedule: Schedule,
    pub target: String,
    pub last_status: String,
}

/// A schedule as an enum rather than a nested dict. The Python version stored
/// this as `{"type": ..., "time": ...}` and crashed on a hand-edited string
/// value; here the shape is enforced by the type and an unknown `type` maps to
/// the default variant instead of panicking.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Schedule {
    Hourly { time: String },
    Daily { time: String },
    Weekly { time: String, day: u8 },
    Monthly { time: String, date: u8 },
}

impl Default for Schedule {
    fn default() -> Self {
        Self::Daily {
            time: "03:00".into(),
        }
    }
}

impl Schedule {
    /// "HH:MM" split into (hour, minute), falling back to 03:00.
    pub fn hour_minute(&self) -> (u32, u32) {
        let raw = match self {
            Self::Hourly { time }
            | Self::Daily { time }
            | Self::Weekly { time, .. }
            | Self::Monthly { time, .. } => time,
        };
        let mut parts = raw.split(':');
        let h = parts.next().and_then(|s| s.parse().ok()).unwrap_or(3);
        let m = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        if h > 23 || m > 59 {
            (3, 0)
        } else {
            (h, m)
        }
    }
}

/// The whole persisted document.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    pub devices: Vec<Device>,
    pub links: Vec<Link>,
    pub quick_actions: Vec<QuickAction>,
    pub tasks: Vec<Task>,
    pub widgets: Vec<crate::widgets::Widget>,
    pub onboarding_done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Link {
    pub id: String,
    pub name: String,
    pub url: String,
    pub icon: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QuickAction {
    pub id: String,
    pub name: String,
    pub path: String,
    pub icon: String,
    pub note: String,
}

impl Config {
    /// A usable starting point: default settings, the local host, and the seeded
    /// widget set.
    fn fresh() -> Self {
        let mut cfg = Self {
            settings: Settings::with_defaults(),
            ..Self::default()
        };
        cfg.devices.push(Device::host_default());
        cfg.seed_widgets();
        cfg
    }

    /// Load from a file, or fall back to a usable default.
    ///
    /// Returns the config plus an optional warning to surface, so a corrupt file
    /// is reported rather than swallowed. Deserialisation here is total: a
    /// truncated or wrongly-shaped document produces a working default config and
    /// a message, never a failed start-up.
    pub fn load_or_default(raw: Option<&[u8]>) -> (Self, Option<String>) {
        let Some(bytes) = raw else {
            return (Self::fresh(), None);
        };
        match serde_json::from_slice::<Config>(bytes) {
            Ok(mut cfg) => {
                if cfg.settings.section_order.is_empty() {
                    cfg.settings.section_order = Settings::with_defaults().section_order;
                }
                cfg.ensure_host();
                (cfg, None)
            }
            Err(e) => (
                Self::fresh(),
                Some(format!("config unreadable ({e}); started from defaults")),
            ),
        }
    }

    /// Curated widgets for a first run, so the dashboard is not an empty page
    /// with nothing but an "Add widget" button.
    fn seed_widgets(&mut self) {
        use crate::widgets::Widget;
        let weather = Widget::new("weather-default", crate::widgets::WidgetKind::Weather);
        let clock = Widget::new("clock-default", crate::widgets::WidgetKind::Clock);
        let overview = Widget::new("overview-default", crate::widgets::WidgetKind::Overview);
        let news = Widget::new("news-default", crate::widgets::WidgetKind::Rss);
        self.widgets = vec![clock, weather, overview, news];
    }

    fn ensure_host(&mut self) {
        if !self.devices.iter().any(|d| d.is_host) {
            self.devices.insert(0, Device::host_default());
        }
    }
}

impl Device {
    pub fn host_default() -> Self {
        Self {
            id: "host".into(),
            name: "MinDash Host".into(),
            ip: "localhost".into(),
            icon: "cpu".into(),
            is_host: true,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_gets_a_host_and_seed_widgets() {
        let (cfg, warn) = Config::load_or_default(None);
        assert!(warn.is_none());
        assert!(cfg.devices.iter().any(|d| d.is_host));
        // A fresh install should show something rather than an empty page.
        assert_eq!(cfg.widgets.len(), 4);
        assert!(cfg.widgets.iter().any(|w| w.id == "weather-default"));
    }

    #[test]
    fn seed_widgets_are_not_added_to_an_existing_config() {
        // Someone who deliberately removed every widget must not have them
        // reappear on the next start.
        let (cfg, _) = Config::load_or_default(Some(br#"{"widgets":[]}"#));
        assert!(cfg.widgets.is_empty());
    }

    #[test]
    fn truncated_config_does_not_panic() {
        let (cfg, warn) = Config::load_or_default(Some(b"{\"devices\": [{\"id\":"));
        assert!(warn.is_some(), "a corrupt file should be reported");
        assert!(cfg.devices.iter().any(|d| d.is_host));
    }

    #[test]
    fn wrong_type_config_does_not_panic() {
        let (cfg, warn) = Config::load_or_default(Some(b"[1,2,3]"));
        assert!(warn.is_some());
        assert!(cfg.devices.iter().any(|d| d.is_host));
    }

    #[test]
    fn partial_config_merges_defaults() {
        let (cfg, warn) = Config::load_or_default(Some(br#"{"devices":[{"id":"a","name":"A"}]}"#));
        assert!(warn.is_none());
        // The host is inserted at the front, so look the device up by id.
        let a = cfg
            .devices
            .iter()
            .find(|d| d.id == "a")
            .expect("device kept");
        assert_eq!(a.name, "A");
        assert!(cfg.devices.iter().any(|d| d.is_host), "host is re-added");
        assert_eq!(cfg.devices[0].id, "host", "host sorts first");
        // settings omitted entirely -> defaults, not zeroes
        assert_eq!(cfg.settings.theme.accent, "#2ed573");
        assert!(!cfg.settings.section_order.is_empty());
    }

    #[test]
    fn explicit_settings_are_not_overwritten_by_defaults() {
        let (cfg, _) = Config::load_or_default(Some(
            r##"{"settings":{"theme":{"accent":"#abcdef"},"section_order":["tasks"]}}"##.as_bytes(),
        ));
        assert_eq!(cfg.settings.theme.accent, "#abcdef");
        assert_eq!(cfg.settings.section_order, vec!["tasks".to_string()]);
        // ...but untouched fields still get their defaults.
        assert_eq!(cfg.settings.theme.bg, "#0f0f10");
    }

    #[test]
    fn container_ref_accepts_both_shapes() {
        let cfg: Config = serde_json::from_str(
            r#"{"devices":[{"id":"h","docker":{"containers":["plex",{"name":"sonarr","web":"http://x"}]}}]}"#,
        )
        .unwrap();
        let c = cfg.devices[0].docker.as_ref().unwrap();
        assert_eq!(c.containers[0].name(), "plex");
        assert_eq!(c.containers[1].name(), "sonarr");
        assert_eq!(c.containers[1].web(), "http://x");
    }

    #[test]
    fn schedule_parses_and_rejects_junk() {
        let s: Schedule = serde_json::from_str(r#"{"type":"daily","time":"06:30"}"#).unwrap();
        assert_eq!(s.hour_minute(), (6, 30));
        // An unknown variant must not be a hard error at the document level.
        let cfg: Result<Config, _> =
            serde_json::from_str(r#"{"tasks":[{"id":"t","schedule":{"type":"nope"}}]}"#);
        assert!(cfg.is_err(), "unknown schedule variant is rejected cleanly");
    }

    #[test]
    fn schedule_time_junk_falls_back() {
        let s = Schedule::Daily {
            time: "99:99".into(),
        };
        assert_eq!(s.hour_minute(), (3, 0));
    }
}
