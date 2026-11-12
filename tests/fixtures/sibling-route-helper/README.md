# Sibling route helper

Invented two-service repo. `packages/kit` exports `addRoute(app, def)`, which
registers `def.path` as a route with `route()` from `fixture-route-server`. `services/api`
calls it at `src/server.ts:4` with a descriptor object from `src/routes/ping.ts`.

## Answer key

| File:line | Call | Unread site |
|---|---|---|
| `services/api/src/server.ts:4` | `addRoute(pingRoute)` | `missed_inside`, `kind` http, `role` producer, labelled end `fixture-route-server` `route` (the route is served by `api`, its path is read from the descriptor in a later change) |
