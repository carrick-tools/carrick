import { keyedBoth, keyedOnly, FieldRelay, Port } from "@fixture/client";
import { oneOnly, twoOnly } from "./local";

export async function insideKeyed(name: string) { return keyedBoth(name); }
export async function insideKeyedOnly(name: string) { return keyedOnly(name); }
export async function insideField(r: FieldRelay) { return r.fieldBoth(); }
export async function insideFieldOnly(r: FieldRelay) { return r.fieldOnly(); }

const ports: Record<string, Port> = {};

export async function ownOne(name: string) { return oneOnly(ports[name]); }
export async function ownTwoA(name: string) { return twoOnly(ports[name]); }
export async function ownTwoB(name: string) { return twoOnly(ports[name]); }
