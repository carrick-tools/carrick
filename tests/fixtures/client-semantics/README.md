# `client-semantics`

Golden fixture for carrick#1564: calls made through a data-fetching
package's client, read through the client semantics framework detection
states, and only where the package's own declarations verify them.

The packages are invented, and their declarations are hand-written in the
vendored `node_modules`:

- `@fixture/http` is a `create({ baseURL })` client: a callable default
  export with `get` and `post` verbs, a config-object `request` member, and a
  `create` factory whose options name the base URL.
- `fixture-prefix-http` is a `create({ prefixUrl })` client: every call takes
  a path and an options bag, the body sits under the bag's `json` key, and
  the client itself is callable.

`fixture-slow-http` and `@fixture/internal-sdk` are named by detection and not
installed; detection answers them `pending` and `skipped`.

The sources:

- `src/http-client.ts` builds an instance with a base path prefix at module
  scope, calls two verbs on it, and calls the export's own `request` member
  with a config object.
- `src/prefix-client.ts` builds an instance in a class field and calls it
  three ways: a verb with a path that has no slash of its own, a verb with a
  body under the options' key, and the instance itself with a path and a
  method in the options.
- `src/negatives.ts` holds a `get` on a `Map` built in place, one on a `Map`
  held in a module constant, and one on a `Map` bound to the client's own
  name inside a function.
- `src/a1-mutated.ts` to `src/a5-static.ts` and `src/a13-inherit.ts` are the
  adversarial sites from the review of the pull request that added this
  fixture: a base key written through the instance after the factory ran
  (a1), a base named in the call's own options (a2), factory options and a
  request config open to a spread (a3), a field a constructor branch writes
  again (a4), a static member reading `this` beside an instance member (a5),
  and a field a subclass declares again plus a block redeclaring the
  instance's name (a13).
- `src/n1-spread-before.ts` to `src/n9-phase1-fetch.ts` are the sites from the
  re-review: a base written after a spread (n1), between two spreads (n2),
  conditional spreads that disagree, agree, or may spread nothing (n3), a
  getter, a computed key, a string key and a method beside the base (n4), a
  field a constructor `try` or loop writes again (n5), a field a method
  reassigns or writes through (n6), a spread constant the file writes to
  (n7), a base written through an alias of the client or `Object.assign`
  (n8), and the same spread shapes on a plain `fetch` (n9).
- `src/r1-passed-const.ts` to `src/r7-interceptor.ts` and
  `src/p-fetch-consts.ts` are the third review's: options the file passes
  to a call (r1), an instance stored in an array and an object (r2),
  returned (r3), built from an export whose property is read (r4) or tested
  with `instanceof` (r4b) beside a control (r4c), called through a member
  its surface does not name (r5), with a header written (r6) or an
  interceptor added (r7), and a plain `fetch` handed constants the file
  writes through, passes to a call, or leaves alone (p).

`variants/http-without-base-key.d.ts` is `@fixture/http` as a release whose
`create` takes `baseUrl`. The tests install it over the vendored declaration
to fail the factory claim. It is not part of the scanned tree.

## The cassettes

`__llm__/framework-detect/framework-detect.json` is the contract sample from
carrick#1564, byte for byte. The cloud holds the same bytes.

The `analyze-file` answers are wrong the way a model's are for these calls:
the verb the method name suggests, the path with no base, no body. Nothing a
test asserts about a stated row can come from them; where a site must read as
it does without the semantics, the test compares it with a scan of the same
tree whose detection answers no semantics.

## The answer key

With the vendored declarations, through the real type sidecar:

| site | row | claims used |
|---|---|---|
| `http-client.ts:6` | `GET /api/v1/users` | factory, `verb:get` |
| `http-client.ts:11` | `POST /api/v1/orders` | factory, `verb:post`, `verb_body:post` |
| `http-client.ts:16` | `POST /inventory/sync` | `request:request:config`, `request_body:request:config` |
| `prefix-client.ts:7` | `GET /svc/jobs/queued` | factory, `verb:get` |
| `prefix-client.ts:11` | `POST /svc/jobs/run` | factory, `verb:post`, `verb_body:post` |
| `prefix-client.ts:15` | `PUT /svc/jobs/reports` | factory, `request:():path_options`, `request_body:():path_options` |
| `negatives.ts` | none | none |
| `a5-static.ts:10` | `GET /instance/ran` | factory, `verb:get` |
| `n1-spread-before.ts:7` | `GET /after/stated` | factory, `verb:get` |
| `n3-cond-spread.ts:10` | `GET /x/keep` | factory, `verb:get` |
| `n3-cond-spread.ts:11` | `GET /same/agree` | factory, `verb:get` |
| `n4-getter-computed.ts:11` | `GET /s1/string-key` | factory, `verb:get` |
| `n4-getter-computed.ts:12` | `GET /m1/method-prop` | factory, `verb:get` |
| `r1-passed-const.ts:10` | `GET /r1b/passed-after` | factory, `verb:get` |
| `r4c-control.ts:5` | `GET /r4c/control` | factory, `verb:get` |

Every row above is `request_summary` and carries `library_semantics`.

Every other adversarial site reads exactly as the same tree does without
the semantics, because the source may set the base, or the field, somewhere
the reading cannot see:

| site | row |
|---|---|
| `a1-mutated.ts:7` | `GET /mutated`, `receiver_type` |
| `a2-percall-base.ts:6` | `GET /users`, `receiver_type` |
| `a2-percall-base.ts:10` | `GET /plain`, `receiver_type` |
| `a3-spread-after.ts:7` | `GET /spread`, `receiver_type` |
| `a3-spread-after.ts:11` | none: the config's `url` and `method` come before a spread |
| `a4-ctor-branch.ts:12` | none |
| `a5-static.ts:7` | none |
| `a13-inherit.ts:6` | none |
| `a13-inherit.ts:18` | none |
| `a13-inherit.ts:20` | `GET /outer-ok`, `receiver_type` |
| `n2-two-spreads.ts:8` | `GET /between`, `receiver_type` |
| `n3-cond-spread.ts:9` | `GET /cond`, `receiver_type` |
| `n3-cond-spread.ts:12` | `GET /and`, `receiver_type`: `prod && {…}` may spread nothing |
| `n4-getter-computed.ts:9` | `GET /getter`, `receiver_type` |
| `n4-getter-computed.ts:10` | `GET /computed`, `receiver_type` |
| `n5-ctor-try-loop.ts:12`, `:23` | none |
| `n6-method-write.ts:8`, `:16` | none |
| `n7-mutated-spread-const.ts:10` | `GET /mutated-const`, `receiver_type` |
| `n8-indirect-write.ts:10` | `GET /alias-write`, `receiver_type` |
| `n8-indirect-write.ts:11` | `GET /assign-write`, `receiver_type` |
| `r1-passed-const.ts:9` | `GET /passed`, `receiver_type` |
| `r2-stored.ts:9` | `GET /direct`, `receiver_type` |
| `r3-returned.ts:6` | `GET /returned`, `receiver_type` |
| `r4-export-read.ts:6` | `GET /const-read`, `receiver_type` |
| `r4b-instanceof.ts:6` | `GET /instanceof`, `receiver_type` (carrick#1568 would recover it) |
| `r5-setter.ts:6` | `GET /setter`, `receiver_type` |
| `r6-header-write.ts:6` | `GET /header-write`, `receiver_type` |
| `r7-interceptor.ts:6` | `GET /interceptor`, `receiver_type` |

`n9-phase1-fetch.ts` has no library in it. Its request summaries state `POST
/api/f2` at line 10 (the method written after the spread) and `GET /api/f5`
at line 13, and nothing at lines 9, 11 and 12, where a spread may set the
method; any other row there is the model's.

`p-fetch-consts.ts` states `PATCH /api/p4` at line 18 and `GET /api/p6` at
line 20, and nothing at line 15, where the constant is written through, or
at 17 and 19, where a spread reads a constant the file writes to or passes
to a call. Line 16 states `DELETE /api/p2` from a constant the file passes to
a call first, which may change it (carrick#1588).

The `receiver_type` rows state the path without the instance's base. That
is how main reads them today, and it is a wrong fact of its own
(carrick#1583).

With `variants/http-without-base-key.d.ts` installed, the factory claim for
`@fixture/http` fails with `key_missing` while the sidecar still verifies
`verb:get` on `instance:create`. The scanner reads an instance only through a
verified factory, so `http-client.ts:6` and `:11` read as they do without the
semantics: the receiver-type rows `GET /users` and `POST /orders`, with no
base. `http-client.ts:16` is on the export and keeps its row, and the prefix
client is untouched.

With no `node_modules`, nothing verifies and every row is the one the same
tree states without library semantics. The same holds for the tree as a
Deno service importing both packages through `npm:` into an empty Deno
cache: every check is `unchecked` and the scan completes (carrick#1570).

The re-ask rules run over the same tree without the sidecar: a stored
detection with no semantics is asked once more on the next scan, and one with
a `pending` entry only when the test installs that package too; a failed ask
keeps the stored detection; an answer naming other packages asks for
guidance again; a fully answered detection asks nothing; and a run retrying
the work it still owes does not ask again in the same run. When the test
installs `fixture-slow-http`, which this sample always answers `pending`, one
scan asks `/framework-detect` three times, the first ask and the in-scan
schedule's two, and its rows are the answer key's.
