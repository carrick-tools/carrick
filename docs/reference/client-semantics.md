# Client-library semantics

How the scanner reads a call made through a data-fetching package's client
(carrick#1564). The pinned contract is the comment on that issue; this page
says how the scanner side works.

## The question

```ts
import http from "@fixture/http";
const api = http.create({ baseURL: "/api/v1" });
api.post("/orders", { action: "create" });
```

Every literal in that request is in the source. What the source does not say
is what `create` does with `baseURL`, that `post` sends a `POST`, and that its
second argument is the body. That is knowledge about the library.

## Where the answer comes from

1. **Framework detection answers it.** The scanner sends
   `ask_client_semantics: true` on `/framework-detect`, and the answer carries
   `client_semantics`: for each `data_fetchers` package, per major version,
   its factories and their base-URL key, its verbs, and its request members.
   The field is parsed one element at a time and can never fail a detection.
2. **The scanner derives claims** with ids the model never writes
   (`<package>@<major>:<export>:<kind>:<member>`), and pairs each claim with
   the receivers it is checked on: a factory on the export, everything else
   on the export and on one instance per factory.
3. **The type sidecar checks every pair** against the package's own
   declarations, installed under the service root, in one request per
   service. A pair is used only when it comes back `verified`, and an
   instance only when its factory's own claim verified. `failed` and
   `unchecked` are dropped, never demoted: the site reads as it would
   without the semantics. Verification runs on every scan and is never
   cached, because it depends on `node_modules`.

Code: `src/client_semantics.rs` (wire shape, claims, the verified surface,
the re-ask rules and the in-scan schedule) and `src/request_summary.rs`
(reading calls through it).

## How a call is read

- **The receiver is named by the syntax.** A binding imported from the
  package (`import`, or `require`), or a binding or class field initialised
  by the export's factory with one object literal and never reassigned. A
  name the file declares again anywhere below module scope is no client in
  that file. A field written anywhere but the constructor's own statements,
  or declared again by a subclass in the file, holds no client, and a static
  member reads no instance field. Nothing is inferred per site, so `new
  Map().get("/r")` reaches no claim.
- **The client is used only to call through it.** A binding the file uses
  in any other way holds no client: a member read that is not called
  (`api.defaults`), a write through it, an argument, an alias, a spread, a
  shorthand property. Exporting it (`export default api`, `module.exports.api
  = api`) is fine. An instance also holds no client when its export's
  binding is used that way, since a write to the export's defaults before
  the factory runs reaches the instance.
- **The base** is the factory options' value at the verified key, joined to
  the path with exactly one slash. A path that is an absolute URL ignores
  it. An empty base is no base, and a base holding `?` or `#` states no URL.
  A key written after every spread is read however open the options are; a
  key a later spread, a getter, a computed key or a conditional spread
  (including `cond && {…}`, which may spread nothing) may overwrite is not.
  Nothing is read where the source may set the base elsewhere: factory
  options open to a spread that do not name the key after it, or a call's
  options or config that are open or name a base key.
- **A spread of a constant** puts its keys in place only when the constant
  is declared once in the file, as `const` holding one object literal, and
  the file never writes through it, passes it to a call or aliases it. Any
  other spread may carry any key.
- **The method** is the verb claim's, or the literal a request call writes
  at the method key. No literal, no row: a library's default is never
  assumed.
- **The body** is read only through a verified body claim.

A call read this way is a `request_summary` row that names the claim ids it
used in `library_semantics`. It states a row at its own site, which a verb
call otherwise never does, and composes across functions and modules like
any other summary.

The summaries are composed after detection, not in discovery: the semantics
exist only once detection has answered, any package it left `pending` has
been asked about again (below), and the service's sidecar is up.

## Asking again

**Within a scan.** When detection leaves an installed package `pending`, the
scanner asks again after 5 s and, if one is still pending, after another
15 s: at most three asks per service per scan
(`PENDING_REASK_WAITS`). Each re-ask is one HTTP attempt, a lease wait
included, bounded by 30 s (`PENDING_REASK_TIMEOUT`), and the schedule stops
as soon as nothing installed is pending. It is started as soon as the
service's detection is known and runs beside the file analysis, which waits
for it only before composing the summaries. The first answer for a package
stands, so the rows do not depend on which ask gave it. A failed ask, or
one naming other packages, changes nothing. A `skipped` package is never
asked about, and neither is a `pending` one that is not installed. The user
reads one line before each re-ask and, if any remain, one with how many are
not described yet.

**On the next scan.** There is no cache-version bump. A stored detection
with no `client_semantics` is asked again when one of its data fetchers is
installed, and one with an entry still `pending` when that package is
installed. The ask comes before the analysis, is one HTTP attempt bounded by
30 s, and prints its line first. If the answer names the same packages in
all four lists, the stored guidance stands and only the semantics are
taken; if not, guidance is asked again from the new answer. A failed or
unanswered ask keeps the stored detection. That ask counts as the scan's
first, so the schedule after it makes at most two more.

A run retrying its own owed work asks neither way: its detection is minutes
old. A service whose model stages were deferred has no detection to settle.

**The most it can add to a scan is 110 s**: 30 s for the ask on a stored
detection, before the analysis, then 5 + 30 + 15 + 30 s for the schedule,
beside the analysis. The schedule adds only what outlasts the analysis.

## Limits

- An instance exported from one module and imported by another is not read
  as a client in the importing module (carrick#1568).
- A base that is an absolute URL written as a literal gives no library row,
  because a summary row states only a path behind an opaque base
  (carrick#1569). The call keeps the reading it has without the semantics,
  which can be a receiver-type fact stating the path without the base
  (carrick#1583).
- A verb whose member name is not the HTTP method is not read (carrick#1566).
- A method call through the client other than a verb is assumed not to
  change its base (a setter such as `api.setBase("/v2")` is not recognised).
- Shapes that lose a reading to the rules above: a client whose defaults
  the file configures (`api.defaults.headers.common.X = …`, or through the
  export), a client handed to a helper or kept in an object, one read by a
  `typeof` or an `instanceof`; a spread of a constant the file also passes
  to a call (`fetch(URL, OPTS)` elsewhere in the file), exports as
  `module.exports = { OPTS }`, or declares again under the same name in
  another scope; a spread of anything but a plain identifier
  (`...this.opts`, `...config.defaults`).
- A key written before a spread whose declared type cannot carry it is not
  stated: only the type says so, and a type is not what the object holds at
  run time (carrick#1585).
- A row read this way states a dispatch value only from its body. A
  producer that dispatches on a header reads the row as unmatched.
