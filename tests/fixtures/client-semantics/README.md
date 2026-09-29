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

Every row above is `request_summary` and carries `library_semantics`.

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

The re-ask rule runs over the same tree without the sidecar: a stored
detection with no semantics, or with a `pending` entry, is asked once more on
the next scan; a failed ask keeps the stored detection; an answer naming other
packages asks for guidance again; a fully answered detection asks nothing.
