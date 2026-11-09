import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
let prefix = "";
createServer((req, res) => {
  const path = req.url;
  if (req.method === "GET" && path === prefix + "/s-module-let") { res.end("a"); }
});
