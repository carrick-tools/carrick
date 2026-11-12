//! The fixture's library store answer, replayed offline (carrick#3711).
//!
//! A scan asks the store what each installed package's exports do, and only
//! a labelled call is a candidate. Offline, the answer is read from
//! `<CARRICK_MOCK_FIXTURE_DIR>/library-claims/default.json`
//! (`src/library_store.rs`). A fixture whose test writes its cassettes into a
//! temporary directory keeps its answer beside the source as
//! `library-claims.json`, and the test copies it in with [`install`].

#![allow(dead_code)]

use std::path::Path;

/// Copy `<fixture>/library-claims.json` to `<cassettes>/library-claims/default.json`.
pub fn install(fixture: &Path, cassettes: &Path) {
    let to = cassettes.join("library-claims");
    std::fs::create_dir_all(&to).expect("create the library-claims cassette dir");
    std::fs::copy(fixture.join("library-claims.json"), to.join("default.json"))
        .unwrap_or_else(|error| panic!("copy {}/library-claims.json: {error}", fixture.display()));
}
