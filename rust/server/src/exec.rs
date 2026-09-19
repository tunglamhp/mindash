//! Remote command execution over SSH.
//!
//! Uses the system `ssh` client rather than a pure-Rust SSH implementation. That
//! is deliberate, and it is the same trade the original made:
//!
//! * key-based auth, `~/.ssh/config`, agent forwarding, jump hosts and
//!   `known_hosts` all keep working with no code
//! * no `russh`/`ring`/`openssl` dependency tree, so the server still builds on a
//!   bare machine and stays a single small binary
//! * `ssh` is present on every host MinDash targets
//!
//! What is *not* carried over is string interpolation into a shell. Every call
//! here is an argv vector, and anything that must reach the remote shell as a
//! single argument is quoted by [`quote`], which is unit-tested against the
//! quoting cases that actually break (`'`, spaces, `$`, backticks, newlines).

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("ssh timed out after {0:?}")]
    Timeout(Duration),
    #[error("{0}")]
    Failed(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Wrap a string for safe use as one POSIX shell word.
///
/// A single-quoted string ends only at another `'`, so the only character that
/// needs care is `'` itself: close, emit an escaped quote, reopen.
pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Base64-encode for `sh -c 'echo <b64> | base64 -d | sh'`.
///
/// Sending a script this way sidesteps remote-shell quoting entirely: base64
/// output is `[A-Za-z0-9+/=]` only, so nothing in the script can be
/// reinterpreted by the login shell.
pub fn b64(data: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data.as_bytes())
}

/// How to reach a host.
#[derive(Debug, Clone, Default)]
pub struct SshTarget {
    pub ip: String,
    pub user: String,
    pub port: u16,
    /// Extra options, e.g. the local test target's identity file.
    pub key: Option<String>,
}

impl SshTarget {
    pub fn port_or_default(&self) -> u16 {
        if self.port == 0 {
            22
        } else {
            self.port
        }
    }

    fn args(&self) -> Vec<String> {
        let mut args = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
            "-o".into(),
            "ConnectTimeout=5".into(),
            "-o".into(),
            "ServerAliveInterval=10".into(),
            "-o".into(),
            "ServerAliveCountMax=2".into(),
            "-p".into(),
            self.port_or_default().to_string(),
        ];
        if let Some(key) = &self.key {
            args.push("-i".into());
            args.push(key.clone());
            // A generated key on a shared filesystem is often "too open"; this
            // is the documented way to use it without world-readable perms.
            args.push("-o".into());
            args.push("IdentitiesOnly=yes".into());
        }
        args
    }

    /// Run a remote shell command, returning trimmed stdout.
    pub async fn run(&self, command: &str, timeout: Duration) -> Result<String, ExecError> {
        if self.ip.trim().is_empty() {
            return Err(ExecError::Failed("no host configured".into()));
        }
        if self.user.trim().is_empty() {
            return Err(ExecError::Failed("no SSH user configured".into()));
        }

        let mut cmd = Command::new("ssh");
        cmd.args(self.args())
            .arg(format!("{}@{}", self.user, self.ip))
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| ExecError::Timeout(timeout))??;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let err = String::from_utf8_lossy(&output.stderr);
            let err = err.trim();
            Err(ExecError::Failed(if err.is_empty() {
                format!("ssh exited with {}", output.status)
            } else {
                err.chars().take(300).collect()
            }))
        }
    }

    /// Send a shell script and run it.
    pub async fn script(&self, script: &str, timeout: Duration) -> Result<String, ExecError> {
        let wrapped = format!("echo {} | base64 -d | sh", b64(script));
        self.run(&wrapped, timeout).await
    }

    /// Is the host reachable over SSH?
    pub async fn reachable(&self, timeout: Duration) -> bool {
        self.run("true", timeout).await.is_ok()
    }
}

/// Read CPU, memory, temperature, disks and uptime from `/proc`.
///
/// One round trip: the remote collects everything and emits `key=value` lines, so
/// there is no shell parsing to get wrong on the server side.
pub async fn remote_stats(target: &SshTarget) -> Result<serde_json::Value, ExecError> {
    let raw = target.script(STATS_SCRIPT, Duration::from_secs(12)).await?;
    Ok(parse_stats(&raw))
}

/// Stats for the machine MinDash itself runs on.
///
/// The host is not reached over SSH. MinDash is *on* that machine, so requiring an
/// sshd and a key to read local counters would be absurd — and would fail on
/// exactly the single-machine install this is most often used for.
///
/// The collector is a POSIX shell script, which needs `sh`. On Windows that does
/// not exist, so [`crate::win_stats::collect`] reads the same values natively and
/// emits the identical shape; either way one parser handles the result.
pub async fn local_stats() -> Result<serde_json::Value, ExecError> {
    #[cfg(windows)]
    {
        let raw = crate::win_stats::collect()
            .await
            .map_err(ExecError::Failed)?;
        Ok(parse_stats(&raw))
    }

    #[cfg(not(windows))]
    {
        let output = tokio::time::timeout(
            Duration::from_secs(8),
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(STATS_SCRIPT)
                .output(),
        )
        .await
        .map_err(|_| ExecError::Timeout(Duration::from_secs(8)))??;

        if output.status.success() {
            let raw = String::from_utf8_lossy(&output.stdout);
            Ok(parse_stats(&raw))
        } else {
            let err = String::from_utf8_lossy(&output.stderr);
            Err(ExecError::Failed(err.trim().chars().take(200).collect()))
        }
    }
}

/// The stats collector, shared by the SSH and local paths.
const STATS_SCRIPT: &str = r#"
cpu=""; mem_total=""; mem_avail=""; temp=""; uptime_s=""
if [ -r /proc/loadavg ]; then cpu=$(cut -d' ' -f1 /proc/loadavg); fi
if [ -r /proc/meminfo ]; then
  mem_total=$(awk '/^MemTotal:/{print $2}' /proc/meminfo)
  mem_avail=$(awk '/^MemAvailable:/{print $2}' /proc/meminfo)
fi
if [ -r /proc/uptime ]; then uptime_s=$(cut -d' ' -f1 /proc/uptime); fi
for z in /sys/class/thermal/thermal_zone*/temp; do
  if [ -r "$z" ]; then temp=$(cat "$z"); break; fi
done
echo "cpu=$cpu"
echo "mem_total=$mem_total"
echo "mem_avail=$mem_avail"
echo "temp=$temp"
echo "uptime=$uptime_s"
echo "cores=$(nproc 2>/dev/null || echo 1)"
echo "DISKS"
df -B1 -P 2>/dev/null | awk 'NR>1 {print $1 "\t" $6 "\t" $2 "\t" $3}'
"#;

/// Short device label for a disk row.
///
/// `/dev/nvme0n1p2` becomes `nvme0n1p` (the partition suffix is dropped so the
/// label names the disk), and a Windows drive stays as its letter.
fn device_label(source: &str) -> String {
    let trimmed = source.trim();
    // Windows: `C:` or `C:\`.
    if trimmed.len() >= 2 && trimmed.as_bytes()[1] == b':' {
        return trimmed.chars().next().unwrap_or('?').to_string();
    }
    let name = trimmed.split('/').next_back().unwrap_or("");
    // Strip a trailing partition number, but never the whole name: `/dev/12` and
    // `/dev/md0` must keep something.
    let stripped = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if stripped.is_empty() {
        name.to_string()
    } else {
        stripped.to_string()
    }
}

/// Is this a filesystem worth showing?
///
/// Accepts the real Unix mount points and Windows drive roots, and rejects the
/// kernel pseudo-filesystems (`/proc`, `/sys`, `/dev`, tmpfs, overlay) that
/// would otherwise fill the list with entries nobody cares about.
fn is_interesting_mount(mount: &str) -> bool {
    // Windows: a drive letter root, e.g. `C:\`.
    let bytes = mount.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return true;
    }
    if mount == "/" {
        return true;
    }
    const ROOTS: [&str; 6] = ["/home", "/mnt", "/media", "/srv", "/data", "/var"];
    if !ROOTS.iter().any(|p| mount.starts_with(p)) {
        return false;
    }
    // A mount under one of those roots can still be a pseudo-filesystem, e.g.
    // `/var/lib/docker/overlay2` on overlayfs.
    const SKIP: [&str; 6] = [
        "/var/lib/docker",
        "/var/lib/containers",
        "/proc",
        "/sys",
        "/dev",
        "/run",
    ];
    !SKIP.iter().any(|p| mount.starts_with(p))
}

// ---------------------------------------------------------------------------
// Local files
//
// The host's own filesystem is read natively rather than through a shell. That
// is faster, needs no `sh` (which Windows lacks), and removes the quoting
// surface entirely -- there is no shell to escape anything for. It also means
// the file browser works on the machine MinDash runs on with no sshd at all.
// ---------------------------------------------------------------------------

/// List a local directory.
pub async fn list_dir_local(path: &str, root: &str) -> Result<(String, Vec<FileEntry>), FileError> {
    let dir = safe_path(path, root)?;
    let mut entries = Vec::new();
    let mut reader = tokio::fs::read_dir(&dir).await.map_err(local_io)?;

    while let Some(entry) = reader.next_entry().await.map_err(local_io)? {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.is_empty() || name == "." || name == ".." {
            continue;
        }
        // A broken symlink or a permission error on one entry must not blank the
        // whole listing, so each row is skipped individually.
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        let full = join_path(&dir, &name);
        entries.push(FileEntry {
            path: full,
            name,
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default(),
            mode: local_mode(&meta),
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries.truncate(500);
    Ok((dir, entries))
}

/// Read a local file, capped.
pub async fn read_file_local(path: &str, root: &str, max_bytes: u64) -> Result<String, FileError> {
    let file = safe_path(path, root)?;
    let meta = tokio::fs::metadata(&file).await.map_err(local_io)?;
    if meta.is_dir() {
        return Err(FileError::Other("that is a directory".into()));
    }
    if meta.len() > max_bytes {
        return Err(FileError::Other(format!(
            "file is larger than the {} KB preview limit",
            max_bytes / 1024
        )));
    }
    tokio::fs::read_to_string(&file).await.map_err(local_io)
}

fn local_io(e: std::io::Error) -> FileError {
    FileError::Other(match e.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => e.to_string(),
    })
}

/// Permission bits for a local file.
///
/// Unix gets the familiar `rwxr-xr-x`. Windows has no mode bits, so the
/// read-only attribute and the directory flag are rendered in the same shape
/// rather than inventing permissions that do not exist.
#[cfg(unix)]
fn local_mode(meta: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bits = meta.permissions().mode();
    let mut out = String::with_capacity(10);
    out.push(if meta.is_dir() { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        let trio = (bits >> shift) & 0o7;
        out.push(if trio & 0o4 != 0 { 'r' } else { '-' });
        out.push(if trio & 0o2 != 0 { 'w' } else { '-' });
        out.push(if trio & 0o1 != 0 { 'x' } else { '-' });
    }
    out
}

#[cfg(not(unix))]
fn local_mode(meta: &std::fs::Metadata) -> String {
    let head = if meta.is_dir() { 'd' } else { '-' };
    // `readonly()` is the one permission Windows does express here.
    let write = if meta.permissions().readonly() {
        '-'
    } else {
        'w'
    };
    format!("{head}r{write}-------")
}

/// Parse the `key=value` + `DISKS` block produced by the stats script.
pub fn parse_stats(raw: &str) -> serde_json::Value {
    let mut values = std::collections::HashMap::new();
    let mut disks = Vec::new();
    let mut in_disks = false;

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "DISKS" {
            in_disks = true;
            continue;
        }
        if in_disks {
            // Tab separated: device, mount, size, used. `df --output` pads with
            // spaces that vary with field width, so whitespace splitting puts the
            // columns in the wrong place.
            let parts: Vec<&str> = line.split('\t').map(str::trim).collect();
            if parts.len() >= 4 {
                let size: u64 = parts[2].parse().unwrap_or(0);
                let used: u64 = parts[3].parse().unwrap_or(0);
                let mount = parts[1].to_string();
                if is_interesting_mount(&mount) && size > 1_000_000_000 {
                    disks.push(serde_json::json!({
                        "mount": mount,
                        "device": device_label(parts[0]),
                        "total": size,
                        "used": used,
                    }));
                }
            }
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            values.insert(k.to_string(), v.to_string());
        }
    }

    let num = |key: &str| -> f64 {
        values
            .get(key)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0.0)
    };

    let cores = num("cores").max(1.0);
    let cpu = ((num("cpu") / cores) * 100.0).round().clamp(0.0, 100.0) as i64;

    // meminfo is in kB.
    let total_bytes = (num("mem_total") * 1024.0) as u64;
    let avail_bytes = (num("mem_avail") * 1024.0) as u64;
    let used_bytes = total_bytes.saturating_sub(avail_bytes);

    let temp_raw = num("temp");
    // thermal zones report millidegrees.
    let temp = if temp_raw > 1000.0 {
        Some((temp_raw / 1000.0).round() as i64)
    } else if temp_raw > 0.0 {
        Some(temp_raw.round() as i64)
    } else {
        None
    };

    let up = num("uptime") as u64;
    let (days, hours) = (up / 86_400, (up % 86_400) / 3600);
    let uptime = if days > 0 {
        format!("{days}d {hours}h")
    } else {
        format!("{hours}h")
    };

    serde_json::json!({
        "cpu": cpu,
        "ram_total": total_bytes,
        "ram_used": used_bytes,
        "temp": temp,
        "uptime": uptime,
        "disks": disks,
    })
}

/// Power operations.
///
/// Every variant is a fixed command with no interpolated input, so there is no
/// injection surface at all: the caller picks a variant, never a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerAction {
    Shutdown,
    Reboot,
    Suspend,
}

impl PowerAction {
    /// The command, prefixed with `sudo -n` so it fails fast rather than hanging
    /// on a password prompt that cannot be answered over a non-interactive SSH
    /// session.
    pub fn command(self) -> &'static str {
        match self {
            Self::Shutdown => "sudo -n shutdown -h now",
            Self::Reboot => "sudo -n reboot",
            Self::Suspend => "sudo -n systemctl suspend",
        }
    }
}

/// Run a power action against a host.
pub async fn power(target: &SshTarget, action: PowerAction) -> Result<(), ExecError> {
    // A shutdown tears the connection down before it can reply, so a non-zero
    // exit is expected and is not a failure.
    match target.run(action.command(), Duration::from_secs(10)).await {
        Ok(_) => Ok(()),
        Err(ExecError::Failed(msg)) if is_disconnect(&msg) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Did the connection drop mid-command, as opposed to failing before it ran?
///
/// This decides whether a shutdown is reported as success. A host going down
/// tears the connection before it can reply, so that has to count as working.
/// A connection *timeout* or a refused connection is different: nothing ran, and
/// reporting success would tell the user their server is rebooting when in fact
/// it is unreachable. Note `"timed out"` does not contain `"timeout"` — matching
/// on the wrong one silently swallowed real reachability failures.
fn is_disconnect(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "closed by remote host",
        "connection to", // "Connection to <host> closed."
        "connection reset",
        "connection closed",
        "broken pipe",
        "remote host",
        "connection refused",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("path escapes the listing root")]
    Escape,
    #[error("{0}")]
    Exec(#[from] ExecError),
    #[error("{0}")]
    Other(String),
}

/// A filesystem entry, as shown in the file browser.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileEntry {
    /// Final path component, what the row displays.
    pub name: String,
    /// Absolute remote path, what the browser requests next.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: String,
    /// Permission bits as `rwxr-xr-x`.
    pub mode: String,
}

/// Reject a path that could climb out of the root it was given.
///
/// The listing itself is read-only and runs over SSH as the configured user, so
/// this is defence in depth rather than the only barrier. It rejects `..`
/// traversal and absolute paths outside the allowed root, and refuses control
/// characters so a path can never carry a newline into the remote shell.
pub fn safe_path(requested: &str, root: &str) -> Result<String, FileError> {
    let root = normalise(root);
    if requested.chars().any(|c| c.is_control()) {
        return Err(FileError::Other("path contains control characters".into()));
    }

    let candidate = if requested.trim().is_empty() {
        root.clone()
    } else if requested.starts_with('/') || is_drive_root(requested) {
        normalise(requested)
    } else {
        normalise(&format!("{}/{}", root, requested.trim_start_matches("./")))
    };

    // Compare on the normalised forms, and require the boundary to fall on a
    // separator so `/srv/backups-evil` does not pass as a child of `/srv/backups`.
    let inside = candidate == root
        || root == "/"
        || candidate.starts_with(&format!("{}/", root.trim_end_matches('/')));
    if !inside {
        return Err(FileError::Escape);
    }
    Ok(candidate)
}

/// Resolve `.` and `..` lexically, without touching the filesystem.
///
/// Backslashes are folded to forward slashes first, so a Windows root like
/// `C:\Users` behaves the same as a Unix one. Without that, `C:\Users` would be
/// treated as a single path component, and every child path built by appending
/// `/name` would compare as outside the root.
///
/// An empty segment is a leading, trailing or doubled separator; none of them
/// contribute a name, so all are skipped. That is what makes `/srv/media/`
/// normalise to `/srv/media` rather than `/srv/media/`.
fn normalise(path: &str) -> String {
    let unified = path.replace('\\', "/");
    let absolute = unified.starts_with('/');

    // Split a drive prefix off first, so `..` can never pop it away. On Windows
    // `C:` is not a directory component -- it is the root of that drive -- and
    // treating it as a name let `C:/..` climb above the root entirely.
    let (prefix, rest) = match drive_prefix(&unified) {
        Some(drive) => (drive, &unified[2..]),
        None => (String::new(), unified.as_str()),
    };

    let mut out: Vec<&str> = Vec::new();
    for part in rest.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");

    // A drive always keeps a separator: `C:` alone is the process's current
    // directory on that drive, not its root.
    if !prefix.is_empty() {
        return format!("{prefix}/{}", joined.trim_start_matches('/'));
    }
    if absolute {
        return format!("/{joined}");
    }
    joined
}

/// The `C:`-style drive prefix of a path, if it has one.
fn drive_prefix(path: &str) -> Option<String> {
    let b = path.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        Some(path[..2].to_string())
    } else {
        None
    }
}

/// Is this a Windows drive root such as `C:` or `C:/`?
fn is_drive_root(path: &str) -> bool {
    drive_prefix(path).is_some()
}

/// Join a directory and a name with the separator the directory already uses.
///
/// On Windows the listing must hand back `C:\Users\me\file`, not
/// `C:\Users\me/file`, or the next request carries a mixed path.
fn join_path(dir: &str, name: &str) -> String {
    let trimmed = dir.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return format!("/{name}");
    }
    // A drive with no separator is not a directory, so re-add it.
    if trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' {
        return format!("{trimmed}\\{name}");
    }
    if trimmed.contains('\\') || is_drive_root(trimmed) {
        format!("{trimmed}\\{name}")
    } else {
        format!("{trimmed}/{name}")
    }
}

/// List a directory over SSH.
pub async fn list_dir(
    target: &SshTarget,
    path: &str,
    root: &str,
) -> Result<(String, Vec<FileEntry>), FileError> {
    let dir = safe_path(path, root)?;

    // `stat -c` does NOT interpret backslash escapes in its format: it printed a
    // literal `\t`, so splitting the output on tabs found nothing and every
    // listing came back empty. The shell has to expand them, which is what
    // `$'...'` does. `%n` is last because a filename may contain anything,
    // including a tab, and it is then the only field allowed to.
    //
    // The explicit `dir/.[!.]*` and `dir/..?*` patterns are what make dotfiles
    // show up; a bare `dir/*` omits them.
    let script = format!(
        "for f in {dir}/* {dir}/.[!.]* {dir}/..?*; do\n\
         [ -e \"$f\" ] || continue\n\
         stat -c $'%F\\t%s\\t%Y\\t%A\\t%n' \"$f\" 2>/dev/null\n\
         done\n\
         printf 'DIR\\t%s\\n' {dir}\n",
        dir = quote(&dir),
    );

    let raw = target.script(&script, Duration::from_secs(12)).await?;
    let mut entries = Vec::new();
    let mut resolved = dir.clone();

    for line in raw.lines() {
        // splitn(5, ..) keeps any tabs inside the filename in the last field.
        let mut fields = line.splitn(5, '\t');
        let (Some(kind), Some(size), Some(modified), Some(mode), Some(name)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };

        if kind == "DIR" {
            resolved = name.trim().to_string();
            continue;
        }

        // `stat %n` prints the full path, so reduce it to the final component.
        // A path ending in a separator has no name, so fall back to the text.
        let full = name.trim_end_matches('\n').to_string();
        let base = full
            .trim_end_matches('/')
            .rsplit_once('/')
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| full.clone());
        if base.is_empty() || base == "." || base == ".." {
            continue;
        }

        entries.push(FileEntry {
            path: full,
            name: base,
            is_dir: kind.trim() == "directory",
            size: size.trim().parse().unwrap_or(0),
            modified: modified.trim().to_string(),
            mode: mode.trim().to_string(),
        });
    }

    // Directories first, then case-insensitive by name.
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries.truncate(500);

    Ok((resolved, entries))
}

/// Read a file's contents, capped so a huge file cannot exhaust memory.
pub async fn read_file(
    target: &SshTarget,
    path: &str,
    root: &str,
    max_bytes: u64,
) -> Result<String, FileError> {
    let file = safe_path(path, root)?;
    // `head -c` bounds the transfer on the remote side; the extra byte lets us
    // detect that truncation happened without a second round trip.
    let script = format!("head -c {} {}", max_bytes + 1, quote(&file));
    let raw = target.script(&script, Duration::from_secs(15)).await?;

    let bytes = raw.len() as u64;
    if bytes > max_bytes {
        return Err(FileError::Other(format!(
            "file is larger than the {} KB preview limit",
            max_bytes / 1024
        )));
    }
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_wraps_plain_words() {
        assert_eq!(quote("hello"), "'hello'");
        assert_eq!(quote(""), "''");
    }

    #[test]
    fn quote_handles_embedded_single_quotes() {
        // The classic: close, escaped quote, reopen.
        assert_eq!(quote("it's"), r#"'it'\''s'"#);
        assert_eq!(quote("'''"), r#"''\'''\'''\'''"#);
        assert_eq!(quote("a'b'c"), r#"'a'\''b'\''c'"#);
    }

    #[test]
    fn quote_neutralises_shell_metacharacters() {
        // None of these can escape a single-quoted word.
        for hostile in [
            "; rm -rf /",
            "$(whoami)",
            "`id`",
            "&& curl evil.test",
            "| sh",
            "> /etc/passwd",
            "\n rm -rf /",
            "a\nb",
            "*",
        ] {
            let quoted = quote(hostile);
            assert!(
                quoted.starts_with('\'') && quoted.ends_with('\''),
                "{hostile}"
            );
            // The only quotes inside must be the escaped sequence.
            let inner = &quoted[1..quoted.len() - 1];
            let mut rest = inner;
            while let Some(pos) = rest.find('\'') {
                assert!(
                    rest[pos..].starts_with("'\\''"),
                    "unescaped quote in {quoted}"
                );
                rest = &rest[pos + 4..];
            }
        }
    }

    #[test]
    fn b64_output_is_shell_safe() {
        for input in ["rm -rf /", "echo 'hi'", "$(id)", "line1\nline2", "üñí"] {
            let encoded = b64(input);
            assert!(!encoded.is_empty());
            assert!(
                encoded
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')),
                "{encoded} contains a shell metacharacter"
            );
        }
    }

    #[test]
    fn b64_round_trips() {
        use base64::Engine;
        let script = "echo hello; if [ -r /proc/x ]; then echo yes; fi";
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64(script))
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), script);
    }

    #[test]
    fn stats_parse_a_realistic_payload() {
        // Tabs, matching what `awk '... "\t" ...'` emits: device, mount, size,
        // used. Note there is no filesystem-type column -- `df -P` does not
        // produce one, and `-B1` is what makes the numbers bytes rather than
        // 1K blocks.
        let raw = "cpu=0.42\n\
mem_total=16384000\n\
mem_avail=12288000\n\
temp=47000\n\
uptime=259200\n\
cores=4\n\
DISKS\n\
/dev/nvme0n1p2\t/\t1000000000000\t480000000000\n\
/dev/sda1\t/mnt/media\t8000000000000\t2000000000000\n\
tmpfs\t/run\t100000\t1000\n";
        let out = parse_stats(raw);
        assert_eq!(out["cpu"], 11); // 0.42 / 4 * 100, rounded
        assert_eq!(out["ram_total"], 16_777_216_000u64);
        assert_eq!(out["ram_used"], 4_194_304_000u64);
        assert_eq!(out["temp"], 47);
        assert_eq!(out["uptime"], "3d 0h");
        let disks = out["disks"].as_array().unwrap();
        // tmpfs is too small to be interesting.
        assert_eq!(disks.len(), 2);
        assert_eq!(disks[0]["mount"], "/");
        assert_eq!(disks[0]["device"], "nvme0n1p");
        assert_eq!(disks[0]["total"], 1_000_000_000_000u64);
        assert_eq!(disks[1]["mount"], "/mnt/media");
    }

    #[test]
    fn stats_tolerate_an_empty_or_partial_payload() {
        let out = parse_stats("");
        assert_eq!(out["cpu"], 0);
        assert_eq!(out["ram_total"], 0);
        assert_eq!(out["temp"], serde_json::Value::Null);
        assert_eq!(out["disks"].as_array().unwrap().len(), 0);

        // A host without thermal zones reports no temperature, not zero.
        let out = parse_stats("cpu=0\ntemp=\ncores=1\nDISKS\n");
        assert_eq!(out["temp"], serde_json::Value::Null);
    }

    #[test]
    fn stats_handle_a_zero_core_count_without_dividing_by_zero() {
        let out = parse_stats("cpu=1.0\ncores=0\n");
        assert!(out["cpu"].as_i64().unwrap() <= 100);
    }

    #[test]
    fn a_non_numeric_size_column_yields_no_disk_not_a_zero_sized_one() {
        // Regression: an earlier fixture put a filesystem *type* where the size
        // belongs (`/dev/sda1 / ext4 1000 500`). `parse().unwrap_or(0)` turned
        // "ext4" into 0, the size filter then dropped the row, and the bug showed
        // up only as a silently missing disk. The row must be rejected outright.
        let raw = "cpu=1\nDISKS\n/dev/sda1\t/\text4\t1000000000000\t500\n";
        let out = parse_stats(raw);
        assert_eq!(
            out["disks"].as_array().unwrap().len(),
            0,
            "a malformed row must not masquerade as a valid disk"
        );
    }

    #[test]
    fn a_zero_sized_mount_is_dropped() {
        let raw = "cpu=1\nDISKS\n/dev/sda1\t/\t0\t0\n";
        let out = parse_stats(raw);
        assert_eq!(out["disks"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn windows_drive_roots_are_kept() {
        // The Windows collector reports `C:\` style mounts, which the Unix-only
        // prefix list rejected outright -- so a Windows host showed no disks at
        // all even though the collector had reported them correctly.
        let raw = "cpu=1\ncores=8\nDISKS\n\
C:\tC:\\\t499051917312\t485865648128\n\
D:\tD:\\\t1000186314752\t480970801152\n";
        let out = parse_stats(raw);
        let disks = out["disks"].as_array().unwrap();
        assert_eq!(disks.len(), 2, "windows drives must survive the filter");
        assert_eq!(disks[0]["mount"], "C:\\");
        assert_eq!(disks[0]["device"], "C");
        assert_eq!(disks[1]["device"], "D");
    }

    #[test]
    fn pseudo_filesystems_are_rejected() {
        for mount in [
            "/proc",
            "/sys",
            "/dev",
            "/run",
            "/dev/shm",
            "/proc/acpi",
            "/var/lib/docker/overlay2",
            "/var/lib/containers/storage",
        ] {
            assert!(!is_interesting_mount(mount), "{mount} should be skipped");
        }
        for mount in [
            "/",
            "/home",
            "/mnt/media",
            "/srv",
            "/data",
            "/var/log",
            "C:\\",
            "Z:\\",
        ] {
            assert!(is_interesting_mount(mount), "{mount} should be kept");
        }
    }

    #[test]
    fn device_labels_are_short() {
        assert_eq!(device_label("/dev/nvme0n1p2"), "nvme0n1p");
        assert_eq!(device_label("/dev/sda1"), "sda");
        assert_eq!(device_label("overlay"), "overlay");
        assert_eq!(device_label("C:"), "C");
        assert_eq!(device_label("C:\\"), "C");
        // A name that is only digits must not be trimmed away to nothing.
        assert_eq!(device_label("/dev/12"), "12");
    }

    #[test]
    fn target_requires_host_and_user() {
        let t = SshTarget::default();
        assert!(t.args().iter().any(|a| a == "22"));
        assert_eq!(t.port_or_default(), 22);
        let t = SshTarget {
            port: 2222,
            ..Default::default()
        };
        assert!(t.args().iter().any(|a| a == "2222"));
    }

    #[tokio::test]
    async fn run_without_a_host_fails_cleanly() {
        let t = SshTarget::default();
        let err = t.run("true", Duration::from_millis(200)).await;
        assert!(matches!(err, Err(ExecError::Failed(_))));
    }

    // -- path safety ------------------------------------------------------

    #[test]
    fn safe_path_allows_children_of_the_root() {
        for (req, want) in [
            ("/srv", "/srv"),
            ("/srv/", "/srv"),
            ("/srv/media", "/srv/media"),
            ("/srv//media//", "/srv/media"),
            ("/srv/./media", "/srv/media"),
            ("/srv/a/../media", "/srv/media"),
            ("", "/srv"),
        ] {
            assert_eq!(safe_path(req, "/srv").unwrap(), want, "{req}");
        }
    }

    #[test]
    fn safe_path_rejects_traversal() {
        for req in [
            "/srv/../etc",
            "/srv/../../etc/shadow",
            "/etc",
            "/",
            "/srv/../..",
            "..",
        ] {
            assert!(
                matches!(safe_path(req, "/srv"), Err(FileError::Escape)),
                "{req} should be rejected"
            );
        }
    }

    #[test]
    fn safe_path_rejects_a_sibling_with_a_shared_prefix() {
        // `/srv/backups-evil` must not pass as a child of `/srv/backups`.
        assert!(matches!(
            safe_path("/srv/backups-evil", "/srv/backups"),
            Err(FileError::Escape)
        ));
        // ...but a real child does.
        assert!(safe_path("/srv/backups/daily", "/srv/backups").is_ok());
    }

    #[test]
    fn safe_path_rejects_control_characters() {
        // A newline or a NUL would let the remote shell see a second command.
        for req in ["/srv/a\nb", "/srv/a\rb", "/srv/a\u{0}b"] {
            assert!(
                matches!(safe_path(req, "/srv"), Err(FileError::Other(_))),
                "control characters must be rejected: {req:?}"
            );
        }
    }

    #[test]
    fn shell_metacharacters_are_legal_in_a_path_and_left_intact() {
        // Semicolons, `$()` and backticks are legal filename characters. They are
        // safe not because the path is filtered but because `quote` wraps it in
        // single quotes before it reaches the remote shell, so the text stays data.
        for hostile in [
            "/srv/x;rm -rf /tmp", // no trailing slash: kept verbatim
            "/srv/$(whoami)",
            "/srv/`id`",
            "/srv/a&&b",
            "/srv/a|b",
        ] {
            assert_eq!(safe_path(hostile, "/srv").unwrap(), hostile, "{hostile}");
            let quoted = quote(hostile);
            assert!(
                quoted.starts_with('\'') && quoted.ends_with('\''),
                "{quoted}"
            );
            // The inner text survives untouched, so the remote shell sees one word.
            assert_eq!(&quoted[1..quoted.len() - 1], hostile);
        }
    }

    #[test]
    fn root_of_slash_allows_everything_absolute() {
        assert!(safe_path("/etc/passwd", "/").is_ok());
        assert!(safe_path("/", "/").is_ok());
    }

    #[test]
    fn normalise_collapses_dot_segments() {
        assert_eq!(normalise("/a/b/../c"), "/a/c");
        assert_eq!(normalise("/a/./b/"), "/a/b");
        assert_eq!(normalise("/../.."), "/");
        assert_eq!(normalise("rel/./x"), "rel/x");
    }

    #[test]
    fn normalise_folds_windows_separators() {
        // Without this a Windows root would be one opaque component and every
        // child would look like an escape attempt.
        assert_eq!(normalise(r"C:\Users"), "C:/Users");
        assert_eq!(normalise(r"C:\Users\me"), "C:/Users/me");
        assert_eq!(normalise(r"C:\Users\..\Public"), "C:/Public");
        assert_eq!(normalise("C:/Users/me/"), "C:/Users/me");
    }

    #[test]
    fn a_bare_drive_keeps_its_root_separator() {
        // `C:` is the process's current directory on that drive, not its root,
        // so collapsing `C:/` to `C:` is both wrong and broke the root
        // comparison -- a root of `C:/` never matched its own listing.
        assert_eq!(normalise("C:/"), "C:/");
        assert_eq!(normalise("C:\\"), "C:/");
        assert_eq!(normalise("c:/"), "c:/");
        assert_eq!(normalise("C:/Users"), "C:/Users");
        // Two-character non-drive names must not be treated as drives.
        assert_eq!(normalise("ab"), "ab");
    }

    #[test]
    fn dotdot_cannot_climb_above_a_drive_root() {
        // `C:` is a root, not a name, so `..` must not be able to pop it away and
        // turn the path into a relative one.
        assert_eq!(normalise("C:/.."), "C:/");
        assert_eq!(normalise("C:/../.."), "C:/");
        assert_eq!(normalise("C:/Users/.."), "C:/");
        assert_eq!(normalise("C:/Users/../.."), "C:/");
        // Unix behaves the same at its root.
        assert_eq!(normalise("/../.."), "/");
    }

    #[test]
    fn a_drive_root_is_its_own_root() {
        let root = "C:/";
        for ok in ["C:/", "C:/Users", "C:/Users/me", r"C:\Users"] {
            assert!(safe_path(ok, root).is_ok(), "{ok} should be allowed");
        }
        // There is nothing above a drive to escape to: `..` collapses back to the
        // root, which is already permitted. The same is true of `/srv/../..` on
        // Unix, which resolves to `/`.
        for weird in ["C:/../..", "C:/Users/../..", "C:/Users/../../.."] {
            assert_eq!(safe_path(weird, root).unwrap(), "C:/", "{weird}");
        }
    }

    #[test]
    fn windows_roots_confine_like_unix_ones() {
        let root = r"C:\Users\me";
        for ok in [r"C:\Users\me", r"C:\Users\me\docs", "C:/Users/me/docs"] {
            assert!(safe_path(ok, root).is_ok(), "{ok} should be allowed");
        }
        for bad in [r"C:\Users", r"C:\Windows", r"C:\Users\me\..\..\Windows"] {
            assert!(
                matches!(safe_path(bad, root), Err(FileError::Escape)),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn join_path_keeps_the_directory_separator() {
        assert_eq!(join_path("/etc", "hosts"), "/etc/hosts");
        assert_eq!(join_path("/etc/", "hosts"), "/etc/hosts");
        assert_eq!(join_path(r"C:\Users", "me"), r"C:\Users\me");
        assert_eq!(join_path("C:", "Users"), r"C:\Users");
        assert_eq!(join_path("C:/", "Users"), r"C:\Users");
        assert_eq!(join_path("/", "etc"), "/etc");
    }

    // -- power ------------------------------------------------------------

    #[test]
    fn power_commands_never_prompt_and_are_constant() {
        for action in [
            PowerAction::Shutdown,
            PowerAction::Reboot,
            PowerAction::Suspend,
        ] {
            let cmd = action.command();
            assert!(
                cmd.starts_with("sudo -n "),
                "{cmd} would prompt for a password"
            );
            // No interpolation surface: the command is a fixed literal.
            assert!(!cmd.contains("{}"));
        }
        assert_eq!(PowerAction::Reboot.command(), "sudo -n reboot");
    }

    #[test]
    fn power_actions_deserialise_from_lowercase() {
        for (json, want) in [
            ("\"shutdown\"", PowerAction::Shutdown),
            ("\"reboot\"", PowerAction::Reboot),
            ("\"suspend\"", PowerAction::Suspend),
        ] {
            let got: PowerAction = serde_json::from_str(json).unwrap();
            assert_eq!(got, want);
        }
        // An arbitrary command string is not accepted.
        assert!(serde_json::from_str::<PowerAction>("\"rm -rf /\"").is_err());
    }

    #[test]
    fn disconnect_detection_matches_real_ssh_errors() {
        for msg in [
            "Connection to 10.0.0.1 closed by remote host.",
            "Connection reset by peer",
            "Broken pipe",
            "ssh: Connection closed by 10.0.0.1 port 22",
        ] {
            assert!(is_disconnect(msg), "{msg} should count as a drop");
        }
    }

    #[test]
    fn unreachable_is_not_treated_as_a_disconnect() {
        // Nothing ran, so a "reboot" must not be reported as successful just
        // because the host was unreachable.
        for msg in [
            "ssh: connect to host 10.0.0.1 port 22: Connection timed out",
            "ssh: connect to host 10.0.0.1 port 22: No route to host",
            "sudo: a password is required",
            "Permission denied (publickey).",
        ] {
            assert!(!is_disconnect(msg), "{msg} is a real failure");
        }
    }
}
