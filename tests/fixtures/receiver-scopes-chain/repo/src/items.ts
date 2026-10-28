import app, { App, Reply, Request, Route } from "fixture-chain-router";

function list(_request: Request, reply: Reply): void {
  reply.send("list");
}

function create(_request: Request, reply: Reply): void {
  reply.send("create");
}

function show(_request: Request, reply: Reply): void {
  reply.send("show");
}

function drop(_request: Request, reply: Reply): void {
  reply.send("drop");
}

function pathOf(): string {
  return "/" + Date.now();
}

export const items = app();
// A verb on the object a scope member returns: the scope call's argument is the path.
items.at("/items").get(list);
// The same, across lines.
items
  .at("/multi")
  .post(create);
// A module-scope constant bound to the scope call.
const one = items.at("/one");
one.get(show);
// A helper that takes the path as a parameter: each caller states one.
function mount(r: App, p: string): void {
  r.at(p).get(show);
}
mount(items, "/p1");
mount(items, "/p2");
// A later verb on the same chain waits on the next slice.
items.at("/items/:id").get(show).delete(drop);
// A function-local constant holds the scope: no site.
function local(): void {
  const x = items.at("/local");
  x.get(show);
}
// A module `let` assigned twice: no site.
let late = items.at("/late1");
late = items.at("/late2");
late.get(show);
// The scope's result handed to a helper: no site.
function helper(route: Route): void {
  route.get(show);
}
helper(items.at("/handed"));
// A path the code cannot read.
items.at(pathOf()).get(show);
