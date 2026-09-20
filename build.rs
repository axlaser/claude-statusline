//! Build script: one linker flag, for one measured reason.
//!
//! The click path calls into `user32.dll` — window enumeration at capture,
//! the foreground recipe in the helper — and nothing else in the binary does.
//! Linked normally, the loader maps and initialises that DLL in every process
//! the binary starts, the status-line tick included, and the paired
//! measurement in `docs/performance.md` §7 put that at ~1.8 ms on a ~11 ms
//! tick. Delay-loading it defers the load to the first call into it, which
//! only a visual-alert capture or a click handler ever makes, so the tick pays
//! nothing for a feature it never uses.
//!
//! MSVC only: `/DELAYLOAD` is a Microsoft linker option and `delayimp.lib`
//! supplies its helper. The GNU toolchain has no equivalent and is not a
//! published target.

fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "msvc" {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:user32.dll");
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
