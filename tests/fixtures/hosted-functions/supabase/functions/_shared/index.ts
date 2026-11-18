import { serve } from "https://deno.land/std@0.168.0/http/server.ts";

// A shared folder is never a function, even with an index module that serves.
serve(async (_req: Request) => new Response("shared"));
