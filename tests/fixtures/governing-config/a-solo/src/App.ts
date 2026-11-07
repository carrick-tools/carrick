import { get } from "@/lib/api";
import { get as rel } from "./lib/api";
export function viaAlias() { return get(); }
export function viaRelative() { return rel(); }
