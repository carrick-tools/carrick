import { createClient } from "@supabase/supabase-js";

// A make with no type argument states no row.
export const plain = createClient("http://localhost:54321", "anon");
