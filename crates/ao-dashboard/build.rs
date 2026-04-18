/// Ensure the UI dist folder exists before rust-embed tries to embed it.
///
/// If `../ao-desktop/ui/dist/` is absent (UI not yet built), create a
/// placeholder `index.html` so compilation succeeds with a degraded page
/// instead of a hard compile error.
///
/// To build the real UI:
///   cd crates/ao-desktop/ui && npm install && npm run build
fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let dist = std::path::Path::new(&manifest)
        .join("../ao-desktop/ui/dist")
        .canonicalize()
        .unwrap_or_else(|_| {
            std::path::Path::new(&manifest)
                .join("../ao-desktop/ui/dist")
                .to_path_buf()
        });

    println!("cargo:rerun-if-changed=../ao-desktop/ui/dist");

    if !dist.exists() {
        println!(
            "cargo:warning=UI not built — serving placeholder. \
             Run: cd crates/ao-desktop/ui && npm install && npm run build"
        );
        std::fs::create_dir_all(&dist).ok();
        std::fs::write(
            dist.join("index.html"),
            b"<!DOCTYPE html><html><body>\
              <h1>ao-dashboard</h1>\
              <p>UI not built. Run:<br>\
              <code>cd crates/ao-desktop/ui &amp;&amp; npm install &amp;&amp; npm run build</code></p>\
              </body></html>",
        )
        .ok();
    }
}
