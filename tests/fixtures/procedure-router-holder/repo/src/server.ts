import web from "fixture-web";
import { serveRpc } from "fixture-procs";
import { directRouter } from "./direct";
import { tableRouter } from "./table";
import { firstRouter, secondRouter } from "./pair";

const app = web();

app.all("/direct/*", (request, reply) => {
  void serveRpc({ router: directRouter, endpoint: "/direct", request }).then((r) => reply.send(r));
});
app.all("/table/*", (request, reply) => {
  void serveRpc({ router: tableRouter, endpoint: "/table", request }).then((r) => reply.send(r));
});
app.all("/first/*", (request, reply) => {
  void serveRpc({ router: firstRouter, endpoint: "/first", request }).then((r) => reply.send(r));
});
app.all("/second/*", (request, reply) => {
  void serveRpc({ router: secondRouter, endpoint: "/second", request }).then((r) => reply.send(r));
});
