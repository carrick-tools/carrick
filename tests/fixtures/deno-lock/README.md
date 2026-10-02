# `deno-lock`

Lockfiles for the library store's public-registry rule on Deno installs
(carrick#1720), read by the unit tests in `src/library_store.rs`. Each lists
`chalk` 5.3.0 from npm, and all but `v2.lock` list `@std/path` 1.1.6 from
JSR.

| File | Written by |
|---|---|
| `v5.lock` | Deno 2.9.6, `deno install` of `npm:chalk@5.3.0` and `jsr:@std/path@1`, default registry |
| `v5-other-registry.lock` | Deno 2.9.6, the same install of `chalk` with `registry=` set to another host in `.npmrc`. Deno records that host as `tarball`; the host is then replaced with `npm.internal.example` |
| `v2.lock`, `v3.lock`, `v4.lock` | Hand-written, to the shapes Deno's `deno_lockfile` crate (0.61.0) reads and migrates: its `read_version_2` test, its v3 transform specs, and `transform3_to_4` |

Format 1 has no `version` field and records remote modules only, so it holds
no npm package.
