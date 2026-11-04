# helper-argument-not-body (carrick#3074)

Two services. `api` serves `POST /events` and reads the body as
`{ name: string; at: number }`. `web` sends that request from one place:

- `src/install.ts` hands each event to `postEvent` from `@fixture/client`,
  a package detection lists as a data fetcher. The package is not on this
  checkout, so discovery cannot follow the call.
- `src/track.ts` exports `trackClick(label: string, now = false): void`. It
  builds its own event and passes it to a sender `install.ts` configures at
  start. The label never reaches the request.
- `src/buttons.ts` calls `trackClick` twice with a string.

The cassette states the request at all three places, as the model does: the
call in `install.ts` under its candidate's id, the two helper calls with an
id no candidate carries (the file is read for its `fetch` call).

The pair must read compatible or unverifiable. A helper call's argument is
not the request body: reading it as one gives "Type 'string' is not
assignable" at both `buttons.ts` sites.
