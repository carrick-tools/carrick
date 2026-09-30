# payload-without-method

Screens that call a member of an imported helper object (carrick#1624). The
helper module decides whether the scanner may overrule the model's verb at the
call:

```
lib/orders.ts   decodeEnvelope({ data: output, format: "json" }, client)    no verb
lib/refunds.ts  logger.info({ body, event: "refund.issue" })                no verb
lib/profile.ts  useForm({ data: values, validateOn: "submit" })             no verb
lib/catalog.ts  fetch(`/v1/catalog?q=${query}`, { headers })                GET
lib/uploads.ts  client.request({ method: "POST", url: "/v1/uploads", data }) POST
```

A request-options bag with no `method` and no payload is a GET. One that
carries `body` or `data` and no `method` states no verb: a payload with no
method is a verb something else supplies, and the same keys go to calls that
are not requests at all. The first three modules' real requests go through
client members the scanner does not read, so those modules state no verb.

## The cassettes

Each screen's answer states a verb at its call: POST, POST, PUT, POST, GET.
The last two are wrong (the helpers send GET and POST); the helper modules'
answers state no rows.

## Answer key

| File | Line | Method | Why |
|---|---|---|---|
| src/checkout.ts | 4 | POST | the decode helper's `{ data }` states no verb |
| src/refund-screen.ts | 4 | POST | the logger's `{ body }` states no verb |
| src/profile-screen.ts | 4 | PUT | the form helper's `{ data }` states no verb |
| src/search-screen.ts | 4 | GET | the helper's only request names no method and no payload |
| src/upload-screen.ts | 4 | POST | the helper writes `method: "POST"` beside its payload |
