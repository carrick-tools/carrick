import { get } from "@one/lib/api";
import { get as rel } from "./lib/api";
export function viaAliasOne() { return get(); }
export function viaRelativeOne() { return rel(); }
