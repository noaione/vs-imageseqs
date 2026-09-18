//! Finds the native library the plugin links against.
//!
//! webp is decoded by libwebp instead of by the pure rust decoder in `image`
//! (see `src/formats/webp.rs`), so the library has to be located at build time.
//! Windows builds take it from the vcpkg tree the other native dependencies
//! already come from; everywhere else the libwebp development files are
//! expected to be installed where `pkg-config` finds them.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    vcpkg::find_package("libwebp").expect(
        "libwebp is not installed for the target triplet; run the vcpkg install in AGENTS.md",
    );

    #[cfg(not(windows))]
    pkg_config::Config::new()
        .atleast_version("1.2.0")
        .probe("libwebp")
        .expect("libwebp development files are not installed");
}
