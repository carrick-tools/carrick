import { serve } from "https://deno.land/std@0.168.0/http/server.ts";

async function purgeSessions(_req: Request): Promise<Response> {
  return new Response(JSON.stringify({ purged: 0 }));
}

serve(purgeSessions);
