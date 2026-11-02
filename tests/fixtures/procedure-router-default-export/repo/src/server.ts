import web from "fixture-web";
import { rpcMiddleware } from "fixture-procs-mw";
import { appRouter } from "./router";

// The app is made through the package's `default` export; the store labels
// `use` (the mount) on the named export alone.
const app = web();

app.use("/rpc", rpcMiddleware({ router: appRouter }));
