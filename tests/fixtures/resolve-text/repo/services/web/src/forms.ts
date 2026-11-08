export {};

// Typed `string`, not as a literal: the text is read at the declaration.
const BASE: string = "/api";
let LATE: string = "/late";
enum Route {
  Items = "/items",
}
const enum Fixed {
  Health = "/health",
}
const ROUTES = { things: "/things", other: "/other" };
const KEY: string = "other";
const FROZEN = { a: "/frozen" } as const;

const p_const = BASE;
const p_let = LATE;
const p_enum = Route.Items;
const p_const_enum = Fixed.Health;
const p_entry = ROUTES.things;
const p_computed_key = ROUTES[KEY];
const p_as_const = FROZEN.a;
const p_join = [BASE, "items", "all"].join("/");
const p_join_default = ["a", "b"].join();
const p_concat = BASE.concat("/x", "/y");
const p_replace = BASE.replace("api", "v1");
const p_replace_all = "/a/a".replaceAll("a", "b");
const p_raw = String.raw`${BASE}/\d`;
const p_template = `${BASE}/${Route.Items}/tail`;
const p_plus = BASE + "/" + Fixed.Health;

export function inside(): string {
  const p_local_join = [BASE, "local"].join("/");
  return p_local_join;
}
