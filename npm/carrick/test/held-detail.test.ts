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
