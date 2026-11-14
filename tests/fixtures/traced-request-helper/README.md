# traced-request-helper

Request helpers that make their request inside a function handed to a
package's export (a timing span), with the request's options coming from the
helper's own parameters. Read by `tests/traced_request_helper_test.rs`.

```
traced.ts  send(path, init)      span(…, () => fetch(`${env.API_URL}${path}`, init))
plain.ts   sendPlain(path, init) fetch(`${env.API_URL}${path}`, init)
rest.ts    restRequest(config)   const { path, headers, ...rest } = config;
                                 span(…, () => Request.make({ ...rest, url: `${env.API_URL}${path}`, headers }))
rest.ts    sendConfig(config)    span(…, () => Request.make(config))
wrapped.ts callApi(method, endpoint, data)  send(endpoint, { method, body })
```

`use-wrapped.ts` awaits `callApi` with string arguments and makes no other
call. No call-site signal raises it: the fact layer reads the file because
discovery reaches `fetch` through `callApi` and `send` (the awaited-call
signal that once raised it is deleted, V1 rule C2).

`env.API_URL` is read at run time, so the path each site states is the part
the helper's caller writes.

## The cassettes

- The store labels the default library's `fetch` (name argument 0, method
  key `method` of argument 1, default `GET`), `@fixture/request`'s
  `Request.make` (name key `url`, method key `method` of argument 0, no
  default) and `@fixture/trace`'s `span` with no network role.
- The reader states a row at each helper call in `tasks-api.ts`,
  `workspace-api.ts` and `use-wrapped.ts`; at lines 6, 14, 12 and 15 its verb is wrong. The helpers'
  answers state no rows.
- `task-panel.ts` calls the `tasks-api.ts` members; its reader rows are a
  wrong verb (line 4), a wrong path (line 8) and a wrong verb (line 12).
  They are not asserted: a row at a caller above a decided site that
  disagrees with it is carrick#2704's.

## Answer key (call rows, with the switches on)

| File | Line | Method | Path | Source | Why |
|---|---|---|---|---|---|
| src/tasks-api.ts | 6 | PUT | /v1/tasks/:id/status | fact | the caller's `init` writes `method: "PUT"` |
| src/tasks-api.ts | 10 | GET | /v1/tasks/:id | model (fact row owed by #3372) | the caller passes no `init`: the label's default; a URL ending in a parameter no caller fills states no fact row yet |
| src/tasks-api.ts | 14 | PUT | /v1/tasks/:id/archive | fact | no span; control |
| src/workspace-api.ts | 9 | GET | /v1/workspaces/:id | fact | the bag's rest carries `method` |
| src/workspace-api.ts | 12 | PATCH | /v1/workspaces/:id | fact | the same |
| src/use-wrapped.ts | 4 | POST | /v1/items | any | `callApi`'s first argument is the method |
| src/use-wrapped.ts | 8 | GET | /v1/items | any | the same |
| src/workspace-api.ts | 15 | DELETE | /v1/workspaces/:id | fact | `sendConfig` hands the caller's whole bag on: `url` and `method` are its keys |
