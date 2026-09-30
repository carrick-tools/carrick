# spread-options-verb

Clients that call an imported fetch helper and pass their request options one
property deep (carrick#1603). What the helper does with those options decides
the verb:

```
sse.ts     fetch(input, { ...init, ...options.request, headers })   caller's verb
upload.ts  fetch(options.url, { method: "POST", ...options.request }) caller's verb
poll.ts    fetch(options.url, { ...options.request, method: "GET" })  GET
status.ts  fetch(url, { headers })                                    GET
```

A spread can carry `method`, and an entry overwrites the ones before it. So a
spread after every `method` key leaves the verb to the caller, and a `method`
written after every spread is the verb sent.

## The cassettes

Each client's answer states the verb the client passes: PATCH, PUT, DELETE.
`status-client.json` states POST, which is wrong: `status.ts` only ever sends a
GET. The helpers' answers state no rows.

## Answer key

| File | Line | Method | Why |
|---|---|---|---|
| src/asset-client.ts | 4 | PUT | the helper's spread overwrites its POST |
| src/build-client.ts | 7 | PATCH | the helper spreads the caller's options |
| src/job-client.ts | 4 | GET | the helper writes GET after the spread |
| src/status-client.ts | 4 | GET | the helper forwards nothing and names no method |
