import web from "fixture-web";
import { serveRpc } from "fixture-procs";
import { mergedRouter } from "./merged";
import { aliasedMerge } from "./merged-alias-use";
import { nestedMerge } from "./merged-nested";
import { spreadMerge } from "./merged-spread";

const app = web();

app.all("/rpc-merged/*", (request, reply) => {
  void serveRpc({ router: mergedRouter, endpoint: "/rpc-merged", request }).then((result) =>
    reply.send(result),
  );
});

app.all("/rpc-merged-alias/*", (request, reply) => {
  void serveRpc({ router: aliasedMerge, endpoint: "/rpc-merged-alias", request }).then((result) =>
    reply.send(result),
  );
});

app.all("/rpc-merged-nested/*", (request, reply) => {
  void serveRpc({ router: nestedMerge, endpoint: "/rpc-merged-nested", request }).then((result) =>
    reply.send(result),
  );
});

app.all("/rpc-merged-spread/*", (request, reply) => {
  void serveRpc({ router: spreadMerge, endpoint: "/rpc-merged-spread", request }).then((result) =>
    reply.send(result),
  );
});
