# call-states-its-verb

A client whose calls go through an imported client instance, each spelled with
its verb (`client.post(...)`) and a template-literal URL, in the three line
shapes from carrick-cloud#1365:

```
LabelPicker.tsx:4   await Promise.all(labelIds.map((labelId) => client.post(url)));        POST
LabelPicker.tsx:8   await Promise.all(labelIds.map((labelId) => client.put(url, { name })));  PUT
InviteCard.tsx:6    try { setBusy(true); const res = await client.post(url, {...}); ... }     POST
InviteCard.tsx:10   try { setBusy(true); await client.patch(url, { name }); } ...             PATCH
MemberToggle.tsx:4  if (member) await client.delete(url); else await client.post(url);       DELETE, POST
```

`src/lib/client.ts` creates the client and issues no request of its own, so no
wrapper module stands behind the screens. `@example/http` is a neutral
stand-in for an HTTP client package, and it is not installed, so no
deterministic source states a row at any of these sites: every row is the
model's.

`src/lib/items.ts` is the negative side: requests whose method the model
states correctly, next to or around verb-named calls that are not the request.
A chain whose `finally` deletes from a `Map` (line 6), a chain whose `then`
reads a header (line 12), a form field read inside a wrapper call's arguments
(line 18), a query parameter read inside the request's URL argument (line 24),
a `Map` read beside a request (line 28), and a `fetch` chain whose `then`
issues a DELETE to the same URL (line 32).

## The cassettes

Every `candidate_id` is a real candidate id (`span:<start>-<end>`, read off
the fixture's own prompt), so each row joins the site it names.

- The screens' rows state `"method": null`. `LabelPicker.tsx:4` names the
  outer `Promise.all` call, which is not a request; line 8 names the request
  inside. Before the fix, every screen row was indexed as a GET, the default
  for a missing method.
- The rows in `items.ts` state the method the code sends, except the DELETE on
  line 32, which states none.

The ids are byte offsets into the source files. Editing a source file moves
them, and the cassette has to be re-read from a `CARRICK_EVAL_DUMP_DIR` run.

## Answer key

| File | Line | Method |
|---|---|---|
| `src/screens/LabelPicker.tsx` | 4 | POST |
| `src/screens/LabelPicker.tsx` | 8 | PUT |
| `src/screens/InviteCard.tsx` | 6 | POST |
| `src/screens/InviteCard.tsx` | 10 | PATCH |
| `src/screens/MemberToggle.tsx` | 4 | DELETE |
| `src/screens/MemberToggle.tsx` | 4 | POST |
| `src/lib/items.ts` | 6 | GET |
| `src/lib/items.ts` | 12 | PUT |
| `src/lib/items.ts` | 18 | PATCH |
| `src/lib/items.ts` | 24 | DELETE |
| `src/lib/items.ts` | 28 | POST |
| `src/lib/items.ts` | 32 | GET |
| `src/lib/items.ts` | 32 | DELETE |
