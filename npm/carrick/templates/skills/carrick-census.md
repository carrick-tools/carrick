---
name: carrick-census
description: Use for every place that does X questions, such as every service that reads this environment variable, every handler that checks a permission, or every client of this queue. Pages the Carrick intent index under two wordings and reports what it read and what it could not see.
---

# Every place that does X

{{SCOPE_NOTE}}

The index produces the list. Your work is to open each row and confirm what it
does.

## 1. Two wordings

Ask in two wordings. One says what the code is for, the other says how it does
it. A purpose wording misses a helper whose description names only its
mechanism, and a mechanism wording misses one described only by its job.

```
search_by_intent({{SCOPE}}, query: "<what it is for>", also_phrased_as: ["<how it does it>"], compact: true, top_k: 20)
```

`compact: true` returns locator-only rows, which is the shape a census needs.
One answer holds both wordings, deduped, with `phrasings` naming them and
`matched_phrasings` on each row saying which ones reached it. Page it while
`has_more` is true:

```
search_by_intent({{SCOPE}}, query: "<the same query>", also_phrased_as: ["<the same second wording>"], compact: true, top_k: 20, offset: <next_offset>)
```

Where the answer carries no `phrasings`, or the field comes back refused, the
server answering takes one wording per call. Run the second wording as its own
paged search.

Stop when `has_more` is false. Lower `similarity_threshold` where the tail of a
page is still on topic.

## 2. Union

One answer over both wordings is already deduped, and `matched_phrasings` says
which wordings reached each row. Two answers are joined here, on `file_path` and
`line_number`, and a row both wordings found is one row. Either way keep
`retrieved_by` and `similarity`, and keep which wording found each row. A row
only one wording reached is the row a single search would have lost.

## 3. Receipt

Report these numbers before the list, per query where the field is per query:

- rows read, which is the count you paged to;
- `total_candidates`, the exact size of the ranked list for that query;
- `hidden_by_threshold` where the answer carries it: how many rows the floor
  removed, and `best` where it names the closest of them. A count above zero is
  the case for one more search at a lower `similarity_threshold`;
- `total_without_intent`, which is index-wide: functions carrying no intent
  text, which the search ranked on their name, signature and body tokens
  alone, so a query that names one finds it and a query that describes what
  it does without naming it does not;
- `total_intent_carried_forward` where the response states it, which are intents
  describing the code as of an earlier scan.

Then say what the index does not hold for this question. It covers each repo's
main branch, so a change on a branch is outside it. It indexes functions that
carry an intent, so a match inside a config file, a template or generated output
is outside it as well.

## 4. Confirm

Read each row at `file_path`, from `line_number` to `end_line`. Keep the rows
that do the thing, and drop the rest with one line each saying what they turned
out to be. `role` on a row says what the scan joined it to, such as a route
handler or a client, and a route handler for a different operation is not a hit.

## 5. Report

| file:line | function | what it does | found by |
|---|---|---|---|
| src/billing/charge.ts:88 | chargeCard | reads the billing key | purpose, mechanism |

Close with the receipt from step 3, so the list is read against what was
searched.

## 6. Act

Change nothing unless you were asked to. Offer one issue per finding that needs
work, and file the ones accepted:

```
gh issue create --title "<concept>: <n> places, <m> need changing" --body "<the table, and the receipt>"
```
