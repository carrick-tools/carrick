# dispatch-generated

Fixture for the unit tests in `src/dispatch.rs` (C16 T3). All names invented.

One routeless handler switches on `body.kind` with four cases. `carrick.json`
lists them in an `operations` block marked `"source": "generated"`.

## Answer key

- With the marker: four `POST /rpc` rows, source `Model` (reads `candidate: model`).
- Marker removed: the same four rows, source `DeclaredOperation` (facts).
- Placement, route and replacement are the same either way.
