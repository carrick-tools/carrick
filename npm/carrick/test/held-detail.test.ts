// The early check's detail (carrick#4688): keeps what the compiler said differs,
// names each side on its own, and never gives both sides the same words.

import test from "node:test";
import assert from "node:assert/strict";
import { heldDetail } from "../src/held/early.ts";

const LONG_SENT = `'{ ${"alpha: string; ".repeat(8)}}'`;
const LONG_EXPECTED = `'{ ${"beta: number; ".repeat(8)}}'`;
const say = (message: string, symbol?: string) => heldDetail({ message, symbol: symbol ?? null } as never);

test("a missing property on long object types names the property and cuts each side on its own", () => {
  const text = say(
    `Type ${LONG_SENT} is not assignable to type 'O1_0'.\n  Property 'total' is missing in type ${LONG_SENT} but required in type ${LONG_EXPECTED}.`,
  )!;
  assert.match(text, /^'total' is missing\. This code sends '\{ alpha: string;/);
  assert.match(text, /the caller expects '\{ beta: number;/);
  assert.ok(!text.includes("the reply"));
  assert.ok(text.includes("…"));
});

test("a missing property against a named type keeps the name", () => {
  const text = say(`Property 'total' is missing in type ${LONG_SENT} but required in type 'O2_1'.`, "OrderView")!;
  assert.match(text, /the caller expects 'OrderView'\.$/);
});

test("a property with two different primitive types gives both types", () => {
  const text = say(
    "Type '{ id: string; }' is not assignable to type 'O1_0'.\n  Types of property 'id' are incompatible.\n    Type 'string' is not assignable to type 'number'.",
  )!;
  assert.equal(text, "'id' has a different type. This code sends 'string'; the caller expects 'number'.");
});

test("a nested property path is kept whole", () => {
  const text = say(
    "Type 'X' is not assignable to type 'O1_0'.\n  Types of property 'owner' are incompatible.\n    Types of property 'age' are incompatible.\n      Type 'string' is not assignable to type 'number'.",
  )!;
  assert.equal(text, "'owner.age' has a different type. This code sends 'string'; the caller expects 'number'.");
});

test("a whole-type mismatch has no property and names each side", () => {
  assert.equal(say("Type 'string' is not assignable to type 'O1_0'.", "OrderView"), "This code sends 'string'; the caller expects 'OrderView'.");
  assert.equal(say("Type 'string' is not assignable to type 'O1_0'."), "This code sends 'string'; the caller expects a different type.");
  assert.equal(
    heldDetail({ message: "Type 'string' is not assignable to type 'O1_0'.", symbol: null, expected: "{ id: string }" } as never),
    "This code sends 'string'; the caller expects '{ id: string }'.",
  );
});

test("a short type needs no shortening", () => {
  const text = say("Property 'id' is missing in type '{ name: string; }' but required in type '{ id: number; name: string; }'.")!;
  assert.equal(text, "'id' is missing. This code sends '{ name: string; }'; the caller expects '{ id: number; name: string; }'.");
});

test("the missing property survives a long chain (the old cut dropped it)", () => {
  const chain = `Type ${LONG_SENT} is not assignable to type 'O1_0'. `.repeat(3);
  const text = say(`${chain}\nProperty 'tail' is missing in type ${LONG_SENT} but required in type 'O1_0'.`)!;
  assert.ok(text.startsWith("'tail' is missing."), text);
  assert.ok(text.length <= 240);
});

test("a message of another shape still reads without the placeholder", () => {
  const text = say(`Object literal may only specify known properties, and 'x' does not exist in type ${LONG_EXPECTED}.`)!;
  assert.ok(!text.includes("the reply"));
  assert.ok(text.includes("'x' does not exist"));
});

// HM4 T3: the sentence is chosen by which end of the check this code is. Every
// name is invented. The producer's sentence stays byte for byte.
const SEND_MISSING = "Property 'status' is missing in type '{ id: string; }' but required in type 'O1_0'.";
const SEND_DIFFERS =
  "Type '{ id: string; status: number; }' is not assignable to type 'O1_0'.\n  Types of property 'status' are incompatible.\n    Type 'number' is not assignable to type 'string'.";
const SEND_WHOLE = "Type 'string' is not assignable to type 'O1_0'.";
const form = (extra: Record<string, unknown>, message: string, symbol?: string) =>
  heldDetail({ message, symbol: symbol ?? null, ...extra } as never);
const EMIT = { side: "consumer", protocol: "socket" };
const CALL = { side: "consumer", protocol: "http", reads: true };

test("today's producer sentence, byte for byte", () => {
  assert.equal(
    say(SEND_DIFFERS),
    "'status' has a different type. This code sends 'number'; the caller expects 'string'.",
  );
  assert.equal(
    say(SEND_MISSING, "OrderView"),
    "'status' is missing. This code sends '{ id: string; }'; the caller expects 'OrderView'.",
  );
  assert.equal(say(SEND_WHOLE, "OrderView"), "This code sends 'string'; the caller expects 'OrderView'.");
});

test("a socket emitter's break names the listener, and the edited side as this code", () => {
  assert.equal(
    form(EMIT, SEND_DIFFERS),
    "'status' has a different type. This code sends 'number'; the listener expects 'string'.",
  );
  assert.equal(
    form(EMIT, SEND_MISSING, "OrderUpdate"),
    "'status' is missing. This code sends '{ id: string; }'; the listener expects 'OrderUpdate'.",
  );
  assert.equal(form(EMIT, SEND_WHOLE, "OrderUpdate"), "This code sends 'string'; the listener expects 'OrderUpdate'.");
  // The edited side sends.
  assert.match(form(EMIT, SEND_DIFFERS)!, /This code sends /);
});

test("an HTTP caller's break names the route as sender and this code as reader", () => {
  assert.equal(
    form(CALL, SEND_DIFFERS),
    "'status' has a different type. The route sends 'number'; this code expects 'string'.",
  );
  assert.equal(
    form(CALL, SEND_MISSING, "OrderView"),
    "'status' is missing. The route sends '{ id: string; }'; this code expects 'OrderView'.",
  );
  assert.equal(form(CALL, SEND_WHOLE, "OrderView"), "The route sends 'string'; this code expects 'OrderView'.");
  // The edited side expects.
  assert.match(form(CALL, SEND_DIFFERS)!, /this code expects /);
});

test("a consumer line of another protocol or form states nothing", () => {
  assert.equal(form({ side: "consumer", protocol: "pubsub" }, SEND_DIFFERS), null);
  assert.equal(form({ side: "consumer", protocol: "graphql", reads: true }, SEND_DIFFERS), null);
  assert.equal(form({ side: "consumer", protocol: "http" }, SEND_DIFFERS), null);
  assert.equal(form({ side: "consumer" }, SEND_DIFFERS), null);
  assert.equal(form({ side: "consumer", protocol: "socket", reads: true }, SEND_DIFFERS), null);
});

test("a producer line of any protocol keeps the producer sentence", () => {
  assert.equal(form({ protocol: "http" }, SEND_WHOLE, "OrderView"), say(SEND_WHOLE, "OrderView"));
  assert.equal(form({ protocol: "graphql" }, SEND_WHOLE, "OrderView"), say(SEND_WHOLE, "OrderView"));
});
