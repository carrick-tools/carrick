import { Port } from "@fixture/client";

export async function oneOnly(port: Port): Promise<void> {
  await port.send("one", {});
}

export async function twoOnly(port: Port): Promise<void> {
  await port.send("two", {});
}
