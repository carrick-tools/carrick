# method-per-site

A client whose calls go through an imported client instance, written in the
three line shapes from carrick-cloud#1365:

```
TeamPanel.tsx:11  await Promise.all(added.map((id) => api.post(url)))           POST
TeamPanel.tsx:15  try { await api.delete(url, { data }); await loadMembers(); }  DELETE
TeamPanel.tsx:19  try { await api.post(url, { notify: true }); ... }            POST
TeamPanel.tsx:23  if (assigned) await api.delete(url); else await api.post(url); DELETE, POST
```

`src/lib/api.ts` creates the client and issues one GET of its own, so the
file-level wrapper shape the screen imports is `GET`. `@example/http` is a
neutral stand-in for an HTTP client package.

## The cassette

`__llm__/analyze-file/TeamPanel.json` states every method correctly, but its
`candidate_id`s name no candidate the scanner raised, so none of the rows join
a span. Before the fix, every span-less row was read as a site delegating to
the imported wrapper and took its `GET`.

## Answer key

| Line | Method |
|---|---|
| 6 | GET |
| 11 | POST |
| 15 | DELETE |
| 19 | POST |
| 23 | DELETE |
| 23 | POST |
