import { api } from "./shared/api";

export function i1(): unknown { return api.get("/users"); }
