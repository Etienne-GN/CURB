//! Raw packet capture via `AF_PACKET` and lightweight header parsing.
//!
//! We open one `AF_PACKET`/`SOCK_RAW` socket (no libpcap dependency) and read
//! every frame. The kernel tags each frame's direction for us via
//! `sll_pkttype` (`PACKET_OUTGOING` ⇒ we sent it), so we don't need to compare
//! against local interface addresses. Each parsed frame becomes a [`Sample`]
//! the aggregator attributes to an application.
//!
//! Requires `CAP_NET_RAW` (run `curbd` as root or grant the capability).

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use super::procmap::Proto;

/// `linux/if_packet.h`: the frame was sent by this host.
const PACKET_OUTGOING: u8 = 4;
/// `ETH_P_ALL` — capture every ethertype.
const ETH_P_ALL: u16 = 0x0003;
const ETH_HEADER_LEN: usize = 14;

/// One parsed packet, normalized to local/remote endpoints.
pub struct Sample {
    pub proto: Proto,
    pub local_ip: IpAddr,
    pub local_port: u16,
    pub rem_ip: IpAddr,
    pub rem_port: u16,
    /// True if this host sent the packet (upload), false if it received it.
    pub outgoing: bool,
    /// On-wire IP bytes (IP total length), used for accounting.
    pub bytes: u64,
}

/// Open a raw `AF_PACKET` capture socket bound to all interfaces.
pub fn open_socket() -> io::Result<OwnedFd> {
    // The protocol field must be the ethertype in network byte order.
    let proto = (ETH_P_ALL.to_be()) as libc::c_int;
    let fd = unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_RAW | libc::SOCK_CLOEXEC, proto) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Block until one frame arrives. Returns its length and whether it was outgoing.
pub fn recv(fd: &OwnedFd, buf: &mut [u8]) -> io::Result<(usize, bool)> {
    let mut addr: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
    let mut addrlen = std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
    loop {
        let n = unsafe {
            libc::recvfrom(
                fd.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
                &mut addr as *mut _ as *mut libc::sockaddr,
                &mut addrlen,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        let outgoing = addr.sll_pkttype == PACKET_OUTGOING;
        return Ok((n as usize, outgoing));
    }
}

/// Parse an Ethernet frame into a [`Sample`], or `None` if it isn't an
/// attributable IPv4/IPv6 TCP/UDP packet (loopback is dropped to avoid double
/// counting localhost traffic).
pub fn parse_frame(buf: &[u8], outgoing: bool) -> Option<Sample> {
    if buf.len() < ETH_HEADER_LEN {
        return None;
    }
    let ethertype = u16::from_be_bytes([buf[12], buf[13]]);
    let l3 = &buf[ETH_HEADER_LEN..];
    match ethertype {
        0x0800 => parse_ipv4(l3, outgoing),
        0x86DD => parse_ipv6(l3, outgoing),
        _ => None,
    }
}

fn parse_ipv4(d: &[u8], outgoing: bool) -> Option<Sample> {
    if d.len() < 20 {
        return None;
    }
    let ihl = (d[0] & 0x0f) as usize * 4;
    if ihl < 20 || d.len() < ihl {
        return None;
    }
    let total_len = u16::from_be_bytes([d[2], d[3]]) as u64;
    let src = Ipv4Addr::new(d[12], d[13], d[14], d[15]);
    let dst = Ipv4Addr::new(d[16], d[17], d[18], d[19]);
    if src.is_loopback() || dst.is_loopback() {
        return None;
    }
    let proto = transport(d[9])?;
    let (sport, dport) = ports(&d[ihl..])?;
    Some(orient(
        proto,
        IpAddr::V4(src),
        sport,
        IpAddr::V4(dst),
        dport,
        outgoing,
        total_len,
    ))
}

fn parse_ipv6(d: &[u8], outgoing: bool) -> Option<Sample> {
    if d.len() < 40 {
        return None;
    }
    // First next-header only; extension-header chains are skipped for now.
    let proto = transport(d[6])?;
    let payload_len = u16::from_be_bytes([d[4], d[5]]) as u64;
    let src = Ipv6Addr::from(<[u8; 16]>::try_from(&d[8..24]).ok()?);
    let dst = Ipv6Addr::from(<[u8; 16]>::try_from(&d[24..40]).ok()?);
    if src.is_loopback() || dst.is_loopback() {
        return None;
    }
    let (sport, dport) = ports(&d[40..])?;
    Some(orient(
        proto,
        IpAddr::V6(src),
        sport,
        IpAddr::V6(dst),
        dport,
        outgoing,
        40 + payload_len,
    ))
}

/// Map an IP protocol number to the transports we account for.
fn transport(proto: u8) -> Option<Proto> {
    match proto {
        6 => Some(Proto::Tcp),
        17 => Some(Proto::Udp),
        _ => None,
    }
}

/// Read source/destination ports from the start of an L4 header.
fn ports(l4: &[u8]) -> Option<(u16, u16)> {
    if l4.len() < 4 {
        return None;
    }
    Some((
        u16::from_be_bytes([l4[0], l4[1]]),
        u16::from_be_bytes([l4[2], l4[3]]),
    ))
}

/// Orient a packet's endpoints into local/remote based on its direction.
#[allow(clippy::too_many_arguments)]
fn orient(
    proto: Proto,
    src: IpAddr,
    sport: u16,
    dst: IpAddr,
    dport: u16,
    outgoing: bool,
    bytes: u64,
) -> Sample {
    let (local_ip, local_port, rem_ip, rem_port) = if outgoing {
        (src, sport, dst, dport)
    } else {
        (dst, dport, src, sport)
    };
    Sample {
        proto,
        local_ip,
        local_port,
        rem_ip,
        rem_port,
        outgoing,
        bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an Ethernet+IPv4+TCP frame for testing the parser.
    fn ipv4_tcp_frame(src: [u8; 4], dst: [u8; 4], sport: u16, dport: u16, total: u16) -> Vec<u8> {
        let mut f = vec![0u8; ETH_HEADER_LEN];
        f[12] = 0x08; // IPv4 ethertype
        f[13] = 0x00;
        let mut ip = vec![0u8; 20];
        ip[0] = 0x45; // version 4, ihl 5
        ip[2..4].copy_from_slice(&total.to_be_bytes());
        ip[9] = 6; // TCP
        ip[12..16].copy_from_slice(&src);
        ip[16..20].copy_from_slice(&dst);
        let mut l4 = vec![0u8; 4];
        l4[0..2].copy_from_slice(&sport.to_be_bytes());
        l4[2..4].copy_from_slice(&dport.to_be_bytes());
        f.extend_from_slice(&ip);
        f.extend_from_slice(&l4);
        f
    }

    #[test]
    fn outgoing_orients_local_to_source() {
        let frame = ipv4_tcp_frame([192, 168, 0, 5], [1, 1, 1, 1], 50000, 443, 1500);
        let s = parse_frame(&frame, true).unwrap();
        assert_eq!(s.local_ip, IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5)));
        assert_eq!(s.local_port, 50000);
        assert_eq!(s.rem_port, 443);
        assert!(s.outgoing);
        assert_eq!(s.bytes, 1500);
    }

    #[test]
    fn incoming_orients_local_to_dest() {
        let frame = ipv4_tcp_frame([1, 1, 1, 1], [192, 168, 0, 5], 443, 50000, 1500);
        let s = parse_frame(&frame, false).unwrap();
        assert_eq!(s.local_ip, IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5)));
        assert_eq!(s.local_port, 50000);
        assert_eq!(s.rem_port, 443);
        assert!(!s.outgoing);
    }

    #[test]
    fn loopback_is_dropped() {
        let frame = ipv4_tcp_frame([127, 0, 0, 1], [127, 0, 0, 1], 1, 2, 100);
        assert!(parse_frame(&frame, true).is_none());
    }
}
