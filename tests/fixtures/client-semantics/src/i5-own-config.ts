import { configured } from "./shared/configured";
import { config } from "./i5-config";

export const c = config;
export function i5(): unknown { return configured.get("/read"); }
