import test from "node:test";
import assert from "node:assert/strict";
import { ownsDelivery, resolveChannel } from "../src/channel.ts";

test("the hook owns delivery wherever the hook is installed", () => {
  const choice = resolveChannel({ env: {}, hooksInstalled: true });
  assert.equal(choice.channel, "hook");
  assert.equal(choice.source, "default");
  assert.equal(ownsDelivery("hook", choice), true);
  assert.equal(ownsDelivery("lsp", choice), false);
});

test("a client with no hook gets the language server", () => {
  const choice = resolveChannel({ env: {}, hooksInstalled: false });
  assert.equal(choice.channel, "lsp");
  assert.equal(ownsDelivery("lsp", choice), true);
});

test("CARRICK_CHANNEL pins one arm", () => {
  assert.equal(resolveChannel({ env: { CARRICK_CHANNEL: "lsp" }, hooksInstalled: true }).channel, "lsp");
  assert.equal(resolveChannel({ env: { CARRICK_CHANNEL: "HOOK" }, hooksInstalled: false }).channel, "hook");
  assert.equal(resolveChannel({ env: { CARRICK_CHANNEL: "off" }, hooksInstalled: true }).channel, "off");
});

test("a value that is not a channel is ignored and reported", () => {
  const choice = resolveChannel({ env: { CARRICK_CHANNEL: "both" }, hooksInstalled: true });
  assert.equal(choice.channel, "hook");
  assert.equal(choice.ignoredValue, "both");
});
