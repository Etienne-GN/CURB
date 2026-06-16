//! Flow → application attribution via `/proc`.
//!
//! Maps a network flow (the local socket endpoint) to the executable that owns
//! it, the same way `nethogs`/`ss` do: socket inode → pid → `/proc/<pid>/exe`.
//! Built fresh on an interval by the monitor and swapped in atomically.
//!
//! This is the P1 "fallback" attribution path from the plan; the eventual
//! cgroup+eBPF engine (P3) will attribute at the kernel without this scan.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Transport protocol of a flow.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Proto {
    Tcp,
    Udp,
}

/// The executable that owns a flow.
#[derive(Clone)]
pub struct Owner {
    pub exe: String,
    pub pid: u32,
}

/// A resolved snapshot of every attributable socket on the host.
#[derive(Default)]
pub struct ProcMap {
    /// Exact 4-tuple match (most precise).
    full: HashMap<(Proto, IpAddr, u16, IpAddr, u16), Owner>,
    /// Local endpoint only (handles unconnected UDP / wildcard remotes).
    local: HashMap<(Proto, IpAddr, u16), Owner>,
    /// Local port only (last-resort fallback).
    port: HashMap<(Proto, u16), Owner>,
    /// Distinct owning pids per executable, for the "PIDs" column.
    exe_pids: HashMap<String, u32>,
}

impl ProcMap {
    /// Resolve a flow to its owning executable, trying most-specific first.
    pub fn lookup(
        &self,
        proto: Proto,
        local_ip: IpAddr,
        local_port: u16,
        rem_ip: IpAddr,
        rem_port: u16,
    ) -> Option<&Owner> {
        self.full
            .get(&(proto, local_ip, local_port, rem_ip, rem_port))
            .or_else(|| self.local.get(&(proto, local_ip, local_port)))
            .or_else(|| self.port.get(&(proto, local_port)))
    }

    /// Number of distinct live pids attributed to an executable.
    pub fn pids_for(&self, exe: &str) -> u32 {
        self.exe_pids.get(exe).copied().unwrap_or(0)
    }

    /// Scan `/proc` and build a fresh attribution map.
    pub fn build() -> Self {
        let inodes = scan_socket_inodes();

        let mut pm = ProcMap::default();
        let mut pids_by_exe: HashMap<String, HashSet<u32>> = HashMap::new();
        for owner in inodes.values() {
            pids_by_exe
                .entry(owner.exe.clone())
                .or_default()
                .insert(owner.pid);
        }
        pm.exe_pids = pids_by_exe
            .into_iter()
            .map(|(exe, pids)| (exe, pids.len() as u32))
            .collect();

        for (proto, path) in [
            (Proto::Tcp, "/proc/net/tcp"),
            (Proto::Tcp, "/proc/net/tcp6"),
            (Proto::Udp, "/proc/net/udp"),
            (Proto::Udp, "/proc/net/udp6"),
        ] {
            parse_net_file(path, proto, &inodes, &mut pm);
        }

        pm
    }
}

/// Build `inode -> Owner` by walking every process's open socket fds.
fn scan_socket_inodes() -> HashMap<u64, Owner> {
    let mut map = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return map;
    };
    for ent in entries.flatten() {
        let Some(pid) = ent
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue; // process gone or not permitted
        };
        let mut exe: Option<String> = None;
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            let Some(inode) = target
                .to_str()
                .and_then(|t| t.strip_prefix("socket:["))
                .and_then(|t| t.strip_suffix(']'))
                .and_then(|t| t.parse::<u64>().ok())
            else {
                continue;
            };
            // Resolve the exe lazily, only for processes that actually hold sockets.
            let exe = exe.get_or_insert_with(|| read_exe(pid));
            if exe.is_empty() {
                continue; // kernel thread or unreadable
            }
            map.entry(inode).or_insert_with(|| Owner {
                exe: exe.clone(),
                pid,
            });
        }
    }
    map
}

fn read_exe(pid: u32) -> String {
    let raw = fs::read_link(format!("/proc/{pid}/exe"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    // The kernel appends " (deleted)" when the on-disk binary was replaced
    // (e.g. an upgrade) while the process keeps running; strip it so the app
    // identity stays stable.
    raw.strip_suffix(" (deleted)")
        .map(str::to_string)
        .unwrap_or(raw)
}

/// Parse one `/proc/net/{tcp,tcp6,udp,udp6}` table into the attribution maps.
fn parse_net_file(path: &str, proto: Proto, inodes: &HashMap<u64, Owner>, pm: &mut ProcMap) {
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    for line in content.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        // sl local_address rem_address st tx:rx tr:when retrnsmt uid timeout inode ...
        if f.len() < 10 {
            continue;
        }
        let Some((lip, lport)) = parse_hex_endpoint(f[1]) else {
            continue;
        };
        let Some((rip, rport)) = parse_hex_endpoint(f[2]) else {
            continue;
        };
        let Ok(inode) = f[9].parse::<u64>() else {
            continue;
        };
        if inode == 0 {
            continue;
        }
        let Some(owner) = inodes.get(&inode) else {
            continue;
        };
        pm.full
            .insert((proto, lip, lport, rip, rport), owner.clone());
        pm.local
            .entry((proto, lip, lport))
            .or_insert_with(|| owner.clone());
        pm.port
            .entry((proto, lport))
            .or_insert_with(|| owner.clone());
    }
}

/// Parse a `/proc/net` `ADDR:PORT` hex endpoint (IPv4 = 8 hex, IPv6 = 32 hex).
///
/// The kernel prints each 32-bit word in host order, so on little-endian hosts
/// the bytes need a per-word little-endian reinterpretation.
fn parse_hex_endpoint(s: &str) -> Option<(IpAddr, u16)> {
    let (addr, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    match addr.len() {
        8 => {
            let word = u32::from_str_radix(addr, 16).ok()?;
            Some((IpAddr::V4(Ipv4Addr::from(word.to_le_bytes())), port))
        }
        32 => {
            let mut bytes = [0u8; 16];
            for w in 0..4 {
                let word = u32::from_str_radix(&addr[w * 8..w * 8 + 8], 16).ok()?;
                bytes[w * 4..w * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
            Some((IpAddr::V6(Ipv6Addr::from(bytes)), port))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_endpoint_little_endian() {
        // 0100007F = 127.0.0.1 in /proc's little-endian word form; 0050 = 80.
        let (ip, port) = parse_hex_endpoint("0100007F:0050").unwrap();
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
        assert_eq!(port, 80);
    }

    #[test]
    fn parses_ipv6_loopback() {
        let (ip, port) = parse_hex_endpoint(
            "00000000000000000000000001000000:1F90",
        )
        .unwrap();
        assert_eq!(ip, IpAddr::V6(Ipv6Addr::LOCALHOST));
        assert_eq!(port, 8080);
    }
}
