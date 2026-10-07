# dispatch-file-route

One service with a file-based route whose handler dispatches on a body field,
and a client component that sends each case (carrick#2048). Every name in it
is invented.

`app/api/orders/[orderId]/route.ts` exports `GET` and `POST`. The `POST`
handler reads `op` off the JSON body and answers `confirm`, `cancel` and
`refund` from three branches. `components/OrderActions.tsx` sends each of the
three, with `op` written as a literal in the body, and loads the order with a
`GET`. A fourth `POST`, `replay`, sends a command it was handed, so the scan
cannot read which case it sends.

The model answers in `__llm__/` state one `POST` row per case, as a handler
that switches on a request field is read (carrick#831). Each case echoes a
different kind of id, so the three take the three paths the join has for a
row the file layout already states:

| Case | Id echoed | Path through the join |
|---|---|---|
| `confirm` | the body read on line 15 | names a call the handler makes, not a site the route is registered at |
| `cancel` | the send on line 20, at the line the model answered | a site inside the handler |
| `refund` | an id the prompt never offered | names nothing |

The scan must keep one `POST` row per case, each stated by the file layout,
and no `POST` row without a case beside them. Each call then links to the
case it sends. `replay` keeps one edge to the route with its case unknown: the
route exists, so it is not a missing endpoint. Before carrick#2048 the first
case to reach the join replaced the layout's row and the others were
discarded, so two of the three calls that state a case matched no route.
