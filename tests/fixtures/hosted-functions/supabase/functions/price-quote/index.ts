import { serve } from "https://deno.land/std@0.168.0/http/server.ts";
import { corsHeaders } from "../_shared/cors.ts";

interface QuoteRequest {
  items: string[];
}

serve(async (req: Request) => {
  const body = (await req.json()) as QuoteRequest;
  if (!Array.isArray(body.items)) {
    return new Response(JSON.stringify({ error: "items is required" }), {
      status: 400,
      headers: corsHeaders,
    });
  }
  return new Response(JSON.stringify({ total: body.items.length }), {
    headers: corsHeaders,
  });
});
