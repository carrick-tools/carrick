---
name: carrick-reuse
description: Use at the end of a task that added or changed functions, and whenever the ask is whether something already exists, whether this duplicates code elsewhere, or where this project has built the same thing twice. Compares against the Carrick function index rather than by name.
---

# What already does this

{{SCOPE_NOTE}}

`find_similar` does the comparison. Your work is to read the spans it names
and class every row it returned.

## Targeted: the functions this task added or changed

One call, up to 20 entries, run once at the end of the task.

```
find_similar({{SCOPE}}, functions: [
  { name: "<Class.member or name>", file: "<path suffix>" },
  { description: "<one plain sentence about a function that is not indexed yet>" }
])
```

An entry is either a `name` with a `file` for a function the index holds, or a
`description` for code you are about to write or have just written. Use one or
the other in an entry, never both. A `name` that matches more than one
definition comes back with its candidates on that entry's `error`.

The two kinds are scored on different scales, and each result states the floor
it was ranked against. `vector_basis` says what the cosines are over. On
`intent` the vector is the intent sentence alone, and a copy somebody renamed
scores as close as one that kept its name. On `name_anchored` the function's
name sits in front of the sentence, a renamed copy scores lower, and
`intent_text` is the signal that still finds it. Read every score against the
floor and the basis in the answer you got.

## Audit: the whole project

```
find_similar({{SCOPE}})
```

`clusters` groups functions that describe the same behaviour, ordered by size.
Call again with `offset: <next_offset>` for as long as the response carries
`has_more`, and class what every page returned.

Where the project is larger than one pass, the response carries `error` in place
of clusters and names the two routes under the ceiling: a `service`, or a higher
`min_lines`. Take the route the response names and run it again. Where
`truncated` is present the audit is partial, and its `scanned_functions` of `of`
says by how much.

## Class every row

Every row the answer returned is classed here. In an audit the first member of a
cluster is what the rest of that cluster is classed against; in a targeted call
it is the function you asked about. Read that span at the file and line the
response gave, read each other row the same way, and take the first of these
that holds:

- **FALSE POSITIVE**: the two contracts differ. Different inputs, a different
  result, or a different effect, and the intent sentences alone brought them
  together.
- **VARIANT**: one contract, and a behavioural difference you can name in a
  clause. A different normalisation, a different error path, a different
  default. Write the clause in the row. Where a member's own comment names the
  file it mirrors, the clause is "documented mirror".
- **DUPLICATE**: one contract, and nothing left to name. Two bodies that run
  the same once the identifiers are renamed land here.

A cluster is transitive, so `lowest_similarity` can sit under the floor and a
large group can hold more than one idea. A member that shares no contract with
the first is FALSE POSITIVE on its own row, and stays in the table.

`matched_on` says which signal joined a row. `similarity` is the intent vectors;
`intent_text` is two identical intent sentences, which is the signal that still
finds a copy somebody renamed.

## Report

One row per match, and per cluster member beyond the first. The class column
carries one of the three words and is never empty.

| class | member | file:line | against | why |
|---|---|---|---|---|
| DUPLICATE | slugify | src/util/url.ts:4 | src/text.ts:12 | same replacement rules |
| VARIANT | slugTag | src/tags.ts:20 | src/text.ts:12 | documented mirror |

State `total_clusters` from the response against the number of clusters carrying
rows above. Where the two differ, name the clusters left out.

Then relay the counts the response stated, in its numbers: `compared_functions`,
and every key the answer carries under `not_compared` and under `excluded`.

Rows outside the comparison were not looked at, so an empty answer covers what
was compared and nothing further.

## Act

Merge nothing unless you were asked to. Offer one issue per DUPLICATE, and file
the ones accepted:

```
gh issue create --title "Duplicate: <behaviour> in <n> files" --body "<each file:line, and which one should remain>"
```
