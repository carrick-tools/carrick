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

## The cassettes

Every row states `"method": null`, and every `candidate_id` is a real candidate
id (`span:<start>-<end>`, read off the fixture's own prompt), so each row joins
the site it names. `LabelPicker.tsx:4` names the outer `Promise.all` candidate,
which is not a request; line 8 names the request inside. Before the fix, every
row was indexed as a GET, the default for a missing method.

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
