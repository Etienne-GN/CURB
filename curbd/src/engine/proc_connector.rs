//! Real-time process-exec notifications via the Linux cn_proc Netlink connector.
//!
//! The kernel broadcasts a `PROC_EVENT_EXEC` message on every `execve()` call.
//! We subscribe to that stream so new processes are placed in their cgroup
//! within milliseconds rather than up to one 1-second polling interval later.
//!
//! Falls back gracefully when the connector is unavailable (missing
//! `CONFIG_PROC_EVENTS`, insufficient permissions, etc.): the caller then
//! keeps polling on a timer.

use std::mem;
use std::sync::mpsc;
use tracing::{debug, warn};

const NETLINK_CONNECTOR: libc::c_int = 11;
const CN_IDX_PROC: u32 = 1;
const CN_VAL_PROC: u32 = 1;
const NLMSG_NOOP: u16 = 1;
const PROC_CN_MCAST_LISTEN: u32 = 1;
const PROC_EVENT_EXEC: u32 = 0x0000_0002;

// ── Kernel struct layout (must match <linux/netlink.h> / <linux/cn_proc.h>) ──

#[repr(C)]
struct NlMsgHdr {
    nlmsg_len:   u32,
    nlmsg_type:  u16,
    nlmsg_flags: u16,
    nlmsg_seq:   u32,
    nlmsg_pid:   u32,
}

#[repr(C)]
struct CnMsg {
    idx:   u32, // cb_id.idx
    val:   u32, // cb_id.val
    seq:   u32,
    ack:   u32,
    len:   u16,
    flags: u16,
}

// proc_event layout: what(u32) + cpu(u32) + timestamp_ns(u64) + union
// For PROC_EVENT_EXEC the union starts with: process_pid(u32) + process_tgid(u32)
const EVT_HDR_SZ: usize = 4 + 4 + 8; // what + cpu + timestamp_ns
const NL_HDR_SZ:  usize = mem::size_of::<NlMsgHdr>(); // 16
const CN_SZ:      usize = mem::size_of::<CnMsg>();     // 20

/// Spawn a background thread listening for `PROC_EVENT_EXEC` notifications.
///
/// Returns `Some(rx)` where `rx` yields the PID of each newly exec'd process.
/// Returns `None` if the Netlink connector is unavailable; caller falls back
/// to polling.
pub fn spawn() -> Option<mpsc::Receiver<u32>> {
    let sock = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            NETLINK_CONNECTOR,
        )
    };
    if sock < 0 {
        warn!(
            error = %std::io::Error::last_os_error(),
            "proc-connector: socket() failed — falling back to polling"
        );
        return None;
    }

    let mut addr: libc::sockaddr_nl = unsafe { mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as u16;
    addr.nl_pid = 0;
    addr.nl_groups = CN_IDX_PROC;
    let rc = unsafe {
        libc::bind(
            sock,
            &addr as *const _ as *const libc::sockaddr,
            mem::size_of_val(&addr) as libc::socklen_t,
        )
    };
    if rc < 0 {
        warn!(
            error = %std::io::Error::last_os_error(),
            "proc-connector: bind() failed — falling back to polling"
        );
        unsafe { libc::close(sock) };
        return None;
    }

    if !subscribe(sock) {
        unsafe { libc::close(sock) };
        return None;
    }

    let (tx, rx) = mpsc::channel::<u32>();
    std::thread::Builder::new()
        .name("curb-procwatch".into())
        .spawn(move || {
            let mut buf = vec![0u8; 4096];
            loop {
                let n = unsafe {
                    libc::recv(
                        sock,
                        buf.as_mut_ptr() as *mut libc::c_void,
                        buf.len(),
                        0,
                    )
                };
                if n <= 0 {
                    break;
                }
                parse_events(&buf[..n as usize], &tx);
            }
            unsafe { libc::close(sock) };
        })
        .ok();

    Some(rx)
}

/// Send `PROC_CN_MCAST_LISTEN` to subscribe to process events.
fn subscribe(sock: libc::c_int) -> bool {
    let data_len = mem::size_of::<u32>() as u16;
    let total = NL_HDR_SZ + CN_SZ + data_len as usize;
    let mut buf = vec![0u8; total];

    // nlmsghdr
    let hdr = buf.as_mut_ptr() as *mut NlMsgHdr;
    unsafe {
        (*hdr).nlmsg_len   = total as u32;
        (*hdr).nlmsg_type  = NLMSG_NOOP;
        (*hdr).nlmsg_flags = 0;
        (*hdr).nlmsg_seq   = 0;
        (*hdr).nlmsg_pid   = libc::getpid() as u32;
    }

    // cn_msg
    let cn = unsafe { (buf.as_mut_ptr().add(NL_HDR_SZ)) as *mut CnMsg };
    unsafe {
        (*cn).idx   = CN_IDX_PROC;
        (*cn).val   = CN_VAL_PROC;
        (*cn).seq   = 0;
        (*cn).ack   = 0;
        (*cn).len   = data_len;
        (*cn).flags = 0;
    }

    // mcast op
    let op = unsafe { buf.as_mut_ptr().add(NL_HDR_SZ + CN_SZ) as *mut u32 };
    unsafe { *op = PROC_CN_MCAST_LISTEN };

    let mut dest: libc::sockaddr_nl = unsafe { mem::zeroed() };
    dest.nl_family = libc::AF_NETLINK as u16;
    let rc = unsafe {
        libc::sendto(
            sock,
            buf.as_ptr() as *const libc::c_void,
            total,
            0,
            &dest as *const _ as *const libc::sockaddr,
            mem::size_of_val(&dest) as libc::socklen_t,
        )
    };
    if rc < 0 {
        warn!(
            error = %std::io::Error::last_os_error(),
            "proc-connector: sendto() failed — falling back to polling"
        );
        return false;
    }
    true
}

/// Extract PIDs from a received Netlink buffer. A single recv() can carry
/// multiple nlmsghdr-framed messages.
fn parse_events(buf: &[u8], tx: &mpsc::Sender<u32>) {
    let min_len = NL_HDR_SZ + CN_SZ + EVT_HDR_SZ + 8; // +8 for exec pid+tgid
    let mut pos = 0;

    while pos + min_len <= buf.len() {
        // nlmsg_len tells us where the next message starts.
        let nlmsg_len = u32::from_ne_bytes(buf[pos..pos + 4].try_into().unwrap_or([0; 4])) as usize;
        if nlmsg_len < min_len || pos + nlmsg_len > buf.len() {
            break;
        }

        let after_cn = &buf[pos + NL_HDR_SZ + CN_SZ..];
        let what = u32::from_ne_bytes(after_cn[..4].try_into().unwrap_or([0; 4]));
        if what == PROC_EVENT_EXEC {
            // skip what(4) + cpu(4) + timestamp_ns(8) = 16 bytes
            let pid = u32::from_ne_bytes(
                after_cn[EVT_HDR_SZ..EVT_HDR_SZ + 4]
                    .try_into()
                    .unwrap_or([0; 4]),
            );
            debug!(pid, "proc-connector: exec");
            let _ = tx.send(pid);
        }

        // Advance to the next message (NLMSG_ALIGN to 4 bytes).
        let aligned = (nlmsg_len + 3) & !3;
        pos += aligned.max(min_len);
    }
}
