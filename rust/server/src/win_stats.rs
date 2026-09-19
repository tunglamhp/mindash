//! Windows host statistics.
//!
//! The POSIX collector is a shell script, and Windows has no `sh`. Rather than
//! require an sshd on the machine MinDash itself runs on, this reads the same values
//! natively and prints them in the identical `key=value` / `DISKS` shape so
//! [`crate::exec::parse_stats`] handles both platforms with one parser.
//!
//! Values are emitted already-reduced to the units the POSIX path produces:
//! memory in kB (as `/proc/meminfo` reports), disk sizes in bytes, uptime in
//! seconds.

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// A PowerShell script that prints the stats block.
///
/// Deliberately avoids the slow WMI providers. `Win32_PerfFormattedData_*` and
/// `MSAcpi_ThermalZoneTemperature` each cost several seconds on a real machine,
/// and together they made a stats request take ~20 s — long enough that the UI
/// looked broken. CPU load and temperature are therefore reported as absent on
/// Windows rather than making every other figure wait for them; the parser
/// already renders a missing value as `—` instead of a misleading zero.
const SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'

# Processor count straight from the environment: no WMI query needed.
$cores = [int]$env:NUMBER_OF_PROCESSORS
if ($cores -lt 1) { $cores = 1 }

# Memory, in kB, to match /proc/meminfo which the shared parser expects.
$os = Get-CimInstance -ClassName Win32_OperatingSystem
$totalKb = [long]($os.TotalVisibleMemorySize)
$freeKb  = [long]($os.FreePhysicalMemory)

# Uptime in seconds.
$up = 0
try { $up = [long]((Get-Date) - $os.LastBootUpTime).TotalSeconds } catch { }

# CPU load is not collected: the performance-counter provider that reports it is
# the single slowest call available here. Reported empty so the UI shows "—".
"cpu="
"mem_total=$totalKb"
"mem_avail=$freeKb"
"temp="
"uptime=$up"
"cores=$cores"
"DISKS"

# Collected first, then emitted. A query piped straight into ForEach-Object
# truncated the output entirely on a real machine, which silently dropped every
# disk from the payload.
$disks = @(Get-CimInstance -ClassName Win32_LogicalDisk -Filter 'DriveType=3')
foreach ($d in $disks) {
  $size = [long]$d.Size
  if ($size -gt 0) {
    $free = [long]$d.FreeSpace
    "$($d.DeviceID)`t$($d.DeviceID)\`t$size`t$($size - $free)"
  }
}
"#;

/// Collect host statistics natively on Windows.
pub async fn collect() -> Result<String, String> {
    let output = tokio::time::timeout(
        // Performance-counter reads can stall on a busy or freshly booted
        // machine, so this gets more room than the POSIX path.
        Duration::from_secs(20),
        Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| "windows stats timed out".to_string())
    .and_then(|r| r.map_err(|e| e.to_string()))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "windows stats failed: {}",
            err.trim().chars().take(200).collect::<String>()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The output has to fit the shape the shared parser expects. This checks the
    /// contract without depending on PowerShell being present.
    #[test]
    fn the_script_emits_the_shared_keys() {
        for key in [
            "cpu=",
            "mem_total=",
            "mem_avail=",
            "temp=",
            "uptime=",
            "cores=",
        ] {
            assert!(SCRIPT.contains(key), "missing {key}");
        }
        // The DISKS sentinel and a tab-separated row are what parse_stats needs.
        assert!(SCRIPT.contains("DISKS"));
        assert!(SCRIPT.contains("`t"), "rows must be tab separated");
    }

    #[test]
    fn memory_is_reported_in_kilobytes_like_proc_meminfo() {
        // Win32_OperatingSystem reports kB, which is what the parser multiplies
        // by 1024. If this ever became bytes the RAM figure would be 1024x out.
        assert!(SCRIPT.contains("TotalVisibleMemorySize"));
        assert!(!SCRIPT.contains("TotalVisibleMemorySize) * 1024"));
    }

    #[tokio::test]
    async fn collect_returns_parseable_output_or_a_clear_error() {
        // On a non-Windows host there is no powershell, which must be an error
        // rather than a panic or a silently empty payload.
        match collect().await {
            Ok(raw) => {
                // Assert on what `parse_stats` actually emits. `cores` is
                // consumed to turn the load figure into a percentage and is not
                // part of the output, so asserting on it fails even when the
                // collector is perfectly correct.
                let parsed = crate::exec::parse_stats(&raw);
                assert!(
                    parsed["ram_total"].as_u64().unwrap_or(0) > 0,
                    "no RAM in {raw}"
                );
                assert!(parsed["uptime"].is_string(), "no uptime in {raw}");
                assert!(
                    parsed["cpu"]
                        .as_i64()
                        .is_some_and(|c| (0..=100).contains(&c)),
                    "cpu out of range in {raw}"
                );
            }
            Err(e) => assert!(!e.is_empty(), "an error must explain itself"),
        }
    }
}
