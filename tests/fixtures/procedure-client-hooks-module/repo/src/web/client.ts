import { createExpandedHooks } from "fixture-procs";
import type { AppRouter } from "../router";

// The hooks proxy, made in its own module and imported where it is called.
export const trpc = createExpandedHooks<AppRouter>();
