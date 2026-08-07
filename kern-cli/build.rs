//! Copies the `llama-server` bundle (binary + shared libs), prepared by
//! the CI release for the target platform, into `OUT_DIR` — from where
//! `src/embedded.rs` embeds it via `include_bytes!` when the
//! `bundled-llama-server` feature is enabled.
//!
//! Normal builds (dev, PR CI — without the feature) don't set
//! `KERN_LLAMA_SERVER_ARCHIVE` and this script does nothing.

fn main() {
    println!("cargo:rerun-if-env-changed=KERN_LLAMA_SERVER_ARCHIVE");

    if std::env::var_os("CARGO_FEATURE_BUNDLED_LLAMA_SERVER").is_none() {
        return;
    }

    let archive_path = std::env::var("KERN_LLAMA_SERVER_ARCHIVE").unwrap_or_else(|_| {
        panic!(
            "bundled-llama-server feature is enabled but KERN_LLAMA_SERVER_ARCHIVE is not \
             set — the CI release must point to the tar.gz prepared with llama-server + \
             shared libs for this platform (see .github/workflows/release.yml)"
        )
    });

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is always set by cargo");
    let dest = std::path::Path::new(&out_dir).join("llama-server-bundle.tar.gz");
    std::fs::copy(&archive_path, &dest).unwrap_or_else(|e| {
        panic!(
            "failed to copy KERN_LLAMA_SERVER_ARCHIVE={archive_path} to {}: {e}",
            dest.display()
        )
    });

    println!("cargo:rerun-if-changed={archive_path}");
}
