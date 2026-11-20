import { createClient } from "@supabase/supabase-js";
import type { Database } from "./types";

const URL_ = import.meta.env.VITE_HOSTED_URL;

export const db = createClient<Database>(URL_, "anon");
