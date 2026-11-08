# declared-dispatch-handlers

Fixture for `tests/declared_dispatch_handlers_test.rs` (carrick#2076). All
names are invented.

Three services. Two are routeless functions that read a field off the request
body and switch on it; `carrick.json` declares each case with an
`operations` block and names its handler as `file:symbol`. The third calls
them with `fetch`.

- `ledger-fn`: `POST /ledger`, field `op`. `credit` is a function
  declaration, `debit` an arrow const. Each takes its own command type and a
  shared `Ctx`, and returns a plain value the platform serialises.
- `notes-fn`: `POST /notes`, field `kind`. Handlers return an envelope
  `{ statusCode, headers?, body: string }`, built by a local `reply(status,
  payload)` or written inline.
  - `add`: `addNote(input: AddNoteInput, ctx)`, a 400 and a 201 branch.
  - `list` and `search`: one handler, `listNotes`, with an inline
    `JSON.stringify`.
  - `purge` and `archive`: two handlers that both take `NotesInput`, the
    dispatcher's own type, so neither states a request of its own.
    `archive` returns a pre-built envelope by name.
  - `export`: names a symbol the file does not define.
- `notes-web`: one `fetch` per case it uses (`credit`, `add`, `list`,
  `purge`, `archive`), each body a typed `const`, each reply read into a
  declared type. `AddReply` is wrong on purpose. One call (`replay`) states
  no case.

## Answer key

| case | row at | request | response |
|---|---|---|---|
| `credit` | `ledger-fn/src/index.ts:36` | `CreditCommand` | `CreditResult` |
| `debit` | `ledger-fn/src/index.ts:41` | `DebitCommand` | `DebitResult` |
| `add` | `notes-fn/src/index.ts:48` | `AddNoteInput` | `AddNoteResult` (400 dropped) |
| `list`, `search` | `notes-fn/src/index.ts:55` | `ListNotesInput` | `{ notes: Note[] }` |
| `purge` | `notes-fn/src/index.ts:61` | unknown | `{ purged: number }` |
| `archive` | `notes-fn/src/index.ts:67` | unknown | unknown, `serialised_body_unread` |
| `export` | `notes-fn/src/index.ts:0` | unknown | unknown |

Verdicts at the consumer: `credit` (`api.ts:50`) compatible on both halves;
`add` (`api.ts:57`) response incompatible; `list` (`:64`) and `purge`
(`:71`) responses compatible.

A handler named twice in one file is not covered: discovery keys a file's
definitions by name, so two same-named functions in one file are one row
there, as `handler_span` reads them too.
