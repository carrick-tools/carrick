# `stated-generic-body`

Fixture for carrick#1817: a client that states the body each `fetch` reads,
where the statement is an instantiation of a generic.

## The shape

`src/shapes.ts` declares two bodies (`Order`, `Member`), a generic wrapper
(`Envelope<T>`), an alias of one instantiation (`OrderEnvelope`), and a
generic whose argument has a default (`Listing<T = Order>`).

`src/client.ts` makes nine calls. Each reads its body with `res.json()` and
states what it is at the read: a cast behind `Promise`, a cast after the
await, or the annotation of the declaration the read initializes.

`__llm__/analyze-file/client.json` answers one data call per `fetch`. Its
`primary_type_symbol` varies on purpose: the element inside the statement, the
generic itself, or nothing. The anchor must not depend on it.

## The answer key

The anchor of each call's response, and where that type is declared in
`src/shapes.ts`.

| call | the read is stated as | the model named | anchor | home |
|---|---|---|---|---|
| `client.ts:5` | `Promise<Envelope<Order[]>>` | `Order` | `Envelope` | line 13 |
| `client.ts:11` | `Envelope<Member>` | nothing | `Envelope` | line 13 |
| `client.ts:17` | `Envelope<Member[]>` (annotation) | `Envelope` | `Envelope` | line 13 |
| `client.ts:24` | `Promise<OrderEnvelope>` | `Order` | `OrderEnvelope` | line 19 |
| `client.ts:30` | `Listing` | nothing | `Listing` | line 22 |
| `client.ts:36` | `Listing<Member>` | `Member` | `Listing` | line 22 |
| `client.ts:42` | `Envelope<Order[]> \| Envelope<Member[]>` | `Order` | none | none |
| `client.ts:48` | `Envelope<Order> \| null` | nothing | `Envelope` | line 13 |
| `client.ts:54` | `Record<string, Order>` | `Order` | none | none |

Why:

- An instantiation is anchored at the generic the source writes outermost,
  once `Promise` and `| null` are seen through. Its home is the generic's own
  declaration. A type argument is never the anchor.
- An alias of an instantiation is the name the source states, so it is the
  anchor, not the generic behind it.
- A generic with a defaulted argument is one anchor, written bare or with the
  argument.
- A union of two instantiations states no single name.
- `Record` is the compiler's: the repo has no declaration to state as its
  home, and its argument is not the body.

Used by `tests/stated_generic_body_anchor_test.rs`.
