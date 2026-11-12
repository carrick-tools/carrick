# Sibling route helper

Invented two-service repo. `packages/kit` exports `addRoute(app, def)`, which
registers `def.path` as a POST or GET route by `def.method`. `services/api`
calls it at `src/server.ts:7` with a descriptor object from `src/routes/ping.ts`.

## Answer key

| File:line | Call | Unread site |
|---|---|---|
| `services/api/src/server.ts:7` | `addRoute(app, pingRoute)` | `missed_inside`, `kind` http, `role` producer, labelled end `fixture-route-server` `server` `post` (the route is served by `api`, its path is read from the descriptor in a later change) |
