import web from "fixture-web";
import { serveRpc } from "fixture-procs";
import { directRouter } from "./direct";
import { aliasedRouter } from "./aliased";
import { importedRouter } from "./aliased-import";
import { letRouter } from "./aliased-let";

const app = web();

app.all("/rpc-direct/*", (request, reply) => {
  void serveRpc({ router: directRouter, endpoint: "/rpc-direct", request }).then((result) =>
    reply.send(result),
  );
});

app.all("/rpc-alias/*", (request, reply) => {
  void serveRpc({ router: aliasedRouter, endpoint: "/rpc-alias", request }).then((result) =>
    reply.send(result),
  );
});

app.all("/rpc-alias-import/*", (request, reply) => {
  void serveRpc({ router: importedRouter, endpoint: "/rpc-alias-import", request }).then(
    (result) => reply.send(result),
  );
});

app.all("/rpc-alias-let/*", (request, reply) => {
  void serveRpc({ router: letRouter, endpoint: "/rpc-alias-let", request }).then((result) =>
    reply.send(result),
  );
});
