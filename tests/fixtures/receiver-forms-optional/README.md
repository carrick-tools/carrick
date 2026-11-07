# receiver-forms-optional

carrick#3044 S3. Each file in `repo/src` is one optional call (`x?.m(...)`);
`expected.json` is the answer key, the rows its plain twin states.

- `o-import.ts`: a request through an imported client.
- `o-field.ts`: a request through a typed class field.
- `t-optional-chain.ts`: a request through a union-typed constant.
- `m-optional.ts`: a request through a maker's instance.
- `o-app.ts`: a route on an app.
- `o-bus.ts`: a send on a broker (control: already states a row).
- `e-optional.ts`: sends and receives on an emitter field.
