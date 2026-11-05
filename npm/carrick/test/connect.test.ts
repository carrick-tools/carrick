// The one dashboard page init names (carrick#3361): repos join a workspace
// through the GitHub App and the dashboard, never from the terminal.

import assert from "node:assert/strict";
import test from "node:test";
import { reposUrl } from "../src/init/connect.ts";

test("the repos page is the workspace's, with its slug escaped", () => {
  assert.equal(reposUrl("acme"), "https://app.carrick.tools/w/acme/repos");
  assert.equal(reposUrl("a b"), "https://app.carrick.tools/w/a%20b/repos");
});
