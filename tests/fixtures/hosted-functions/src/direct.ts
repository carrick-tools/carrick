import { supabase } from "./client";

export async function quoteDirect() {
  return supabase.functions.invoke("price-quote", { body: { items: [] } });
}
