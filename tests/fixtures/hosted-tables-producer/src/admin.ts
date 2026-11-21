import { createClient } from "@supabase/supabase-js";
import type { Database } from "./types";

// A second make of the same type: each key is still one row.
export const admin = createClient<Database>("http://localhost:54321", "service");
