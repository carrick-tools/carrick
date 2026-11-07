import { get } from "@two/lib/api";
import { get as rel } from "./lib/api";
export function viaAliasTwo() { return get(); }
export function viaRelativeTwo() { return rel(); }
