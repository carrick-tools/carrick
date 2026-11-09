import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
import { ROUTE } from "./s-routes";
createServer((req, res) => {
  if (req.method === "GET" && req.url === ROUTE) { res.end("a"); }
});
