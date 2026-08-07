//! Copia o pacote `llama-server` (binário + libs compartilhadas), preparado
//! pela release CI para a plataforma alvo, pra dentro de `OUT_DIR` — de onde
//! `src/embedded.rs` o embute via `include_bytes!` quando a feature
//! `bundled-llama-server` está ativa.
//!
//! Builds normais (dev, CI de PR — sem a feature) não setam
//! `KERN_LLAMA_SERVER_ARCHIVE` e este script não faz nada.

fn main() {
    println!("cargo:rerun-if-env-changed=KERN_LLAMA_SERVER_ARCHIVE");

    if std::env::var_os("CARGO_FEATURE_BUNDLED_LLAMA_SERVER").is_none() {
        return;
    }

    let archive_path = std::env::var("KERN_LLAMA_SERVER_ARCHIVE").unwrap_or_else(|_| {
        panic!(
            "feature bundled-llama-server ativa mas KERN_LLAMA_SERVER_ARCHIVE não está \
             setada — a release CI deve apontar pro tar.gz preparado com llama-server + \
             libs compartilhadas pra essa plataforma (ver .github/workflows/release.yml)"
        )
    });

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR sempre setado pelo cargo");
    let dest = std::path::Path::new(&out_dir).join("llama-server-bundle.tar.gz");
    std::fs::copy(&archive_path, &dest).unwrap_or_else(|e| {
        panic!(
            "falha ao copiar KERN_LLAMA_SERVER_ARCHIVE={archive_path} pra {}: {e}",
            dest.display()
        )
    });

    println!("cargo:rerun-if-changed={archive_path}");
}
