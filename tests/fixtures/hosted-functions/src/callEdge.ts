import { supabase } from "./client";

export async function callEdge(name: string, body: unknown) {
  return supabase.functions.invoke(name, { body });
}
