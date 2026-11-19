import { supabase } from "./client";

const DIGEST_FN = "team-digest";

export async function sendDigest(teamId: string) {
  return supabase.functions.invoke(DIGEST_FN, { body: { teamId } });
}
