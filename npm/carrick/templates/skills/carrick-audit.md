---
name: carrick-audit
description: Use when the ask is what is wrong in this codebase, what to clean up, or a health check of the project. Reads what the Carrick index already flags on main (type mismatches between endpoints and their callers, functions written twice), proves each item in source, and returns a fix list. It reads a lot of source, so it uses many tokens; narrow it to one service where you can.
---

# What the index flags on main, proven in source

{{SCOPE_NOTE}}

Cost: this skill opens source for every item it reads. Ten type mismatches and
ten groups of similar functions took between 130,000 and 230,000 tokens on a
project of six services and 2,200 files. Before reading, tell the user how many items each
section holds and offer to narrow to one service or cap the count.

The index states the leads. Your work is to prove or drop each one in source.
Class nothing from the index's own words alone.

## 1. The counts

```
get_project_map({{SCOPE}})
```

Note the counts under FOUND ON MAIN: type mismatches and similar functions.

## 2. Type mismatches

```
get_findings({{SCOPE}}, kind: "type_mismatch")
```

Page until the last page. For each finding, read the producer's handler, the
consumer's call site and the code that uses its result. Where the finding does
not show both types, narrow the pair:

```
get_contract_pair({{SCOPE}}, consumer_service: "<consumer>", producer_service: "<producer>", method: "<METHOD>", path: "<path>")
```

Before you class a finding REAL, grep for a second producer of the same path in
another router or under another prefix. Where one exists, read which one the
consumer calls.

Class each finding:

- **REAL**: the consumer reads a field the producer does not send, or a value
  the producer can send breaks the consumer, such as a null passed to code that
  needs a string. Name the field and the line that breaks.
- **TYPE-ONLY**: the two types disagree, but no value the producer sends can
  break the consumer: a string column holding a fixed set of values against a
  union of those values, a null the database never writes (a default, a check
  that blocks it), a null where every read is guarded. Say which.
- **FALSE**: the index is wrong about one side: the field is sent, or the type is
  misread.
- **CAN'T TELL**: the value comes from somewhere you cannot read, such as a JSON
  column or an outside service. Say where.

The fields a finding names are leads. The field that breaks may be another
one; name the one that does. Where it is not one the finding names, class the
finding by the field that breaks and list the named fields beside it as
TYPE-ONLY.

## 3. Functions written twice

```
get_findings({{SCOPE}}, kind: "similar_functions")
```

Page until the last page, or to the cap the user agreed, and state the cap.
Read every member of each group at its file and line, and diff the bodies.
Class each member against the first:

- **DUPLICATE**: same contract, nothing to name apart.
- **VARIANT**: same contract, and a difference you can name in a clause.
- **FALSE POSITIVE**: different contracts.

Where the members sit in separate apps, check their package.json files for a
package they already share. Where one exists, the fix is to move the function
there and import it; where none does, say so: the fix is a new shared package.

## 4. Report

One table per section, with the class column never empty, and the counts read
against the counts from step 1 and any cap.

Then a fix list: every REAL and DUPLICATE, most harmful first, each with
file:line and the change in one line ("send createdAt from
formatTaskResponse"; "move validateName into a shared package and delete the
two copies"). TYPE-ONLY items go in a short list after it, as optional tidying.

Not covered yet: calls that reach no route and endpoints nobody calls. Say so in
the report.

## 5. Act

Change nothing yet. Ask the user which items on the fix list to apply, and apply
only those.
