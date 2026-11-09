# discovery-factory-objects

A key of an object a factory returns, or a module binding holds, read where a
caller hands the object to a function that calls `deps.get` (carrick#3670).
Read by the resolver's tests in `src/request_summary/resolve.rs`
(`factory_objects`). One shape per file under `repo/services/web/src/`; the
answer is what the callee `deps.get` (or `get`) of each `run*With` holds.

| File | Shape | Holds |
|---|---|---|
| `a-cache.ts` | caching factory: `if (c) return c; c = { get }; return c` | `loadA` |
| `b-literal.ts` | factory returns `{ get: loadB }`, passed as `runWith(makeB())` | `loadB` |
| `c-const.ts` | factory returns a module `const` object | `loadC` |
| `d-default.ts` | caching factory as a parameter's default `deps = makeD()` | `loadD` |
| `e-arrow.ts` | caching factory whose object holds an arrow | the arrow (line 6) |
| `f-direct.ts` | `const deps = makeF(); deps.get(id)` in one body | `loadF` |
| `g-passed.ts` | caller passes `{ get: loadG }` | `loadG` |
| `h-passed-const.ts` | a module `const` object handed to the call | nothing: a callee may change it |
| `i-passed-arrow.ts` | caller passes `{ get: (x) => loadI(x) }` | a callback |
| `j-fn.ts` | factory returns a function, passed as a parameter | `loadJ` |
| `k-closed.ts` | factory returns a closed object without `get` | nothing |

A module binding is read as the object it holds when its only uses are its
declaration, plain writes placed in a function of the module, truthiness tests
(`if (c)`, `!c`, `c && …`) and `return c`, besides member reads, and nothing is
written through it. A binding handed to a call stays unread.
