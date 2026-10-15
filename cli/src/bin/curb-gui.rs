//! `curb-gui` launcher — installed setgid `curb`, execs the real Tauri GUI.
//!
//! GTK aborts when real GID != effective GID, so the GUI binary itself cannot
//! be setgid. This launcher collapses real/effective/saved GID to `curb` before
//! exec'ing the GUI, so the socket is reachable without a logout/login after
//! the user is added to the group (and without `sg`, which some distros lack).
//!
//! The GUI is resolved relative to this binary: `<prefix>/bin/curb-gui` →
//! `<prefix>/lib/curb/curb-gui`. No env override — this binary is setgid.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{exit, Command};

extern "C" {
    fn getgid() -> u32;
    fn getegid() -> u32;
    fn setresgid(rgid: u32, egid: u32, sgid: u32) -> i32;
}

fn main() {
    let (rgid, egid) = unsafe { (getgid(), getegid()) };
    if rgid != egid {
        // Unprivileged processes may set any of the three IDs to the current
        // effective GID; afterwards GTK sees a consistent, non-setgid process.
        if unsafe { setresgid(egid, egid, egid) } != 0 {
            eprintln!("curb-gui: setresgid failed: {}", std::io::Error::last_os_error());
            exit(1);
        }
    }

    let gui = match gui_path() {
        Some(p) => p,
        None => {
            eprintln!("curb-gui: cannot locate GUI binary (expected <prefix>/lib/curb/curb-gui)");
            exit(1);
        }
    };

    let err = Command::new(&gui).args(std::env::args_os().skip(1)).exec();
    eprintln!("curb-gui: failed to exec {}: {err}", gui.display());
    exit(1);
}

fn gui_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let prefix = exe.parent()?.parent()?;
    let gui = prefix.join("lib/curb/curb-gui");
    gui.is_file().then_some(gui)
}
