---
name: carrick-reuse
description: Use at the end of a task that added or changed functions, and whenever the ask is whether something already exists, whether this duplicates code elsewhere, or where this project has built the same thing twice. Compares against the Carrick function index rather than by name.
---

# What already does this

{{SCOPE_NOTE}}

`find_similar` does the comparison. Your work is to read both spans and class
each pair.

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

The two kinds are scored on different scales and the response states both
floors: 0.85 between two indexed functions, 0.45 for a description. A stored
vector carries the function's name in front of its intent and a bare sentence
does not, so a description scoring 0.5 is a hit worth reading.

## Audit: the whole project

```
find_similar({{SCOPE}})
```

`clusters` groups functions that describe the same behaviour, ordered by size.
Page with `offset: <next_offset>` while `has_more` is true. A group is
transitive, so `lowest_similarity` can sit under the floor and a large group can
hold more than one idea.

Where the project is larger than one pass, the response carries `error` in place
of clusters and names the two routes under the ceiling: a `service`, or a higher
`min_lines`. Take the route the response names and run it again. Where
`truncated` is present the audit is partial, and its `scanned_functions` of `of`
says by how much.

## Class each pair

Read both spans in source, then class:

- **DUPLICATE**: the same behaviour, and one call site could use the other.
- **VARIANT**: near neighbours that cannot share an implementation. Say in one
  line why they cannot.
- **FALSE POSITIVE**: the index describes them alike and the code does different
  work.

`matched_on` says which signal joined a row. `similarity` is the intent vectors;
`intent_text` is two identical intent sentences, which is the signal that still
finds a copy somebody renamed.

## Report

| class | function | file:line | pair | why |
|---|---|---|---|---|
| DUPLICATE | slugify | src/text.ts:12 | src/util/url.ts:4 | same replacement rules |

Then relay the counts the response stated, in its numbers:

- `compared_functions`, and `total_clusters` on an audit;
- `not_compared`: `without_intent`, `intent_not_embedded`, `awaiting_embedding`,
  `model_mismatch`;
- `excluded`: `below_min_lines`, `tests`, `generated`, `callbacks`,
  `other_service`.

Rows outside the comparison were not looked at, so an empty answer covers what
was compared and nothing further.

## Act

Merge nothing unless you were asked to. Offer one issue per DUPLICATE, and file
the ones accepted:

```
gh issue create --title "Duplicate: <behaviour> in <n> files" --body "<each file:line, and which one should remain>"
```
