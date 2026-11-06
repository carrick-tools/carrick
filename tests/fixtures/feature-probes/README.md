# Feature probes

One invented repo (`repo/`), one probe per language feature: a call or route
whose name, target or payload is written in that feature's form. The probes
carry no code from any third-party corpus; each one's `source` names the
language-test list or TypeScript form it stands for.

- `probes.json`: the labels (what each probe must state: `expect`, `kind`).
- `library-claims.json`: the library-store answer replayed for the offline scan.
- `expected.json`: the outcome each probe has today, one of `covered`,
  `partial`, `lead`, `not_reached`. `tests/feature_probes_test.rs` fails when
  a probe moves either way; a fix that lifts a probe records its gain here.
