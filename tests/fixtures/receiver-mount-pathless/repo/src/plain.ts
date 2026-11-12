import router, { Request, Response } from "fixture-hrouter";

function ok(_request: Request, response: Response): void {
  response.send("ok");
}

const top = router();
const plain = router();
top.use("/plain", plain);

// A router handed to `use` with no path: served at its parent's address.
const plainRouter = router();
plainRouter.post("/a", ok);
plain.use(plainRouter);
