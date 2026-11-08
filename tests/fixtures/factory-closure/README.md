# A function a factory returns reads the factory's parameters from the call that made it (carrick#3504 S0b)

`repo/services/web` is one service. `@fixture/send` (`send`, an HTTP client
function, method `POST`) and `@fixture/pub` (`publish`, a broker send) are
labelled function exports: `library-claims.json` is the store's answer, and
`__llm__/library-claims/default.json` is the same answer as the scan's
cassette. The lockfile is there because the offline scan applies no claim
without one.

Each file has a factory that returns a function it writes, and a caller that
holds what the factory returns and calls it. The factory call is the maker
call: it fills the factory's parameters, which the returned function reads
one function out.

## Answer key

| File:line | Shape | Record |
|---|---|---|
| `s-closure-coalesce.ts:8` | factory local `o.send ?? send`; maker call `make({ token })` | A site, end `send`, no stop: the maker call writes no `send` key, so the other value decides |
| `s-closure-coalesce-arrow.ts:7` | same, the factory and the returned function are arrows | Same |
| `s-closure-param.ts:7` | the factory's parameter itself, `make(send)` | A site, end `send`, no stop |
| `s-closure-param-key.ts:7` | a key of the parameter, `make({ post: send })`, `o.post(…)` | Same |
| `s-closure-param-local.ts:8` | a factory local holding that key | Same |
| `s-two-level.ts:17` | the maker call `makeHttp(opts)` is in `makeClient`, passing its own parameter; the call is in `search`, one function further in | A site, end `send`, no stop: `opts` is read at `makeClient`'s callers |
| `t-real.ts:26` | the same two-level shape, with a module-constant host and the method in the send's options | Same |
| `s-closure-local.ts:7` | a factory local holding `send` directly | A site, end `send` (already before the step) |
| `s-two-level-direct.ts:11` | the returned function calls `send` directly | Same |
| `z-literal-fn.ts:5` | a closed URL inside the returned function | A site at the inner call, end `send` (already before the step) |
| `b-closure-coalesce.ts:5` | broker: factory local `o.pub ?? publish` | A site, end `publish` |
| `b-closure-coalesce.ts:8` | the call of what the factory returned | Not `injected_callee` |
| `s-closure-coalesce-filled.ts:9` | the maker call fills `send` with `quiet`, which sends nothing | A site with stop `several_values`: the values disagree |
| `c-binding-maker.ts:11` | the maker call `make(o)` writes a module `let` in `init`; `use` calls the binding | A site with stop `several_values`: `init`'s parameter is no parameter of `use`, so `use`'s caller passing `send` does not decide it |
