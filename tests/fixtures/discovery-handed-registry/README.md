# Discovery: a call that hands its callee a function it keeps (carrick#3527)

`repo/services/relay` has one service. `library-claims.json` is the store's
answer: `fixture-wire`'s `listen` is a server framework, its `configure` is
labelled `none`.

`RelayServer` keeps a route table of its own. `add` and `addFn` only store
what they are handed; the `listen` callback reads the table later and runs
the stored handler. A walk into `add` or `addFn` finds no call, but it has
not read what the call does: the handed function runs later, from wherever
the callee put it.

| File | Role |
|---|---|
| `src/relay-server.ts` | `RelayServer`: its constructor and `wire()` register routes into its own table through `add` (an object argument holding `handler`) and `addFn` (a callback argument it never runs) |
| `src/reply.ts` | `Reply.text`: string work only, reaches no label |
| `src/configure.ts` | Control C2: a library call handed an object holding a function |

## Answer key

| File | Call | Record |
|---|---|---|
| `relay-server.ts` (constructor) | `this.add("/gauges", "GET", { handler: ... })` | `missed_inside`, not `finished` |
| `relay-server.ts` (`wire`) | `this.add("/pulse", "GET", { handler: ... })` | `missed_inside`, not `finished` |
| `relay-server.ts` (`wire`) | `this.addFn("/beat", async ({ reply }) => ...)` | `missed_inside`, not `finished` |
| `relay-server.ts` (`tally`) | Control C1: `this.runNow(() => { ... })`, whose callee runs the callback | A site stopped at `injected_callee` (the callee's own `fn()`), never `missed_inside` |
| `configure.ts` | Control C2: `configure({ onIdle: () => {} })`, a library callee | `finished` |
| `relay-server.ts` (`note`) | Control C3: `this.record("/note")`, beside a function written in the caller's body but not at the call | `finished` |
