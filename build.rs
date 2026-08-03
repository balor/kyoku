//! Windows-only build step: embed the application manifest declaring
//! `longPathAware` (see `packaging/windows/kyoku.manifest`).
//!
//! Music trees with verbose CJK album titles can exceed the legacy
//! 260-char MAX_PATH limit. The manifest alone changes nothing — it only
//! *allows* long paths on machines where the user (or their admin) has
//! set `LongPathsEnabled=1` / the "Enable Win32 long paths" GPO. Harmless
//! everywhere else. No-op on non-Windows targets, including
//! cross-compiles from Linux/macOS where the target gate below keeps
//! `embed-resource` from running.

fn main() {
    // `compile` is a no-op on non-Windows hosts that emits zero cargo
    // directives… except it still verifies tooling — so gate on target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Emits `cargo:rustc-link-arg-bins=<resource.o>` itself.
        // Result is `Ok` on MSVC targets and `NotImplemented` where no
        // usable resource compiler was found — non-fatal by design (the
        // exe just lacks the manifest then; behavior is unchanged).
        let _ = embed_resource::compile("packaging/windows/kyoku.rc", embed_resource::NONE);
    }
    // Re-run if the manifest or rc changes even when the target isn't
    // windows (so a later windows cross-build picks it up).
    println!("cargo:rerun-if-changed=packaging/windows/kyoku.manifest");
    println!("cargo:rerun-if-changed=packaging/windows/kyoku.rc");
}
