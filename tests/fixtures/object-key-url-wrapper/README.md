# `object-key-url-wrapper`

Fixture for carrick#1950: a request wrapper that takes its request as one
object, so the URL arrives as a key of a parameter, and a URL that ends in a
query one of the service's own functions builds.

Invented for the shape. No file here is copied from a scanned project.

## The shape

A client funnels its requests through one wrapper:

```ts
private async submit({ target, payload }: { target: string; payload?: Payload }) {
  const res = await fetch(target, { method: "POST", … });
  return res.json();
}
```

and each caller writes the URL into the object it hands over, often ending in
a query the class builds:

```ts
return await this.submit({
  target: this.base + `/sessions/open/${kind}${this.query(ref)}`,
  payload: { kind },
});
```

Neither half is a row on its own. The wrapper's `fetch(target, …)` is the only
candidate the file raises, and its target is a parameter with one definition
per caller. The callers raise no candidate: the receiver is `this`, and the
argument is an object with neither `method` nor `url`. Before carrick#1950 the
request summaries read a wrapper's URL only through a plain positional
parameter, and refused any value glued after a path segment, so the file
stated nothing.

## Files

- `src/portal-client.ts`: a class whose wrapper method destructures its one
  parameter. Callers write the URL inline, through a `const`, with and without
  the class's own query builder, and through a method that passes its options
  on.
- `src/things.ts`: module functions that read the key every other way: a
  destructured parameter, `options.url`, `const { url } = options`, a key
  bound under another name, and a verb the caller writes beside the URL. One
  URL ends in a module-scope query builder.
- `src/query.ts`: a query builder used in the module that declares it.
- `src/keyed-base.ts`: requests that state their own route, with a key as the
  base or as a path segment. They are stated where they always were.
- `src/refused.ts`: callers that must state nothing.
- `src/refused-class.ts`: classes whose query builder can be replaced.

## The answer key

| site | truth |
|---|---|
| `portal-client.ts:14` | `POST ${this.base}/sessions/open/:kind` |
| `portal-client.ts:23` | `POST ${this.base}/sessions/close/:kind` |
| `portal-client.ts:29` | `POST ${this.base}/sessions` |
| `portal-client.ts:34` | `POST ${this.base}/sessions/renew/:kind` |
| `things.ts:45` | `PUT ${THINGS_API_URL}/things/:id/name` |
| `things.ts:49` | `PATCH ${THINGS_API_URL}/things/:id/touch` |
| `things.ts:53` | `DELETE ${THINGS_API_URL}/things/:id` |
| `things.ts:57` | `POST ${THINGS_API_URL}/things/:id/copies` |
| `things.ts:61` | `GET ${THINGS_API_URL}/things` |
| `things.ts:65` | `GET ${THINGS_API_URL}/things/count` |
| `query.ts:14` | `GET ${THINGS_API_URL}/reports` |
| `keyed-base.ts:7` | `GET ${apiUrl}/widgets` |
| `keyed-base.ts:18` | `GET ${options.apiUrl}/widgets/${options.id}` |

Thirteen rows, each `request_summary`, and no other: not at a wrapper's own
`fetch`, and not where `portal-client.ts:39` passes its options on.

The last two are unchanged by carrick#1950, and are here to hold that: a
request that states its route without its caller (a key as the base before a
literal path, a key as a whole path segment) is stated at its own line, under
the name the function reads the key by, and not at `keyed-base.ts:12`, the
caller that writes the base. A key waits on a caller only where the request
states no route without it.

`THINGS_API_URL` is undeclared (there is no `carrick.json`), so each row keeps
its env-var base: an expected find.

## What must state nothing

Every caller in `refused.ts`, one line each:

| caller | why |
|---|---|
| `spreadAfter` | a spread after the URL may overwrite it |
| `computedAfter` | a computed key after the URL may overwrite it |
| `missingKey` | the caller does not write the key |
| `writtenThrough` | the object is written through after it is built |
| `rewritten` | the wrapper assigns the binding again before it sends |
| `defaultVerb` | the wrapper supplies a verb when the caller writes none (never read as `GET`) |
| `looseTail` | the builder has one `return` that is not a query |
| `importedTail` | the builder is another module's |
| `asyncTail` | the builder is `async` |
| `recursiveTail` | one `return` is another call |
| `partialTail` | the builder can return nothing |
| `plainTail` | the tail is a plain value no function builds |
| `queryThenPath` | text follows the query, which may be empty |

And both classes in `refused-class.ts`: a subclass in the file overrides the
builder, or the constructor assigns over it.

## The cassettes

Every file under `__llm__/analyze-file/` is the empty answer, which is what
extraction can honestly say here: no candidate has a URL to write. Every row
the test pins is the summaries' own.
