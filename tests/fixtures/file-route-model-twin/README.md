# file-route-model-twin

A route declared by file location (`app/orders/route.ts` serves `GET /orders`)
in a module that ALSO makes an outbound call, so the file reaches the model
with a candidate to anchor an answer on. That is the one arrangement in which
the model's endpoint row and the deterministic route row are both present for
the same route, which is what the join order is pinned against
(`deterministic_emission_test`, carrick#660): the model contributes the owner,
the handler and the type anchors, and the row stays the convention's.

The manifest declares `next`, which is what bootstraps the routing convention
without an LLM call.
