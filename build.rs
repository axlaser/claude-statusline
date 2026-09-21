//! Build script: delay-load `user32.dll` on MSVC.
//!
//! Only the click path calls into `user32.dll`, yet linked normally the loader
//! initialises it in every process, the tick included: ~1.8 ms on a ~11 ms
//! tick by the paired measurement in `docs/performance.md` §7. Delay-loading
//! defers that to the first call, which only a capture or a click ever makes.
//! MSVC only: `/DELAYLOAD` and `delayimp.lib` are Microsoft's; the GNU
//! toolchain has no equivalent and is not a published target.

fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "msvc" {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:user32.dll");
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
