# receiver-role-split

One service, two library packages, every name invented. The two packages
declare a member of the same name, `get(path, ...)`. `library-claims.json` is
the library store's answer: `role-server` is a server framework whose `get`
receives a route, `role-fetcher` is an HTTP client whose `get` requests one.

Two pairs of sites are written alike (the same verb, the same literal, a path
and one more argument) and differ only in what the receiver is:

| Site | Receiver | Answer |
|---|---|---|
| `src/app.ts:10` `app.get("/widgets", ...)` | built by the server package's maker | route `GET /widgets` |
| `src/app.ts:11` `api.get("/widgets", ...)` | built by the client package's maker | request `GET /widgets` |
| `src/typed.ts:7` `server.get("/gadgets", ...)` | a parameter typed by the server package | route `GET /gadgets` |
| `src/typed.ts:11` `client.get("/gadgets", ...)` | a parameter typed by the client package | request `GET /gadgets` |

`expected.json` is the answer key. `node_modules/` holds only declaration
files, committed on purpose (the scan reads the types through them).
`tests/receiver_role_split_test.rs` reads it.
