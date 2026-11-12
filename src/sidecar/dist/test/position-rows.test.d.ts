/**
 * Per-position compiler rows (carrick#2405).
 *
 * The `position_rows` action writes one row for each call, member access,
 * comparison and expression identifier of the files it is sent, as a gzip
 * JSON-lines file. The fixture is a two-package workspace
 * (`tests/fixtures/position-rows/`): a service under `apps/api` that imports a
 * workspace package by its `source` export condition, an installed package
 * inside the repo, one installed above it, and files that stress the
 * position key: multi-byte text, CRLF line endings, a BOM, an unresolved
 * import, plain JavaScript, JSX and a file the tsconfig does not include.
 *
 * The decompressed output is compared with the committed
 * `api.expected.jsonl`, exactly. The scanner reads the same file with its
 * serde types (`src/position_rows.rs`), so the file is the seam pin both
 * sides read.
 */
export {};
