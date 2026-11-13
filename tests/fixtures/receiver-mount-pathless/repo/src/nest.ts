import router, { Request, Response } from "fixture-hrouter";

function ok(_request: Request, response: Response): void {
  response.send("ok");
}

// Seven mounts deep, each `use` verified by a claim and its handle twin: one
// edge per mount, so the route composes once, within the chain budget.
const r0 = router();
const r1 = router();
const r2 = router();
const r3 = router();
const r4 = router();
const r5 = router();
const r6 = router();
const r7 = router();
r0.use("/a", r1);
r1.use("/b", r2);
r2.use("/c", r3);
r3.use("/d", r4);
r4.use("/e", r5);
r5.use("/f", r6);
r6.use("/g", r7);
r7.post("/z", ok);
