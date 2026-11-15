# chained-member-receivers

One root service, four stub packages (`fixture-courier` HTTP client,
`fixture-hub` pub/sub, `fixture-jobs` queue, `fixture-tap` pub/sub whose op
maker the verifier cannot answer), one cassette (`library-claims.json`).

Answer key for `tests/chained_member_receivers_test.rs` (carrick#4498): a
member called on a call's value or on a `new`'s value is keyed by the type the
compiler gives that value, as the same member on a held name is. Only the
type is read, never the `call` row's symbol (the callee).

| Site | Form | Before | After |
|---|---|---|---|
| `http.ts:6` | held `courier.area` | `no_op_claim path Courier.area` | same |
| `http.ts:12` | `Courier.connect(..).area(..)` | no row | `no_op_claim path Courier.area` |
| `pubsub.ts:13` | `hub.region(..).channel` | `package_unmatched callee` | `no_op_claim name Region.channel` |
| `pubsub.ts:18` | `new Hub().region`, `.channel` | `package_unmatched callee` | `no_op_claim name Hub.region`, `Region.channel` |
| `queue.ts:12` | `connect(..).queue` | `package_unmatched callee` | `no_op_claim name Jobs.queue` (never `connect.queue`) |
| `queue.ts:21` | `jobs().queue` | `package_unmatched callee` | `no_op_claim name Jobs.queue` |
| `queue.ts:32` | in-repo `LocalJobs` (negative) | `value_callee callee` | same |
| `unverified.ts:13`, `:18` | chained op whose maker fails | `unanswered callee` (unkeyed) | `unanswered name Pipe.write` |

The sends (`calls` rows) are the same before and after. Stub packages are
added with `git add -f` (`node_modules` is ignored).

Protocols: HTTP, pub/sub (queues).
