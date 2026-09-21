//! Build script: delay-load the notification and click DLLs on MSVC.
//!
//! Only the click path calls into `user32.dll` and only an alert calls
//! `PlaySoundW` in `winmm.dll`, yet linked normally the loader initialises both
//! in every process, the tick included: ~1.8 ms on a ~11 ms tick for `user32`
//! alone by the paired measurement in `docs/performance.md` §7. Delay-loading
//! defers that to the first call, which only a capture, a click or an alert
//! ever makes. MSVC only: `/DELAYLOAD` and `delayimp.lib` are Microsoft's; the
//! GNU toolchain has no equivalent and is not a published target.
//!
//! **`gdi32` is deliberately absent.** `Win32_Graphics_Gdi` is in the feature
//! list only because `WNDCLASSW` carries a brush handle, which the crate sets
//! to null; no gdi32 entry point is called anywhere, so there is no import to
//! defer and the linker would ignore the directive. The import table confirms
//! it: `gdi32.dll` does not appear in the binary's dependents at all.

/// The DLLs whose loader cost no per-tick path should pay.
const DELAY_LOADED: [&str; 2] = ["user32.dll", "winmm.dll"];

fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "msvc" {
        for dll in DELAY_LOADED {
            println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
        }
        println!("cargo:rustc-link-arg-bins=delayimp.lib");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
