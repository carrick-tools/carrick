import { sendBoth, sendStopOnly, Port } from "@fixture/client";

export async function passTwo(p: Port) { return sendBoth(p); }
export async function passTwoOnly(p: Port) { return sendStopOnly(p); }
export async function passOne(p: Port) { return sendBoth(p); }
export async function passOneOnly(p: Port) { return sendStopOnly(p); }

const ports: Record<string, Port> = {};

export async function twoA(n: string) { return passTwo(ports[n]); }
export async function twoB(n: string) { return passTwo(ports[n]); }
export async function twoOnlyA(n: string) { return passTwoOnly(ports[n]); }
export async function twoOnlyB(n: string) { return passTwoOnly(ports[n]); }
export async function one(n: string) { return passOne(ports[n]); }
export async function oneOnly(n: string) { return passOneOnly(ports[n]); }
