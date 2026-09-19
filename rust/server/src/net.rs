//! Network and process helpers.
//!
//! Two things the Python version did that this deliberately does not:
//!
//! * Wake-on-LAN built the magic packet by hand. Python used `subprocess` with
//!   a `shell=True` string in several places for the same job; here the packet
//!   is assembled directly, so there is no external tool to depend on and no
//!   argument for a shell to reinterpret.
//! * Every command is an argv vector. No `sh -c`, no string interpolation, so a
//!   device name or container name can never become shell syntax. The Python
//!   version quoted with `shlex.quote`, which is correct on POSIX but silently
//!   wrong on Windows — and it still required the string path.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("invalid MAC address: {0}")]
    Mac(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

/// Parse `aa:bb:cc:dd:ee:ff`, `aa-bb-...`, or `aabbccddeeff`.
pub fn parse_mac(input: &str) -> Result<[u8; 6], NetError> {
    let clean: String = input
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.' | ' '))
        .collect();
    if clean.len() != 12 || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(NetError::Mac(input.to_string()));
    }
    let mut out = [0u8; 6];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16)
            .map_err(|_| NetError::Mac(input.to_string()))?;
    }
    Ok(out)
}

/// A Wake-on-LAN magic packet: 6 x 0xFF then the MAC repeated 16 times.
pub fn magic_packet(mac: [u8; 6]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(102);
    packet.extend_from_slice(&[0xFF; 6]);
    for _ in 0..16 {
        packet.extend_from_slice(&mac);
    }
    packet
}

/// Send the magic packet to a broadcast address on the discard ports that
/// network cards listen on.
pub fn wake_on_lan(mac: &str, broadcast: &str) -> Result<(), NetError> {
    let mac = parse_mac(mac)?;
    let packet = magic_packet(mac);
    let target: IpAddr = broadcast
        .parse()
        .map_err(|_| NetError::Other(format!("invalid broadcast address: {broadcast}")))?;

    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_broadcast(true)?;
    // Ports 7 and 9 are both in common use; sending to both is harmless and
    // covers the case the Python version handled by trying one.
    for port in [9u16, 7] {
        socket.send_to(&packet, SocketAddr::new(target, port))?;
    }
    Ok(())
}

/// TCP connect with a timeout. Cheaper and more honest than ICMP, which needs
/// privileges, and than a ping subprocess, which needs a binary.
pub async fn tcp_reachable(host: &str, port: u16, timeout: Duration) -> bool {
    let addr = format!("{host}:{port}");
    matches!(
        tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// Run a command, argv style. Returns trimmed stdout on success.
pub async fn run(program: &str, args: &[&str], timeout: Duration) -> Result<String, NetError> {
    let output = tokio::time::timeout(
        timeout,
        tokio::process::Command::new(program).args(args).output(),
    )
    .await
    .map_err(|_| NetError::Other(format!("{program} timed out")))??;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err.trim();
        Err(NetError::Other(if err.is_empty() {
            format!("{program} exited with {}", output.status)
        } else {
            err.chars().take(200).collect()
        }))
    }
}

/// Docker container names, validated rather than quoted.
///
/// The Python version allowed `[a-zA-Z0-9][a-zA-Z0-9_.-]*`; the same rule is
/// enforced here, and because every call site passes argv there is no second
/// line of defence to forget.
pub fn valid_container_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    name.len() <= 128 && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Container name -> running/exited, from one `docker ps` call.
pub async fn container_states() -> Result<std::collections::HashMap<String, String>, NetError> {
    let out = run(
        "docker",
        &["ps", "-a", "--format", "{{.Names}}:{{.State}}"],
        Duration::from_secs(8),
    )
    .await?;
    Ok(out
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(n, s)| (n.to_string(), s.to_ascii_lowercase()))
        .collect())
}

/// A container lifecycle action.
///
/// A closed enum, so the wire format can only ever name one of these three. The
/// command is a fixed argv vector and the container name is validated before it
/// is used, which together mean there is no caller-supplied string that can turn
/// into shell syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerAction {
    Start,
    Stop,
    Restart,
}

impl ContainerAction {
    pub fn verb(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

/// Run a container action against the local Docker daemon.
pub async fn container_action(name: &str, action: ContainerAction) -> Result<(), NetError> {
    if !valid_container_name(name) {
        return Err(NetError::Other(format!("invalid container name: {name}")));
    }
    run(
        "docker",
        &[action.verb(), name],
        // `stop` waits for the container to exit, so it needs more room than a
        // simple start.
        Duration::from_secs(45),
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_parsing_accepts_common_formats() {
        let expected = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        for s in [
            "aa:bb:cc:dd:ee:ff",
            "AA:BB:CC:DD:EE:FF",
            "aa-bb-cc-dd-ee-ff",
            "aabbccddeeff",
            "aa:bb:cc:dd:ee:ff ",
        ] {
            assert_eq!(parse_mac(s).unwrap(), expected, "{s}");
        }
    }

    #[test]
    fn mac_parsing_rejects_junk() {
        for s in [
            "",
            "zz:bb:cc:dd:ee:ff",
            "aa:bb:cc:dd:ee",
            "aa:bb:cc:dd:ee:ff:00",
            "not a mac",
        ] {
            assert!(parse_mac(s).is_err(), "{s:?} should be rejected");
        }
    }

    #[test]
    fn magic_packet_is_102_bytes_and_correct() {
        let mac = [1, 2, 3, 4, 5, 6];
        let p = magic_packet(mac);
        assert_eq!(p.len(), 102);
        assert_eq!(&p[..6], &[0xFF; 6]);
        for i in 0..16 {
            assert_eq!(&p[6 + i * 6..12 + i * 6], &mac[..]);
        }
    }

    #[test]
    fn wake_rejects_a_bad_broadcast() {
        assert!(wake_on_lan("aa:bb:cc:dd:ee:ff", "not-an-ip").is_err());
    }

    #[test]
    fn wake_rejects_a_bad_mac() {
        assert!(wake_on_lan("nope", "255.255.255.255").is_err());
    }

    #[test]
    fn container_names_follow_docker_rules() {
        for good in ["plex", "my-app_1", "a", "A1.b-c_d"] {
            assert!(valid_container_name(good), "{good} should be valid");
        }
        for bad in [
            "",
            "-leading",
            ".leading",
            "has space",
            "semi;colon",
            "dollar$sign",
            "back`tick",
            "new\nline",
            "slash/name",
            "../../etc/passwd",
            "$(whoami)",
            ";rm -rf /",
        ] {
            assert!(!valid_container_name(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn container_names_are_length_bounded() {
        assert!(valid_container_name(&"a".repeat(128)));
        assert!(!valid_container_name(&"a".repeat(129)));
    }

    #[tokio::test]
    async fn tcp_reachable_reports_closed_ports_quickly() {
        // Port 1 on loopback should refuse immediately.
        assert!(!tcp_reachable("127.0.0.1", 1, Duration::from_millis(500)).await);
    }

    #[tokio::test]
    async fn run_reports_a_missing_binary_without_panicking() {
        let err = run(
            "mindash-definitely-not-a-real-binary",
            &[],
            Duration::from_secs(2),
        )
        .await;
        assert!(err.is_err());
    }

    #[test]
    fn container_actions_map_to_the_right_verbs() {
        assert_eq!(ContainerAction::Start.verb(), "start");
        assert_eq!(ContainerAction::Stop.verb(), "stop");
        assert_eq!(ContainerAction::Restart.verb(), "restart");
    }

    #[test]
    fn container_actions_deserialise_from_lowercase_only() {
        for (json, want) in [
            ("\"start\"", ContainerAction::Start),
            ("\"stop\"", ContainerAction::Stop),
            ("\"restart\"", ContainerAction::Restart),
        ] {
            assert_eq!(serde_json::from_str::<ContainerAction>(json).unwrap(), want);
        }
        // Arbitrary commands are not expressible.
        for bad in ["\"rm\"", "\"exec\"", "\"start; rm -rf /\"", "\"START\""] {
            assert!(
                serde_json::from_str::<ContainerAction>(bad).is_err(),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn container_action_rejects_a_hostile_name_before_running_anything() {
        for bad in ["a; rm -rf /", "$(id)", "a b", "", "../x"] {
            let err = container_action(bad, ContainerAction::Restart).await;
            assert!(err.is_err(), "{bad:?} must be rejected");
            // The rejection is the name check, not a docker failure.
            let msg = err.unwrap_err().to_string();
            assert!(msg.contains("invalid container name"), "{msg}");
        }
    }
}
