---
name: carrick-impact
description: Use before changing or removing a route, a handler, a response shape, an event, or a function other code calls, and whenever the ask is who calls this, who consumes this endpoint, or what breaks if I change it. Names every producer and consumer call site the Carrick index holds, with file and line.
---

# Who depends on what you are about to change

{{SCOPE_NOTE}}

Carrick finds the call sites. Your work is to open them, decide whether each one
breaks, and act.

## 1. Name the thing

A route, a GraphQL field, a socket event or a pub/sub topic: its method label
and its path, as the index spells them. A function: its name, and the file it is
defined in.

## 2. Producers and consumers

For an operation:

```
get_operation({{SCOPE}}, method: "<METHOD>", path: "<path>")
```

Four sections carry the answer.

- `producers`: every service that serves it, with `file_location` and `source`.
- `consumers`: every call site that reaches it, with `services`,
  `call_file_location`, `via` (the function the call is written through, where
  the index names one) and `source`.
- `unmatched_calls`: recorded calls that resolved to no producer.
- the near-miss sections: rows the index holds under a neighbouring method or a
  path one segment away. Read them as candidates to check in source, never as
  consumers of the operation you asked for.

`source` is `fact: …` where a deterministic pass stated the row and
`candidate: …` where the model did. Both are claims about the same path, so the
label belongs in your table.

For a function:

```
get_callers({{SCOPE}}, function_name: "<name>", file: "<file>", depth: 1)
```

Each row names the enclosing caller and its line span, not the call-site line.
Raise `depth` to 2 or 3 for transitive callers. Zero recorded callers is not
deletion evidence, because a function passed as a value is unmeasured.

## 3. Verdicts, one consumer at a time

For each consumer service the step above listed:

```
check_compatibility({{SCOPE}}, consumer_service: "<consumer>", producer_service: "<producer>", path: "<path>")
```

Pass `path`. Without it a large producer returns hundreds of rows. Read
`type_verdicts` (`compatible`, `incompatible`, `unresolved`, `not_compared`) and
the `issues` rows for this operation. A `not_compared` pair has no stored
verdict, which is never agreement.

## 4. A file you have already edited

```
carrick check <file> --recheck --json
```

`items[]` holds this file's routes and calls with their `counterparts` and
`verdict`, judged against the working tree. `recheck.ran` says which answer came
back: `extraction+types` and `extraction` describe the tree, and `none` means
the rows are the indexed ones with `recheck.reason` saying why. `boundary_note`
states what the local run could not classify.

## 5. Report

One table, then the detail.

| consumer service | call site | verdict | source |
|---|---|---|---|
| admin-ui | src/api/orders.ts:44 | INCOMPATIBLE | fact |

Verdict words, and only these: COMPATIBLE, INCOMPATIBLE, UNRESOLVED,
NOT COMPARED. In the same message, state:

- the producers, with file and line;
- unmatched calls and near misses, listed apart from consumers;
- the counts the responses carried: `consumer_count`, `consumers_outside_service`
  where present, and `issues_total` against the rows you read.

Two limits belong in the report wherever they apply. A route added on your
branch has no consumers on main, so an empty consumer list says nothing about
it. A call whose URL is built at the call site can be recorded under the wrong
method, which is what the near-miss rows exist to show.

## 6. Act

Confirm each consumer in source before you call it broken. Change no consumer
code unless you were asked to. Then offer one issue per finding, and file the
ones accepted:

```
gh issue create --title "<consumer> breaks on <METHOD> <path>" --body "<call site, what it expects, what the producer now sends>"
```
