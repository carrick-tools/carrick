import { supabase } from "./client";

export function useEdge(name: string) {
  return (body: unknown) => supabase.functions.invoke(name, { body });
}
