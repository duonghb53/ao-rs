//! Staleness guard for `parity_*` modules in `crates/ao-core/src/`.
//!
//! Fails if the on-disk set of parity modules drifts from the documented
//! classification, or if any module is missing the `Parity status:` header.
//!
//! See `docs/ts-core-parity-report.md` → "Parity-only modules" for the
//! policy and per-module notes.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParityClass {
    TestOnly,
    Mixed,
    #[allow(dead_code)] // reserved for future modules whose code runs in runtime paths
    ProductionUsed,
}

impl ParityClass {
    fn header_tag(self) -> &'static str {
        match self {
            ParityClass::TestOnly => "Parity status: test-only",
            ParityClass::Mixed => "Parity status: mixed",
            ParityClass::ProductionUsed => "Parity status: production-used",
        }
    }
}

/// Single source of truth for the classification of every `parity_*` module.
///
/// To add / remove / reclassify a parity module:
/// 1. Update this list.
/// 2. Update the table in `docs/ts-core-parity-report.md` →
///    "Parity-only modules".
/// 3. Ensure the module's `//!` header contains the matching
///    `Parity status: <value>` line.
const PARITY_MODULES: &[(&str, ParityClass)] = &[
    ("parity_config_validation.rs", ParityClass::Mixed),
    ("parity_feedback_tools.rs", ParityClass::TestOnly),
    ("parity_metadata.rs", ParityClass::TestOnly),
    ("parity_notifier_resolution.rs", ParityClass::TestOnly),
    ("parity_observability.rs", ParityClass::TestOnly),
    ("parity_plugin_registry.rs", ParityClass::TestOnly),
    ("parity_session_strategy.rs", ParityClass::Mixed),
    ("parity_utils.rs", ParityClass::TestOnly),
];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn discovered_parity_files() -> BTreeSet<String> {
    fs::read_dir(src_dir())
        .expect("read src dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("parity_") && name.ends_with(".rs") {
                Some(name)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn parity_module_set_matches_expected() {
    let on_disk = discovered_parity_files();
    let expected: BTreeSet<String> = PARITY_MODULES
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();

    let missing: Vec<_> = expected.difference(&on_disk).collect();
    let unexpected: Vec<_> = on_disk.difference(&expected).collect();

    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "Parity module set drift.\n\
         Missing (listed in `PARITY_MODULES` but not on disk): {missing:?}\n\
         Unexpected (on disk but not listed): {unexpected:?}\n\
         \n\
         To fix: update `PARITY_MODULES` in this test AND the table in\n\
         `docs/ts-core-parity-report.md` → \"Parity-only modules\"."
    );
}

#[test]
fn every_parity_module_has_status_header() {
    let src = src_dir();
    for (name, class) in PARITY_MODULES {
        let path = src.join(name);
        let content =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(
            content.contains(class.header_tag()),
            "{name} is missing required header line `{}`. \
             Every `parity_*` module must carry a `//!` `Parity status:` tag \
             matching its classification in `PARITY_MODULES`.",
            class.header_tag(),
        );
    }
}

/// The `mixed` classification for `parity_session_strategy.rs` depends on
/// its two enums being re-exported from `ao_core`. If the re-export goes
/// away, the module is no longer "mixed" and must be reclassified (or the
/// enums must be moved into a non-parity module).
#[test]
fn session_strategy_enums_still_reexported_from_lib() {
    let lib = fs::read_to_string(src_dir().join("lib.rs")).expect("read lib.rs");
    let has_reexport = lib.contains(
        "pub use parity_session_strategy::{OpencodeIssueSessionStrategy, \
         OrchestratorSessionStrategy}",
    ) || (lib.contains("pub use parity_session_strategy::")
        && lib.contains("OrchestratorSessionStrategy")
        && lib.contains("OpencodeIssueSessionStrategy"));
    assert!(
        has_reexport,
        "parity_session_strategy is classified as Mixed because its enums are \
         re-exported from `ao_core::lib`. That re-export is gone. Either \
         restore it, or reclassify the module in `PARITY_MODULES` and the \
         docs table."
    );
}
