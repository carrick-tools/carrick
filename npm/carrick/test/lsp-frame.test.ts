import assert from "node:assert/strict";
import { test } from "node:test";
import { Framer, frame } from "../src/lsp-frame.ts";

test("a message split across chunks comes out whole, once", () => {
  const framer = new Framer();
  const bytes = frame({ id: 1, method: "x", params: { text: "héllo" } });
  assert.deepEqual(framer.push(bytes.subarray(0, 10)), []);
  assert.deepEqual(framer.push(bytes.subarray(10, bytes.length - 3)), []);
  const out = framer.push(bytes.subarray(bytes.length - 3));
  assert.equal(out.length, 1);
  assert.equal(out[0]?.method, "x");
  assert.deepEqual(out[0]?.params, { text: "héllo" });
});

test("two messages in one chunk come out in order", () => {
  const framer = new Framer();
  const out = framer.push(Buffer.concat([frame({ id: 1 }), frame({ id: 2 })]));
  assert.deepEqual(out.map((m) => m.id), [1, 2]);
});

test("a header without Content-Length is dropped and reading resumes", () => {
  const errors: string[] = [];
  const framer = new Framer((what) => errors.push(what));
  const out = framer.push(Buffer.concat([Buffer.from("X-Other: 1\r\n\r\n"), frame({ id: 7 })]));
  assert.deepEqual(out.map((m) => m.id), [7]);
  assert.equal(errors.length, 1);
});

test("a body that is not JSON is reported and skipped", () => {
  const errors: string[] = [];
  const framer = new Framer((what) => errors.push(what));
  const bad = Buffer.from("not json");
  const out = framer.push(
    Buffer.concat([Buffer.from(`Content-Length: ${bad.length}\r\n\r\n`), bad, frame({ id: 3 })]),
  );
  assert.deepEqual(out.map((m) => m.id), [3]);
  assert.equal(errors.length, 1);
});
