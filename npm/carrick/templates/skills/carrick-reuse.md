---
name: carrick-reuse
description: Use at the end of a task that added or changed functions, and whenever the ask is whether something already exists or whether this code duplicates code elsewhere. Compares against the Carrick function index rather than by name. For where the whole project has built the same thing twice, use carrick-audit.
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

Functions you added in this task are not in the index yet, so give each one a
`description` entry: one plain sentence about what it does, written from the
code you wrote. Do not look for the names in a file list; you wrote them.

Two limits decide what an empty answer means. The index was taken at the
commit the answer states, on the default branch, so anything added on this
branch is compared against that. And the comparison is on what each function is
described as doing, not on its source: the index holds no body hash.

When you are done, if Carrick found a problem or changed what you did, tell the user in one line. Otherwise do not mention it.

The two kinds are scored on different scales, and each result states the floor
it was ranked against. `vector_basis` says what the cosines are over. On
`intent` the vector is the intent sentence alone, and a copy somebody renamed
scores as close as one that kept its name. On `name_anchored` the function's
name sits in front of the sentence, a renamed copy scores lower, and
`intent_text` is the signal that still finds it. Read every score against the
floor and the basis in the answer you got.

## The whole project

Where the ask is where the whole project has built the same thing twice, or
which duplicates are worth fixing, this skill is the wrong tool: one run cannot
page the whole list and open every member. Load the carrick-audit skill, which
reads every group in units, finds routes written twice and jobs done several
ways, and says what it will cost first. `find_similar({{SCOPE}}, audit: true,
limit: 1)` gives the number of groups, in `total_clusters`, if you want it
before you hand over.

## Class every row

Every row the answer returned is classed here, against the function you asked
about. Read that span at the file and line the
response gave, read each other row the same way, and take the first of these
that holds:

- **FALSE POSITIVE**: the two contracts differ. Different inputs, a different
  result, or a different effect, and the intent sentences alone brought them
  together.
- **VARIANT**: one contract, and a behavioural difference you can name in a
  clause. A different normalisation, a different error path, a different
  default. Write the clause in the row. Where a comment on the member or at the
  head of its file names the file it mirrors, the clause is "documented
  mirror".
- **DUPLICATE**: one contract, and nothing left to name. Two bodies that run
  the same once the identifiers are renamed land here.

`matched_on` says which signal joined a row. `similarity` is the intent vectors;
`intent_text` is two identical intent sentences, which is the signal that still
finds a copy somebody renamed.

## Report

One row per match. The class column carries one of the three words and is
never empty.

| class | member | file:line | against | why |
|---|---|---|---|---|
| DUPLICATE | slugify | src/util/url.ts:4 | src/text.ts:12 | same replacement rules |
| VARIANT | slugTag | src/tags.ts:20 | src/text.ts:12 | documented mirror |

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
