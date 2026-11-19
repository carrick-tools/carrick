# hosted-functions

Functions served one per folder: `supabase/functions/<name>/index.ts` is
served at `/functions/v1/<name>` for every method, by the handler the module
hands to its one top-level call. `supabase/config.toml` selects the layout.
All names are invented.

## Answer key (producer rows)

| File | Row | Handler |
|---|---|---|
| `supabase/functions/price-quote/index.ts` | `ALL /functions/v1/price-quote` | inline, line 8 (`serve` imported by URL; replies 400 or 200) |
| `supabase/functions/team-digest/index.ts` | `ALL /functions/v1/team-digest` | inline, line 1 (`Deno.serve`) |
| `supabase/functions/purge-sessions/index.ts` | `ALL /functions/v1/purge-sessions` | `purgeSessions`, line 3 (nobody calls it) |
| `supabase/functions/send-invite/index.ts` | `ALL /functions/v1/send-invite` | not located: `serve(handleInvite)` hands an imported function. Unread site, slot `handler`, reason `handler_not_located`, line 4, naming `handleInvite` |
| `supabase/functions/report-export/index.ts` | `ALL /functions/v1/report-export` | not located: no serving call. Unread site, slot `handler`, reason `handler_not_located`, line 1 |
| `supabase/functions/send-invite/handler.ts` | no row | not an entry file |
| `supabase/functions/_shared/*` | no row | shared code, never a function |

- A row whose handler is not located asks for no request or response type.
- The model's guess `POST /api/quote` for `price-quote/index.ts`
  (`__llm__/analyze-file/price-quote__index.ts.json`) folds onto the
  `ALL` row: it is never a second row, and its anchors travel onto it.
- Without `supabase/config.toml` no file states a row.
- The same rows come out with `carrick.json` (the folder is its own service)
  and without it (one service at the root).

## Answer key (consumer rows)

Read with `library-claims.json` replayed as the store's answer (the tests
copy it to `__llm__/library-claims/default.json`): the `fetch` claim, and
`createClient` of `@supabase/supabase-js` making a client on argument 0.
`src/client.ts:5` makes that client on `VITE_SUPABASE_URL`, and the repository
serves the platform's functions (`supabase/config.toml`), so that variable is
the repository's own address, read as a declared `internalEnvVars` entry is.

| File | Row |
|---|---|
| `src/quote.ts:2` | `POST /functions/v1/price-quote`, one row (the model's reading in `__llm__/analyze-file/quote.json` and the base-path reading fold onto it), pairing with the `ALL /functions/v1/price-quote` row |
| `src/other.ts:2` | `POST`, still keyed on its `VITE_OTHER_URL` env base and `/x`: no client is made on that variable |
| `src/client.ts:5` | no row (the client is made, nothing is sent) |

- The scan's run log says what it learned: `Treating env var
  VITE_SUPABASE_URL as internal because the @supabase/supabase-js client
  created at src/client.ts:5 uses it as its base address and this
  repository serves that platform's functions`.
- Without `supabase/config.toml` nothing is learned and `src/quote.ts:2`
  keeps its env base.
- A second scan sharing the store and cache keys the same.
