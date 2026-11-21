import { createClient } from "@supabase/supabase-js";
import type { Database } from "./types";

const HOSTED_URL = import.meta.env.VITE_HOSTED_URL;

export const db = createClient<Database>(HOSTED_URL, "anon");
