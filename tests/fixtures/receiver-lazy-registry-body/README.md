# receiver-lazy-registry-body

Answer key for a callee destructured from a lazy module registry inside a
function or loop body (K3 body keys). One registry per form in
`repo/src/registries.ts`, one module per form in `repo/src/svc/*.ts`; each
module's default export is `init(app)`, with one route at line 8.

| Form | Shape | Where | Answer |
|---|---|---|---|
| A | destructured key at the top level of a function body | `src/start.ts` `startA` | `/a/ping` at the top |
| B | in a for-of body inside a function, key cast with `as keyof typeof`, `await Promise.resolve(init(app))` | `startB` | `/b/ping` |
| C | in a `forEach(async (name) => ...)` callback, `app` captured from the enclosing body | `startC` | `/c/ping` |
| D | in a for-of body at module scope | `src/top.ts` | `/d/ping` |
| E | default-exported registry, `Object.keys(registry).includes(name)` guard with a `throw`, for-of over a list, `init(app, ...)` in `Promise.resolve`, three inits | `src/real/main.ts` | `/e/ping` |
| F | `const { default: init } = await import("./svc/f")` in a body | `src/direct.ts` | `entry_point` (stays unread) |
| G | the same at module scope | `src/top-direct.ts` | `/g/ping` |

`src/partial.ts`: a block key read in part (one branch a definition, the
other a call nothing answers) keeps its answer, a `UnreadExpression` stop.
