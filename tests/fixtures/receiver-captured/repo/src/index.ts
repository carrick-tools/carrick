import router, { Reply, Request } from "fixture-router";

function ok(_request: Request, reply: Reply): void {
  reply.send("ok");
}

const app = router();
// A route written in a callback nested inside the plugin that owns `scoped`.
app.register(
  (scoped) => {
    ["a"].forEach(() => {
      scoped.get("/each", ok);
    });
    setTimeout(() => scoped.get("/later", ok), 0);
  },
  { prefix: "/cap" },
);
// Control: the route is written in the plugin itself.
app.register(
  (scoped) => {
    scoped.get("/direct", ok);
  },
  { prefix: "/cap" },
);
