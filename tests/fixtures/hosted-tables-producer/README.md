# hosted-tables-producer

A hosted database's generated schema type states the tables, views and
functions the platform serves. `supabase/config.toml` selects the layout; the
type is type argument 0 of the `createClient` make of the served package. All
names are invented; the package is a minimal stand-in declaration.

## Answer key (producer rows)

Read with `library-claims.json` replayed as the store's answer (the tests copy
it to `__llm__/library-claims/default.json`): the make claim only.

| Declared at | Row | `resource` |
|---|---|---|
| `src/types.ts:4` | `ALL /rest/v1/accounts` | `{schema: "public", name: "accounts", kind: "table"}` |
| `src/types.ts:5` | `ALL /rest/v1/notes` | `{schema: "public", name: "notes", kind: "table"}` |
| `src/types.ts:6` | `ALL /rest/v1/unused_table` | `{schema: "public", name: "unused_table", kind: "table"}` |
| `src/types.ts:10` | `ALL /rest/v1/rpc/bump_count` | `{schema: "public", name: "bump_count", kind: "function"}` |

- Source `type_resource`; no type entries (no manifest entry, no bundled type).
- `src/client.ts` and `src/admin.ts` make the same type: each key is one row.
- `src/untyped.ts` makes with no type argument: no row.
- Without `supabase/config.toml` there are no rows.
