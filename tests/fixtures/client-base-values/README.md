# client-base-values

Answer key for `a_client_base_reads_through_the_resolver`
(`tests/receiver_trace_test.rs`, carrick#3504 S5a). Each `repo/src` file makes
a client with `mk.create({ baseURL: ... })` in one form and calls
`api.get('/p/<file>')`. `b-*` files hold the instance in a module binding;
`w-*` files hand it to a function through a parameter (instance-walk root).

| Files | Row |
|---|---|
| literal, `const`, imported `const`, template, shorthand, `const` object entry, key after a spread, factory parameter and options, walk root over a literal or `const` | `https://m.test/p/<file>` |
| enum member, `let` never written again, `join`, `?:` with equal arms, walk root over an imported `const` | `https://m.test/p/<file>` (`b-join`: `https://m.test/v1/p/b-join`) |
| options object held in a `const` (`b-opts-const`, `w-opts-const`) | `https://m.test/p/<file>` (was host-free: a correction) |
| `{ baseURL: x, ...defaults }` (`b-opts-spread-after`) | no row (was host-free: a correction) |
| env read, `??` over env, `?:` with two texts, `let` written again, instance written through | no row |
| options `const` with no `baseURL` key (`b-opts-const-nobase`) | `/p/<file>` (no base) |

`b-contested`, `w-contested` and `w-root-unread` also serve the follow-up that
reads walk roots as the reader's makers are read; this test does not assert
the walk-root ones.
