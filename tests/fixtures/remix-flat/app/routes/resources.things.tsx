// A `.tsx` module with a read handler and NO default export: a resource route,
// written in the page plane's extension because that is where its siblings
// live. Route-ness is the exported handler, so it derives
// `GET /resources/things` and is not a view module.

type Thing = { id: string };

export async function loader(): Promise<Thing[]> {
  return [{ id: "thing-1" }];
}
