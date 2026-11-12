import router, { Handler, Request, Response } from "fixture-hrouter";

function ok(_request: Request, response: Response): void {
  response.send("ok");
}

function logged(): Handler {
  return (_request, _response, next) => next();
}

const app = router();
// Plain middleware under a handle mount: no instance, so no edge and no site.
app.use(logged());
app.use("/m", logged());
app.post("/m/ok", ok);
