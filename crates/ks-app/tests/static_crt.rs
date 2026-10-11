//! `ks.exe` is a single static executable. Cargo ignores
//! `[target.*] rustflags` from `.cargo/config.toml` when the `RUSTFLAGS` environment variable is
//! set, which would silently relink the C runtime dynamically; this fails loudly instead.

#[cfg(all(windows, target_env = "msvc"))]
#[test]
fn the_msvc_crt_is_linked_statically() {
    let crt_static = cfg!(target_feature = "crt-static");
    assert!(
        crt_static,
        "built without +crt-static: if RUSTFLAGS is set, add `-C target-feature=+crt-static` to it"
    );
}
