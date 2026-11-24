---
name: carrick-audit
description: Use when the ask is what is wrong in this codebase, what to clean up, or a health check of the project. A full audit from the Carrick index on main (calls that reach no route, type mismatches between endpoints and their callers, functions written twice, routes written twice, one job done several ways), with each item proven in source and a fix list at the end. It states its estimated tokens and minutes before it reads any source; narrow it to one service to spend less.
---

# A full audit from the index, proven in source

{{SCOPE_NOTE}}

Three rules hold from the first call to the last.

- **Leads come from the index.** Every item starts as a row a Carrick call
  returned. Do not read through the code looking for problems. Open source only
  to prove or drop a row: the lines the row names, and a grep for a path or a
  name the row names.
- **Nothing is classed from the index's words alone.** Every item you call real
  quotes the source line that makes it real. Say what code does only where you
  can quote the line that does it; a comment or a log line is not behaviour.
- **Nothing is skipped or cut short.** Every section runs. Each list is paged to
  its end. Each section ends with a count line: rows the index returned against
  rows you classed. The two numbers are equal, or the line says which call was
  refused.

Run one command per shell call, with no `;`, `&&`, `|` or `$(...)`: a
non-interactive run refuses compound commands.

## 1. Count, estimate, ask

Four calls. Open no source yet.

```
get_project_map({{SCOPE}})
get_service_graph({{SCOPE}}, limit: 1)
find_similar({{SCOPE}}, audit: true, limit: 1)
get_findings({{SCOPE}}, limit: 1)
```

Read these counts out of the answers:

- **Calls that reach no route**: in the graph's `unmatched_by_reason`, the sum
  of `no_matching_producer`, `no_case_answers` and `procedure_unserved`. Keep
  `unmatched_by_service` too: section A reads those services.
- **Pairs**: each consumer and producer pair under the map's CONSUMERS. Where
  the map cuts the list short, call it again with `detail: true`.
- **Groups of similar functions**: `total_clusters` from `find_similar`.
- **Operations served**: the endpoint count per service in the map's SERVICES.
- The first line of the `get_findings` answer (`FINDINGS · project <slug> ·
  index at <commits> · built <day>`): copy it to the top of the report, word
  for word. Where the answer says no findings list has been built yet, say so
  in the report and carry on: the sections below ask the index directly.

Work out the estimate from the counts. New tokens are input read for the first
time plus output:

| Section | Per item | New tokens |
|---|---|---|
| Counting and the report | once | 80,000 |
| A. Calls that reach no route | each service with unmatched calls | 20,000 |
| | each call that reaches no route | 6,000 |
| B. Type verdicts | each pair | 16,000 |
| C. Functions written twice | each group | 3,500 |
| D. Routes written twice | each 100 operations served | 90,000 |
| E. One job, several answers | once | 110,000 |

Each turn also re-reads what the run already holds from the cache: about six
times the new tokens again, at a tenth of the price. Time is about 13 minutes
for each million new tokens when the sections run one after another, and about
half of that with sub-agents. These figures were measured with Claude Sonnet
on a monorepo of 17 packages, 208 operations and 4,500 functions (24 Nov 2026):
about 1 million new tokens and 5 minutes a run with sub-agents.

The first thing the user reads from you is this block, filled in. Write it as
text in your reply, not only in your reasoning, before any source is opened:
ahead of the first call of section 2, and at the latest as your first words
once the first units have started and before any has returned.

```
Carrick audit: estimate, before any source is read
- Calls that reach no route: <n>, in <n> services with unmatched calls
- Consumer and producer pairs: <n>
- Groups of similar functions: <n>
- Operations served: <n>
Estimate: about <total> new tokens (A <n>, B <n>, C <n>, D <n>, E <n>, counting and report <n>)
Time: about <n> minutes with sub-agents, about <n> one section after another
```

After the block, one of two things:

- You have a tool for putting a question to the user (such as
  `AskUserQuestion`): use it to ask whether to go ahead or to narrow the audit
  to one service or to some sections, and wait for the answer.
- You have no such tool: nobody can answer, so do not end your turn on a
  question. Carry on with every section in the same turn.

There is no cap in either case.

## 2. Run every section

Sections A to E do not depend on each other. Split them into units:

- **A**: one unit for each service in `unmatched_by_service` with more than 30
  unmatched calls, and one unit for all the other services in it together.
- **B**: one unit for every 10 pairs.
- **C**: one unit for every 20 groups: offsets 0 to 19, 20 to 39, and so on to
  `total_clusters`.
- **D**: one unit.
- **E**: one unit.

Where you can start sub-agents, give each unit to its own sub-agent and start
them together, six at a time at most. The brief for each is these words with
the unit filled in, and nothing else:

> Load the carrick-audit skill. Run only section <letter> of it, for <the
> unit: its services or pairs, each named as the index spells it, or its
> offsets>. Work to the end of the unit
> and return its table, its count line and any refused calls. Run no other
> section. Do not estimate, do not ask the user anything and do not start
> sub-agents.

Where you cannot start sub-agents, run the units yourself in the order above,
and write each unit's table and count line to the user before you start the
next unit.

A unit is done when it has returned its count line and the two numbers in it
are equal. Run again any unit that came back without one.

## A. Calls that reach no route

For each service in the unit:

```
get_service_graph({{SCOPE}}, service: "<service>", limit: 200)
```

Read `unmatched_calls` and nothing else: the other arrays are for other
questions, so never page for them. Page with `offset` until you have seen as
many unmatched rows as `unmatched_count`. Take only the rows whose `reason` is
`no_matching_producer`, `no_case_answers` or `procedure_unserved`: each names
an operation no indexed service serves. The other reasons go to third parties
or to a base Carrick has not placed, and are not evidence of a broken call.

For each row you took, open the call site at its file and line. Then ask the
index for the nearest thing that does exist, once for each service that serves
operations, until one answers:

```
get_api_endpoints({{SCOPE}}, service: "<producer>", path_contains: "<last segment of the path, or the operation, event or topic name>")
```

Then grep the workspace for the last two segments of the path, or for the
name, to see whether a route, a proxy rule or a rewrite serves it somewhere the
index did not read.

Class each row:

- **REAL**: the line makes this call and nothing in the workspace serves it.
  Say what stops working for the user, and name the nearest route the index
  holds, such as the same handler under another prefix. A relative path a
  browser sends to its own origin is REAL when no route, proxy rule or rewrite
  in the workspace answers it.
- **FALSE**: the line does not make this call (a `candidate: model` row whose
  path is not in the source), or a route does serve it. Name the route's file
  and line.
- **OUTSIDE**: the call is meant for something this project does not hold, such
  as a third party or a service that is not indexed. Say which.
- **CAN'T TELL**: you cannot settle it. Say what you looked for.

Count line: `A <services>: <rows taken> no-route rows, <rows classed> classed,
<unmatched rows seen> of <unmatched_count> unmatched rows read`.

## B. Type verdicts, pair by pair

The findings list can hold no type mismatch while a pair still holds a stored
verdict, so read the verdicts from each pair:

```
check_compatibility({{SCOPE}}, consumer_service: "<consumer>", producer_service: "<producer>", limit: 50)
```

Note `status`, `pairs_compared` and `pairs_uncompared`. Errors come first in
`issues`: take every row whose severity is `error`, and page with `offset`
while the last row of a page is still an error. Where `candidate_findings` is
above 0, page on and take those warning rows too. Leave the other warnings and
the unused endpoints.

An error row that says a call matches no operation is classed as in section A.
For an error row that says two types disagree, read the producer's handler, the
consumer's call site and the code that uses its result. Where the row does not
show both types, narrow the pair:

```
get_contract_pair({{SCOPE}}, consumer_service: "<consumer>", producer_service: "<producer>", method: "<METHOD>", path: "<path>")
```

Before you class a row REAL, check for a second producer of the same path with
`get_operation({{SCOPE}}, method: "<METHOD>", path: "<path>")`. Where one
exists, read which one the consumer calls.

Class each type row:

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

The fields a row names are leads. The field that breaks may be another one;
name the one that does, and list the named fields beside it as TYPE-ONLY.

Count line: `B: <pairs read> of <pairs> pairs, <error rows> error rows,
<rows classed> classed, <sum of pairs_compared> calls compared, <sum of
pairs_uncompared> not compared`. A call that was not compared has no verdict,
which is not agreement.

## C. Functions written twice

```
find_similar({{SCOPE}}, audit: true, offset: <first offset of the unit>, limit: 20)
```

A page can come back shorter than its limit. Follow `next_offset` until you
hold every group of the unit. Read every member of every group at its file and
line, also in a group that lists its members as locators only, and diff the
bodies. Class each group:

- **DUPLICATE**: the same job and the same result. Nothing to name apart.
- **DIVERGED**: the same job, the same kind of input and the same kind of
  result, and for one input the members give different results that a user or
  another system can see. Quote the line that differs in each member, and write
  one input with each result beside it ("1234.5 comes out as `1,234.50` here
  and `1234,5` there"). Work each result through the code line by line; where
  you cannot, describe the difference in words and write no example. Say where
  each result is shown.
- **VARIANT**: the same job with a difference that is there on purpose or that
  changes no result: an extra parameter, another unit, a format an outside
  service asks for, names that state the difference (`fetchJson` beside
  `fetchJsonWithAuth`). Quote the line that differs. If you cannot quote one,
  the class is DUPLICATE.
- **FALSE POSITIVE**: different jobs, a different call signature (the
  parameters a caller passes), a different kind of result, or handlers that
  read their own component's state or props and so cannot be shared.
- **CAN'T TELL**: you could not read a body or a helper it depends on. Say
  which. A group you have not proven to the end is never DIVERGED.

Class what the function does: its body and the constants and helpers it reads.
Other code in the same file that it does not read does not count.

For a DUPLICATE or DIVERGED group whose members sit in separate apps, check
their package.json files for a package they already share. Where one exists,
the fix is to move the function there and import it; where none does, say so:
the fix is a new shared package.

Count line: `C <first offset>-<last offset>: <groups returned> groups,
<groups classed> classed`.

## D. Routes written twice

Route handlers are left out of section C's list. Ask for them, once for each
service that serves operations:

```
find_similar({{SCOPE}}, audit: true, include_callbacks: true, service: "<producer>", limit: 20)
```

Page with `next_offset` to the end. Take only the groups where two or more
members are route handlers in different files: a route handler's name starts
with its method or operation kind and ends in `_handler`, such as
`post_orders_handler`. Leave every other group: section C covers the named
functions, and callbacks inside one function are not routes.

For each group you took, find the full path of each handler and who calls it:

```
get_api_endpoints({{SCOPE}}, service: "<producer>", path_contains: "<a word of the route's path>")
get_operation({{SCOPE}}, method: "<METHOD>", path: "<full path>")
```

One file and line listed under two prefixes is one router mounted twice, which
is not a copy. `consumer_count` on each operation says which copy the apps
call. Then read both handlers to their last line and diff them line by line.
Name a difference only where you quote the line from each copy; a line that is
only a comment or a log in one copy is not a difference in what the route
does. Class each group:

- **WRITTEN TWICE, DIFFERS**: two handlers serve the same thing and the bodies
  differ in what they do. Quote a line that differs, and say which copy the
  apps call and which has no recorded caller.
- **WRITTEN TWICE, SAME**: the same logic in two files.
- **NOT A COPY**: the handlers serve different things.

Count line: `D: <groups returned> groups read, <handler groups> with route
handlers in two files, <groups classed> classed`.

## E. One job, several answers

Functions that do one job and give different results are the ones a user
sees, and they rank low on a list of look-alikes. Ask the function index for
each of these jobs, one call each:

```
search_by_intent({{SCOPE}}, query: "<job>", also_phrased_as: ["<the same job in other words>"], top_k: 30, compact: true)
```

| Job | In other words |
|---|---|
| format an amount of money as a currency string for display | turn a number of cents into a price string with a currency symbol |
| format a date or a time for display | turn a timestamp into a readable date string |
| validate or normalise a phone number | mask or format a phone number for display |
| validate an email address | check that a string is a well-formed email |
| check whether the current user may do something | check a role or a permission before an action |
| build the page or cursor arguments for a list request | encode or decode a pagination cursor |
| report an error to a tracking service | send a client error to the server |
| build the headers or base address for a call to this project's own API | wrap fetch with authentication and error handling |

In each answer, flag a set of rows when one name appears in two or more files,
or when three or more rows in one service do the job asked for. Rows that only
share a word with the query are not flagged. Read every flagged function at its
file and lines. Class each set with section C's classes: DIVERGED, DUPLICATE,
VARIANT or FALSE POSITIVE. A DIVERGED set needs the same proof: the lines that
differ, and one input with each result beside it.

Count line: `E: 8 jobs searched, <sets flagged> sets flagged, <sets classed>
classed`.

## 3. Report

Before you write, check that you hold a count line for every unit of every
section and that its numbers are equal. Run now any unit that is missing.

The report, in this order:

1. The `get_findings` first line. Then the estimate block you gave at the
   start, again. Then one line per section: rows the index returned, rows classed,
   and how many are in the fix list.
2. **The fix list.** Every REAL, DIVERGED, WRITTEN TWICE and DUPLICATE item,
   most harmful first: what a user can hit comes first (a REAL call that reaches
   no route, a REAL type break), then DIVERGED, then WRITTEN TWICE, DIFFERS,
   then WRITTEN TWICE, SAME, then the copies, largest group first. Each item
   has its class, every file and line, what the user sees or what it costs, and
   the change in one line ("send createdAt from formatTaskResponse"; "move
   validateName into a shared package and delete the two copies"). An item two
   sections found is one item. An item a unit could not prove to the end is not
   in the fix list: it is CAN'T TELL.
3. TYPE-ONLY and VARIANT items in a short list, as optional tidying.
4. **Everything else the index returned**, section by section, so every row is
   accounted for: each FALSE, FALSE POSITIVE, OUTSIDE, NOT A COPY and CAN'T
   TELL row by name, with its class and the reason in a few words.
5. **What the index could not tell you**: calls compared against calls not
   compared, from section B; the call sites Carrick could not resolve, with the
   three largest reasons from `unread_sites_by_reason`; and the rows only a
   model states, from the map.
6. Every refused or failed call, including any you then worked around. An item
   you could not settle without it is CAN'T TELL with the reason `refused: <the
   command>`.
7. The protocols the items came from (HTTP, GraphQL, sockets, pub/sub, queues,
   RPC routers) and the ones that held none.

Not covered yet: routes nobody calls that have no second copy, and routes that
change data without an auth check. Say so in the report.

## 4. Act

Change nothing yet. Ask the user which items on the fix list to apply, and apply
only those.
