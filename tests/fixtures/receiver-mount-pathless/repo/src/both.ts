import router, { Request, Response } from "fixture-hrouter";

function ok(_request: Request, response: Response): void {
  response.send("ok");
}

const app = router();
const first = router();
first.post("/one", ok);
const second = router();
second.post("/two", ok);
// A path-first and a path-less `use` on one member: each reads its own call.
app.use("/x", first);
app.use(second);
