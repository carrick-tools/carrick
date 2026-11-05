# wrapper-call-path-argument

A helper whose request path ends in a template hole, and a hook that fills
the hole with an imported constant (carrick#1794):

```
files.ts:4          fetchShelfFile(name)    request({ path: `/v1/shelves/files/${name}` })
names.ts:1          COVER_FILE = "cover.png"
useShelfCover.ts:5  fetchShelfFile(COVER_FILE)
```

`transport.ts` hands the options to a client package with a spread, so no
deterministic pass reads the request: every row is the model's.

## The cassette

- `files.json` answers line 4 with the template, `/v1/shelves/files/${name}`.
- `useShelfCover.json` answers the hook's call with the filled path,
  `/v1/shelves/files/cover.png`. That row is true: the constant fills the hole.

## The answer key

| Site | Route |
|---|---|
| `useShelfCover.ts:5` | GET /v1/shelves/files/cover.png |
| `files.ts:4` | GET /v1/shelves/files/:name |

The hook's last segment, `cover.png`, is written nowhere in the helper, as in
`wrapper-call-route`. Here that is because the helper's request ends in a hole
the caller's argument fills, not because the model named a sibling route, so
the hook's row stays.
