import { createClient } from "@supabase/supabase-js";

const SUPABASE_URL = import.meta.env.VITE_SUPABASE_URL;

export const supabase = createClient(SUPABASE_URL, "anon");
