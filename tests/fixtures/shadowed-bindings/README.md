# `shadowed-bindings`

Fixture for carrick#1648: a name declared again in an inner scope holds its
own value, and no deterministic row may state the outer one there.

## The shape

Every file pairs a site where an inner block (or a parameter, or a closure)
declares a name again with a control site that reads the outer binding. The
scan runs with the model stage off (`CARRICK_NO_MODEL=1`), so every row is one
a deterministic pass states as a fact.

Each source that reads a binding is covered:

| source | how it read the binding before | files |
|---|---|---|
| `request_summary` | a module constant, a function's local or a parameter by name | `module-const.ts`, `function-local.ts`, `var-redeclared.ts`, `parameter.ts` (+ `parameter-caller.ts`), `closure.ts`, `own-fetch.ts` |
| `new_url` (and the imported-member join) | a `new URL(path, base)` binding by name, one frame per function | `new-url.ts`, `new-url-frame.ts`, `member.ts` (+ `member-caller.ts`) |
| `env_base_path` | the env-alias table, keyed by name | `env-base.ts`; control `env-base-control.ts` |
| `whole_url_env` | the env-alias and fallback tables, keyed by name | `whole-url.ts`; control `whole-url-control.ts` |
| `literal_base_path` | the literal-base table, keyed by name | `literal-base.ts`; control `literal-base-control.ts` |

The three table sources read a name only where the file declares it once
(`CandidateTarget::url_binding`), so their controls sit in files of their
own.

## The answer key

No row at the shadowed sites:

| site | what the call sends | wrong fact stated before |
|---|---|---|
| `module-const.ts:6` | `POST /api/admins` | `POST /api/users` |
| `function-local.ts:5` | `POST /api/admin-items` | `POST /api/items` |
| `var-redeclared.ts:6` | `/api/var-first` or `/api/var-second` | `POST /api/var-first` |
| `parameter-caller.ts:4` | `DELETE /api/param-admins` | `DELETE /api/param-users` |
| `closure.ts:7` | `PATCH /api/archived-orders` | `PATCH /api/orders` |
| `own-fetch.ts:6` | nothing (a local function named `fetch`) | `GET /api/reports` |
| `new-url.ts:6` | `POST /api/new-url-raw` | `POST /api/new-url-module` |
| `member.ts:6` | `PUT /api/member-local` | `PUT /api/member-module` |
| `member-caller.ts:4` | `PUT /api/member-local` | `PUT /api/member-module` |
| `env-base.ts:6` | `POST /internal-prefix/users` | `POST ${process.env.API_URL}/users` |
| `whole-url.ts:6` | `POST /api/local-answer` | `POST ${process.env.HELPDESK_URL}/api/answer` |
| `literal-base.ts:6` | `DELETE http://other.example.com/status` | `DELETE http://api.example.com/status` |

The shadowed sites state nothing rather than the inner value: a block's own
declarations are not read, so the site is left to the model's row.

These rows are stated:

| site | row | source |
|---|---|---|
| `module-const.ts:12` | `PUT /api/users` | `request_summary` |
| `function-local.ts:7` | `GET /api/items` | `request_summary` |
| `parameter-caller.ts:5` | `PATCH /api/param-archive` | `request_summary` |
| `closure.ts:9` | `GET /api/orders` | `request_summary` |
| `own-fetch.ts:12` | `GET /api/reports` | `request_summary` |
| `new-url.ts:12` | `GET /api/new-url-module` | `new_url` |
| `new-url-frame.ts:7` | `POST /api/frame-outer` (was `/api/frame-inner`) | `new_url` |
| `member.ts:12` | `GET /api/member-module` | `new_url` |
| `member-caller.ts:5` | `GET /api/member-module` | `request_summary` |
| `env-base-control.ts:4` | `GET ${process.env.ACCOUNTS_URL}/accounts` | `env_base_path` |
| `whole-url-control.ts:4` | `POST ${process.env.TICKETS_URL}/api/tickets` | `whole_url_env` |
| `literal-base-control.ts:4` | `GET http://status.example.com/health` | `literal_base_path` |
