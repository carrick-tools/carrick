---
name: carrick-drift
description: Use when the ask is whether a consumer and a producer still agree on a type, before changing a request or response shape, and when a compatibility verdict names a problem you cannot place. Puts the producer's type, each consumer call site's expected type and the stored verdict side by side.
---

# Where two services disagree about a shape

{{SCOPE_NOTE}}

`get_contract_pair` holds both sides and the verdict. It reports what the scan
stored and computes no verdict of its own, and neither do you.

## 1. The pairs

```
get_service_graph({{SCOPE}})
```

Each edge is a consumer and a producer. An edge carrying `via_sdk` reaches its
producer through a published package and has no call site of its own. Page with
`offset: <next_offset>` while the rows keep coming. To start from one service,
pass `service: "<name>"`.

## 2. Each pair, operation by operation

```
get_contract_pair({{SCOPE}}, consumer_service: "<consumer>", producer_service: "<producer>")
```

Per operation the answer carries `producer.request` and `producer.response` as
type ids, `consumer.call_sites` with each site's `file_location`,
`expected_request` and `expected_response`, and `verdicts`. The `types` array
holds each distinct type text once, keyed by the id those rows reference.
Operations are ranked by call-site count then by untyped sides. Page with
`offset: <next_offset>`, or narrow with `path` and `method`.

A verdict's `state` is `compatible`, `incompatible` or `unresolved`, and it
carries `scanner_version` with `mismatch_reason` or `unresolved_reason` where
the scan wrote one. One verdict covers the whole consumer and producer pair, so
every call site on that operation shares it. An operation with `verdicts: []`
was never judged, and `verdict_note` says so.

Honour the wire note. Where `wire_notes` is present, one side declares a field
as `Date` and the other as `string`. JSON serialises a Date to a string, so
those two texts can describe the same bytes, and that is not drift.

## 3. Class each operation

- **MATCH**: the stored verdict is `compatible`.
- **DRIFT**: the stored verdict is `incompatible`. Name the field and the
  direction, request or response, from `mismatch_reason` and the two type texts.
- **UNRESOLVED**: the stored verdict is `unresolved`. Report `unresolved_reason`
  as it is written. Then read the two type texts this answer already returned in
  `types`, the producer's side against the call site's expected type, and where
  they differ name the field and say whether it is missing on one side, optional
  on one side and required on the other, or of a different type. Report that as
  "type texts differ", never as a verdict.
- **NOT JUDGED**: `verdicts` is empty. Relay `verdict_note`, then read the same
  two type texts the same way and report any difference as "type texts differ".
- **CONSUMER UNTYPED**: a call site's `expected_request` or `expected_response`
  is null on a side that carries one.
- **PRODUCER UNTYPED**: `producer.request` is null on a method that carries a
  body, or `producer.response` is null.

A GET declares no request type by design, and `untyped_sides` already counts it
that way. The wire note covers both readings above, so a `Date` on one side
against a `string` on the other is not a difference.

## 4. Report

| operation | class | producer type | consumer type | call site | verdict |
|---|---|---|---|---|---|
| GET /api/orders/:id | DRIFT | Order | OrderSummary | web/src/orders.ts:31 | incompatible |
| POST /api/orders | UNRESOLVED | NewOrder | OrderDraft | web/src/orders.ts:52 | unresolved; type texts differ on `note` |

Class words, and only these: MATCH, DRIFT, UNRESOLVED, NOT JUDGED, CONSUMER
UNTYPED, PRODUCER UNTYPED. Every operation the answer returned carries one of
them, the class column is never empty, and the report states operations returned
against operations classed. Where more than one word fits an operation, the row
takes the first that applies of DRIFT, PRODUCER UNTYPED, CONSUMER UNTYPED,
UNRESOLVED, NOT JUDGED, MATCH, because a stored incompatible verdict is the
finding and a side carrying no type is why nothing past it could be judged.
A reading of the two type texts goes in the verdict
column beside the stored state, in the words "type texts differ", so nothing in
the table reads as a verdict the index did not give you.

State alongside it: `operations_total` against `operations_shown`,
`matched_calls` and `unmatched_calls`, `dropped_rows`, and the `non_http_note`
where the pair also carries GraphQL, socket or pub/sub operations, whose type
text this surface does not hold.

## 5. Act

Change no type unless you were asked to. Offer one issue per DRIFT, and one per
UNRESOLVED or NOT JUDGED row whose type texts differ. File the ones accepted:

```
gh issue create --title "<consumer> and <producer> disagree on <field> of <operation>" --body "<producer type, consumer type, call sites, stored verdict and scanner version>"
```

An UNRESOLVED or NOT JUDGED row holds no verdict on the difference you read, so
its title says the two sides may disagree and its body carries the reason the
tool gave in place of a stored verdict:

```
gh issue create --title "<consumer> and <producer> may disagree on <field> of <operation>" --body "<producer type, consumer type, call sites, and the unresolved_reason or verdict_note as written>"
```
