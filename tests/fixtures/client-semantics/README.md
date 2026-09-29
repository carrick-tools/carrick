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
