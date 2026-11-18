import { serve } from "https://deno.land/std@0.168.0/http/server.ts";
import { handleInvite } from "./handler.ts";

serve(handleInvite);
