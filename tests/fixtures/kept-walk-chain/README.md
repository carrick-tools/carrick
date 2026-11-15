# Kept walks of a chain

`repo/` has three services in a chain: `services/app` serves
`GET /api/v1/widgets/:widgetId` (a file route, no model) and calls
`packages/mid`, which states the request and sends through `packages/base`,
the only one that calls `fetch`.

A per-edit re-check of the app's route re-reads `app` and carries `mid` and
`base`: it walks `mid` for the app's calls and `base` for the calls the walk
of `mid` reaches. The join moves `mid`'s pair, so the scan also writes `mid`'s
blob. Both walks are kept for the next re-check, with the record of the commit
they were carried at; the app's rows, an analysed service's, never are.
`tests/local_mode_test.rs` holds the check.
