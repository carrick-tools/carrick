import { api as shared, svc } from "./shared";
import direct from "./shared/svc";

export function i4a(): unknown { return shared.get("/renamed"); }
export function i4b(): unknown { return svc.get("/barrel-default"); }
export function i4c(): unknown { return direct.get("/default"); }
