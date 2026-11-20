# Matched-pair record contract

`contract.json` is the one example of the stored record, `carrick.matched/1`:
call-to-server pairs a model placed after the scan, for calls the scan did not
pair. The cloud pins the same bytes by the same sha256
(`10d6047678d751f8fb395eba8cb56f939f6b78ee170abce9194c35341848d065`), and
`tests/matched_record_contract_test.rs` fails if either side changes. The data
is invented.

## Top level

- `schema`: `"carrick.matched/1"`.
- `repo`: `owner/name`.
- `commit`: 40 hex characters, the indexed commit.
- `run`: `id`, `mode` (`full` or `changed_files`), `base_commit` (sha or null),
  `model`, `brief_sha256`, `started_at`, `finished_at`.
- `engine`: `status` (`done`, `partial`, `failed`, `not_run`), optional
  `reason` (`[a-z_]{1,64}`), `ms`, `scanner_version`.
- `pairs`: a list of pairs.
- `dropped`: a count per reason, non-zero entries only.

## A pair

Required: `id`, `caller`, `server`, `operation`, `basis`, `model_confidence`,
`carried_from`.

- `caller` and `server` are each `{file, line, line_sha256}`. The file is
  repo-relative, with no leading `/` and no `..`. The line is 1 or more.
- `operation` is `{kind, method, name}`. The method is uppercase. `kind` is one
  of `route`, `graphql_field`, `procedure`, `topic`, `event`, `queue`, `table`,
  `database_function`, `function`, `other`.
- `basis` is `text`, `symbol` or `config`.
- `model_confidence` is `stated` or `inferred`.
- `carried_from` is the commit the pair was carried from, or null.

Optional, where absence means nobody wrote it:

- `sides`: any of `caller_sends`, `caller_expects`, `server_reads`,
  `server_returns`. Each holds `compiler` and/or `model`. A reading is
  `{text, evidence}` (text at most 16,384 bytes) or `{reason, evidence?}`.
  A compiler `text` is never `any` or `unknown`; the engine writes a `reason`
  instead so a model reading can fill the side.
- `verdicts`: `compiler` and/or `model`, each `{grade, compiler_verdict?,
  detail?, evidence?}`. `grade` is `fails_at_run_time`, `type_level_only`,
  `extra_field_ignored`, `agree`, `cannot_tell`, or null (a compiler
  `incompatible` only). No verdict gates.
- `wrapper`: `{name, file, line, callers, callers_total}` (at most 200
  callers), or `{reason}`.

## Hashes

- `line_sha256` is the sha256 of the UTF-8 bytes of the line, trimmed, without
  its newline.
- `id` is the first 16 hex characters of the sha256 of these values joined by
  `\n`: caller file, caller line, kind, method, name, server file, server line.
  A duplicate id in one record keeps the first and counts `dropped.duplicate`.
