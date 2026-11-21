//! The cloud adds model rows to an index blob at read time (`withModelRows`,
//! carrick-cloud `lambdas/_shared/model_rows.ts`). Every scanner since
//! carrick#691 reads such a blob: a row marked `resolution_source: "model"`
//! deserialises, its required fields are filled, and the optional `model_end`
//! and the blob's `model_rows` stamp are ignored.
//!
//! `tests/fixtures/model-rows/model_rows.merged.json` is a copy of the cloud's
//! golden merged output for an invented two-service project. Never under
//! `src/cloud_storage/`, which is a seam prefix.

use carrick::agents::file_analyzer_agent::ResolutionSource;
use carrick::cloud_storage::CloudRepoData;
use carrick::operation::OperationKey;

fn merged() -> Vec<CloudRepoData> {
    let text = std::fs::read_to_string("tests/fixtures/model-rows/model_rows.merged.json")
        .expect("the merged fixture");
    let blobs: Vec<serde_json::Value> = serde_json::from_str(&text).expect("a JSON array");
    blobs
        .into_iter()
        .map(|b| {
            serde_json::from_value::<CloudRepoData>(b).expect("a merged blob is a CloudRepoData")
        })
        .collect()
}

fn service<'a>(blobs: &'a [CloudRepoData], name: &str) -> &'a CloudRepoData {
    blobs
        .iter()
        .find(|b| b.service_name.as_deref() == Some(name))
        .expect("service in the fixture")
}

#[test]
fn a_merged_blob_deserialises_as_cloud_repo_data() {
    let blobs = merged();
    assert_eq!(blobs.len(), 3);
}

#[test]
fn model_rows_carry_the_model_source_and_the_index_key() {
    let blobs = merged();
    let api = service(&blobs, "api");
    let row = api
        .endpoints
        .iter()
        .find(|r| matches!(&r.key, OperationKey::Http { path, .. } if path == "/api/orders/:id"))
        .expect("the model serving end");
    assert_eq!(row.resolution_source, Some(ResolutionSource::Model));
    assert!(row.resolution_source.unwrap().is_candidate());
    assert!(matches!(&row.key, OperationKey::Http { method, .. } if method == "GET"));

    let web = service(&blobs, "web");
    let mount = web.mount_graph.as_ref().expect("web has a mount graph");
    let call = mount
        .data_calls
        .iter()
        .find(|c| c.canonical_path == "/functions/v1/x")
        .expect("the model call is placed in the mount graph, where the graph already has rows");
    assert_eq!(call.resolution_source, Some(ResolutionSource::Model));
    assert_eq!(call.line, Some(7));
    // the scanner's own rows keep no source
    assert!(web.calls.iter().any(|c| c.resolution_source.is_none()));
}

#[test]
fn a_merged_blob_round_trips_with_its_model_rows_intact() {
    let blobs = merged();
    for blob in &blobs {
        let again: CloudRepoData =
            serde_json::from_value(serde_json::to_value(blob).unwrap()).unwrap();
        let count = |b: &CloudRepoData| {
            b.endpoints
                .iter()
                .chain(b.calls.iter())
                .filter(|r| r.resolution_source == Some(ResolutionSource::Model))
                .count()
        };
        assert_eq!(count(blob), count(&again));
    }
}
