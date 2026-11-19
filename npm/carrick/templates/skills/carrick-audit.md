---
name: carrick-audit
description: Use when the ask is what is wrong in this codebase, what to clean up, or a health check of the project. Reads what the Carrick index already flags on main (type mismatches between endpoints and their callers, functions written twice), proves each item in source, and returns a fix list. It reads a lot of source, so it uses many tokens; narrow it to one service where you can.
---

# What the index flags on main, proven in source

{{SCOPE_NOTE}}

Cost: this skill opens source for every item it reads. On a monorepo of six
services and about 1,600 TypeScript files, ten type mismatches and ten groups of
similar functions took one headless Sonnet run 85 to 100 seconds and 39 to 44
turns: about 110,000 new input tokens, 1.1 to 1.3 million cached input tokens
and about 12,000 output tokens (2 runs, 19 Nov 2026).

The index states the leads. Your work is to prove or drop each one in source.
Class nothing from the index's own words alone.

## 1. The counts

```
get_project_map({{SCOPE}})
```

Note the counts under FOUND ON MAIN: type mismatches and similar functions.

Before you open any source, write the two counts and the cap you will use. Where
you can ask, offer to narrow to one service or cap the count. Where you cannot
wait for a reply, such as a non-interactive run, the run caps each section at
10, says so, and carries on.

## 2. Type mismatches

```
get_findings({{SCOPE}}, kind: "type_mismatch")
```

Copy the first line of the answer (`FINDINGS · project <slug> · index at
<commits> · built <day>`) to the top of the report, word for word. The report's
first line names the index commit and the day the list was built.

When the answer is `No findings list has been built for this project yet.`,
stop the audit: the similar-functions list in step 3 comes from the same list.
Tell the user in one sentence that the index has not built
its findings list yet and that the list is built after a scan of main. Do not
look for issues by hand and present them as the audit.

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

When a command or tool call you need is refused or fails, class any finding you
cannot settle without it CAN'T TELL with the reason `refused: <the command>`.
List every refused call under the tables.

Name the protocols the findings came from (HTTP, GraphQL, sockets, pub/sub,
queues, RPC routers) and the ones that held none.

Not covered yet: calls that reach no route, endpoints nobody calls, and routes
that change data without an auth check. Say so in the report.

## 5. Act

Change nothing yet. Ask the user which items on the fix list to apply, and apply
only those.
