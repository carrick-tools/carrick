import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
createServer((req, res) => {
  const m = req.method;
  if (m === "GET" && req.url === "/s-method-alias") { res.end("a"); }
});
