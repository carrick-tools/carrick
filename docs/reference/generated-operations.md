# Generated `operations` blocks

A top-level `operations` block in `carrick.json` (carrick#831) states the
values a body-dispatching handler answers. A block a person wrote is accepted
and its rows are facts. A block Carrick Cloud writes for a repo is generated
and its rows are candidates.

## The marker

A generated block carries `"source": "generated"` beside `service`, `route`,
`dispatch` and `operations`. The key is optional. A block without it, or with
any other value, is a written block.

```json
{
  "service": "api",
  "route": "POST /rpc",
  "dispatch": { "location": "body", "field": "kind" },
  "operations": [{ "value": "a" }, { "value": "b" }],
  "source": "generated"
}
```

## What a scanner does with it

- A scanner that supports the marker writes the block's rows as candidates,
  where a written block's rows are facts. Route, placement and the replacement
  of inferred rows are the same either way.
- An older scanner ignores the key and reads the block as written (facts). It
  fails nothing and loses no row.
- The config Carrick Cloud returns carries a generated block only when the
  scanner asking has announced a version that supports the marker, so an older
  scanner receives the block it would have received before.
