import router, { Reply, Request } from "fixture-router";

function ok(_request: Request, reply: Reply): void {
  reply.send("ok");
}

const app = router();
app.get("/health", ok);

// A function that calls itself with a longer path each time: the path it
// hands the library grows on every call, so no two walks of it read the
// same value.
function nest(path: string, depth: number): void {
  app.get(path, ok);
  if (depth > 0) {
    nest(`${path}/child`, depth - 1);
  }
}

nest("/tree", 3);
