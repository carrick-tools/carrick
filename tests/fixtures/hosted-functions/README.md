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
| `supabase/functions/report-export/index.ts` | no row | none: one unread site, slot `handler`, reason `handler_not_located` |
| `supabase/functions/_shared/*` | no row | shared code, never a function |

- The model's guess `POST /api/quote` for `price-quote/index.ts`
  (`__llm__/analyze-file/price-quote__index.ts.json`) folds onto the
  `ALL` row: it is never a second row, and its anchors travel onto it.
- Without `supabase/config.toml` no file states a row.
- The same rows come out with `carrick.json` (the folder is its own service)
  and without it (one service at the root).
