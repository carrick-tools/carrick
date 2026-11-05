# Discovery fixture (carrick#2431)

`repo/` is one service (`services/web`) whose calls reach labelled library
exports through in-repo functions, for every protocol. `library-claims.json`
is the store's answer for its fixture packages, in the `/library-claims`
answer format (`lib.dom` stands for the compiler's default library until the
store answers it).

`sites.expected.json` is the answer key: the record discovery writes per
site, sorted by file and span.

| Field | Meaning |
|---|---|
| `file`, `line` | The call, repo-relative, 1-based line |
| `span_start`, `span_end` | The call expression's SWC span in the file parsed on its own (byte offset + 1) |
| `callee` | The callee as written |
| `end` | The labelled call the walk reached: the store `package`, `export`, `member` (`null` for the export itself), `role`, `op` (the claim's op for that member, `null` for a construction), and that call's `file`, `line`, `span_start`, `span_end`. `null` when the walk stopped |
| `chain` | Each in-repo function the walk entered, outermost first: the definition key, its file and line. `<callback>` is a function written at the call and run by the callee; `<value>` is a function entered because the call runs it as a value (carrick#2432) |
| `stop` | Why a walk ended with no labelled export: a `HoleReason`, `interface_member`, `injected_callee`, `cycle`, `no_body` (a definition whose body the pass did not read), or for a callee that is a value `value_never_bound` (no caller fills it and it has no default) or `values_over_cap`, or for a call through an interface member the sidecar found more implementations of than it lists, `implementations_over_cap`. `null` when `end` is set, except `several_values` and `several_implementations`: the callee's values (or the interface member's implementations, carrick#2432 slice B) reach different labelled exports, or one reaches none, and each end is kept with this stop (the call is a lead, not a site) |

A site that reaches several labelled calls has one record per labelled call.
Two sites can share one end (`traced-load.ts:4`: the `traced(…)` call and the
`fetch` it runs). A reader that folds records by their end must dedupe them.

A callee that is a value (`values.ts`, `passed-in.ts`, carrick#2432) is
followed through what its callers pass: a destructured parameter's default
(`values.ts:6`), a field filled two calls up (`values.ts:19`), and two
callers handing two labelled exports (`values.ts:32`, `several_values`, while
each caller reaches its own). `fixture-http` is a second labelled HTTP export
for that case.

## Calls discovery could not follow (carrick#2613)

`sites.expected.json` also carries `unfollowed`: a call that reached no
labelled export and entered no code, with its reason as `stop` (`end` null,
`chain` empty). It is not a site; nothing decided reads it.

| `stop` | Meaning |
|---|---|
| `value_callee` | The callee is an in-repo value that is no definition: a parameter, a field, a function a factory returned |
| `outside_service` | The callee is a function of this repo outside the service's files |
| `package_unmatched` | Declared in a package the store answered with network-role exports, and none of its labels matched the call (an export the store labelled with no role is `no_site`) |
| `unanswered_package` | Declared in a package the answer holds nothing for |
| the row's hole | The call's own row names none (`unreadable`, and so on) |

A call into the default library is never `package_unmatched`. A body that
calls only local helpers and the default library gives `no_site`. A class
built in the repo is not `value_callee`.
