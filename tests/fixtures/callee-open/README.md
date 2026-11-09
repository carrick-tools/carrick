# A module-private object read in a service with an unread import (carrick#3504 S4b-3a)

Same `send.ts` and label as `callee-values`. `f-open-trigger.ts` uses an
import only as a shorthand property (`{ send }`), a use no row follows, so
the service is open: any export may be changed by a module the scan cannot
see. A module-private object cannot be reached from another module, so open
does not contest it; an exported one stays contested. Discovery's test
`a_private_objects_entry_is_read_when_an_import_is_unread` reads the calls
at the lines below.

## Answer key

| File | Line | Read | Recorded |
|---|---|---|---|
| `f-keyed-open.ts` | 6 | `d["a"](…)`, `const d = { a: send }` (private) | site, `fetch` |
| `f-keyed-exported-open.ts` | 6 | `export const d = { a: send }` (control) | not decided |
