import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
createServer((req, res) => {
  if (req.method === "GET" && req.url === "/s-direct") { res.end("a"); }
});
