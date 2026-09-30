import { api } from "./shared/api";

export function i2(): unknown { return api.post("/orders", { action: "create" }); }
