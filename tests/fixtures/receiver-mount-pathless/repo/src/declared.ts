import { makeRouter, Request, Response } from "fixture-hrouter";

function ok(_request: Request, response: Response): void {
  response.send("ok");
}

// Routers of an export no scope claim names: the `use` declaration the call
// resolves to says which claims apply, a path-first and a path-less one alike.
const app = makeRouter();
const first = makeRouter();
first.post("/one", ok);
const second = makeRouter();
second.post("/two", ok);
app.use("/y", first);
app.use(second);
