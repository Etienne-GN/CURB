//! Compiles the eBPF classifier (`bpf/curb_cls.bpf.c`) to a BPF ELF object with
//! clang and exposes its path to the crate via `CURB_BPF_OBJ` for `include_bytes!`.
//!
//! If clang is unavailable, the build still succeeds but `CURB_BPF_OBJ` points
//! at an empty object; the daemon detects the empty program and falls back to
//! nftables policing at runtime.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let src = "bpf/curb_cls.bpf.c";
    println!("cargo:rerun-if-changed={src}");
    println!("cargo:rerun-if-changed=build.rs");

    let obj = out.join("curb_cls.bpf.o");
    // Debian multiarch include dir for <asm/types.h>, etc.
    let arch_inc = format!("/usr/include/{}-linux-gnu", std::env::consts::ARCH);

    let result = Command::new("clang")
        .args([
            "-O2",
            "-g",
            "-Wall",
            "-target",
            "bpf",
            "-I",
            "/usr/include",
            "-I",
            &arch_inc,
            "-c",
            src,
            "-o",
        ])
        .arg(&obj)
        .status();

    match result {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("clang failed to build eBPF object (exit {status})"),
        Err(e) => {
            // clang missing: emit an empty file so include_bytes! still works.
            println!("cargo:warning=clang unavailable ({e}); eBPF shaping disabled, will fall back to nftables policing");
            std::fs::write(&obj, []).unwrap();
        }
    }

    println!("cargo:rustc-env=CURB_BPF_OBJ={}", obj.display());
}
