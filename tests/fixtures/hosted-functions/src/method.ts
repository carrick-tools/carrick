import { supabase } from "./client";

export async function quoteStatus() {
  return supabase.functions.invoke("price-quote", { method: "GET" });
}
