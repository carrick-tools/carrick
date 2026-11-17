import { Tap } from "fixture-tap";

// (u1) Held all the way; the op's maker does not verify.
export async function heldPipe() {
  const tap = new Tap();
  const pool = tap.pool("eu");
  const pipe = pool.pipe("orders");
  await pipe.write(new Uint8Array());
}

// (u2) A member call on a member call's value, on a held client.
export async function chainedPipe(tap: Tap) {
  await tap.pool("eu").pipe("audit").write(new Uint8Array());
}

// (u3) The whole chain in place.
export async function inPlacePipe() {
  await new Tap().pool("us").pipe("events").write(new Uint8Array());
}
