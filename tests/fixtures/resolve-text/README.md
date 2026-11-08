# Value resolver: the text reader (carrick#3504 S2)

`repo/services/web` has one service. `src/request_summary/resolve/text_reader.rs`'s
test `the_text_reader_reads_every_form_the_name_fold_read` reads the initialiser
of each `p_*` constant through `Resolver::text_values`: the one text it
always is, with the files whose declarations it read. The `p_*` names are the
probe names; the declarations they read sit above them.

## Answer key

Read (`forms.ts`; every declaration is in this file, so `read_from` is
`src/forms.ts`, except `p_join_default` and `p_replace_all`, which are
literals and read no declaration):

| Probe | Text |
|---|---|
| `p_const` | `/api` |
| `p_let` (a `let` never assigned again) | `/late` |
| `p_enum` | `/items` |
| `p_const_enum` | `/health` |
| `p_entry` (an entry of a constant object) | `/things` |
| `p_computed_key` (`ROUTES[KEY]`, a constant key) | `/other` |
| `p_as_const` (the compiler types the entry as one literal) | `/frozen` |
| `p_join` | `/api/items/all` |
| `p_join_default` (no separator is a comma) | `a,b` |
| `p_concat` | `/api/x/y` |
| `p_replace` | `/v1` |
| `p_replace_all` | `/b/b` |
| `p_raw` | `/api/\d` |
| `p_template` | `/api//items/tail` |
| `p_plus` | `/api//health` |
| `p_local_join` (a `const` in a top-level function's body) | `/api/local` |

A `let` assigned again (`p_written`) is each value it holds, `/before` and `/after` (S2b-4).

Not read (`refuse.ts`, `evalfile.ts`): each is a hole.

| Probe | Why |
|---|---|
| `p_written_entry` | an entry of the object is assigned |
| `p_exported` | an importer may write the object's entries |
| `p_var` | a `var` |
| `p_pushed` | a call may change the object through an entry |
| `p_dollar` | a replacement that names a `$` pattern reads the match |
| `p_param` | a parameter |
| `p_eval` | `eval` in the module may write any binding |

`shadow.ts`: `p_shadow` reads `/outer`: the inner function declares its own
`OUTER` and writes its entry, which does not block the outer one.

Choices (`alt.ts`, carrick#3504 S2b-1): `?:`, `??` and `||` read every branch, never collapsed. `p_coalesce` is a hole (`maybe`, a runtime value) and `/fallback`; `p_cond` is `/api` and `/other`; `p_chain` is a hole, `/api` and `/last`; `p_union` is `/u/a`, `/u/b` and `/u/c`.
