import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
import { serve } from "./s-net";
serve((req, res) => {
  if (req.method === "GET" && req.url === "/s-reexport") { res.end("a"); }
});
