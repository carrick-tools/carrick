# pr-empty-answer

A file is sent to the model and the model's answer omits a route the fact
layer reads. Same sources as `receiver-role-split` (two invented packages;
`library-claims.json` is the store's answer): `src/app.ts:10` and
`src/typed.ts:7` register a literal route on a server receiver the compiler
reads.

`tests/pr_empty_answer_test.rs` scans twice, once with an answer that states
both routes and once with an answer that omits them, and compares the rows.
`node_modules/` holds declaration files only, committed on purpose.

`src/app.ts` also registers two routes in a loop over a tuple array and holds
a `fetch` call to a host passed in, which the model is asked about; the second
test states them in one scan and omits them in the other.
