/// Populate `ui-dist/` (inside this crate) so rust-embed can embed it.
///
/// Priority:
///   1. `ui-dist/` already exists (pre-built or from crates.io package) → use as-is
///   2. `../ao-desktop/ui/dist/` exists (local workspace build) → copy into `ui-dist/`
///   3. Neither exists → write a placeholder so compilation succeeds
///
/// To build the real UI:
///   cd crates/ao-desktop/ui && npm install && npm run build
fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest = std::path::Path::new(&manifest);

    let ui_dist = manifest.join("ui-dist");
    let src_dist = manifest.join("../ao-desktop/ui/dist");

    println!("cargo:rerun-if-changed=../ao-desktop/ui/dist");
    println!("cargo:rerun-if-changed=ui-dist");

    // Case 1: ui-dist already populated — nothing to do.
    if ui_dist.exists() && ui_dist.join("index.html").exists() {
        return;
    }

    // Case 2: workspace build — copy from sibling crate's dist.
    if src_dist.exists() {
        if let Err(e) = copy_dir(&src_dist, &ui_dist) {
            println!("cargo:warning=failed to copy UI dist: {e}");
        } else {
            return;
        }
    }

    // Case 3: no UI available — write placeholder.
    println!(
        "cargo:warning=UI not built — serving placeholder. \
         Run: cd crates/ao-desktop/ui && npm install && npm run build"
    );
    std::fs::create_dir_all(&ui_dist).ok();
    std::fs::write(
        ui_dist.join("index.html"),
        b"<!DOCTYPE html><html><body>\
          <h1>ao-dashboard</h1>\
          <p>UI not built. Run:<br>\
          <code>cd crates/ao-desktop/ui &amp;&amp; npm install &amp;&amp; npm run build</code>\
          </p></body></html>",
    )
    .ok();
}

fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}
