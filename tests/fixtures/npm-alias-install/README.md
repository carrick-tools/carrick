# `npm-alias-install`

Fixture for carrick#3571 (G3): a dependency installed as an npm alias.
`tests/npm_alias_install_test.rs` runs it. Everything in it is invented.

`package.json` declares `"wirelink-v4": "npm:wirelink@4.x"`. The installed
copy sits at `node_modules/wirelink-v4` and its manifest says it is
`wirelink` 4.2.0 (public registry, `package-lock.json`). The store knows only
`wirelink`, so the scanner asks under that name; the cassette
`__llm__/library-claims/default.json` is the store's answer for it, and the
rows must land on the code that imports `wirelink-v4`.

| Site | Row |
|---|---|
| `src/client.ts:6` | call `CLIENT->SERVER` `hub.join` |
| `src/client.ts:10` | call `CLIENT->SERVER` `hub.leave` |
