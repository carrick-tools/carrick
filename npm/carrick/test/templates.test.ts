// The templates, which two writers render: `carrick init` and the hosted
// scaffold tool. A placeholder left unrendered is the failure that looks like
// success, so it is the one asserted hardest.

import assert from "node:assert/strict";
import test from "node:test";
import {
  DEFAULTS,
  TEMPLATE_NAMES,
  TEMPLATE_PATHS,
  renderTemplate,
  scaffoldFiles,
  templateSource,
} from "../src/templates.ts";

test("the workflow renders with the action ref and the branch filled in", () => {
  const rendered = renderTemplate("workflow", {
    ACTION_REF: "carrick-tools/carrick@v1",
    DEFAULT_BRANCH: "trunk",
  });
  assert.match(rendered, /^name: Carrick\n/);
  assert.match(rendered, /branches: \[trunk\]/);
  assert.match(rendered, /- uses: carrick-tools\/carrick@v1/);
  // The Action's own expression syntax must survive rendering untouched.
  assert.doesNotMatch(rendered, /\{\{[A-Z_]+\}\}/);
});

test("the defaults are the ones a user would want without saying anything", () => {
  const rendered = renderTemplate("workflow");
  assert.match(rendered, /branches: \[main\]/);
  assert.match(rendered, new RegExp(`- uses: ${DEFAULTS["ACTION_REF"]?.replace("/", "\\/")}`));
});

test("the workflow asks for OIDC and no secret", () => {
  const rendered = renderTemplate("workflow");
  assert.match(rendered, /id-token: write/);
  assert.doesNotMatch(rendered, /secrets\./);
  // A shallow checkout costs the incremental scan its diff.
  assert.match(rendered, /fetch-depth: 0/);
});

test("carrick.json is the flat single-service skeleton, and valid JSON", () => {
  const rendered = renderTemplate("carrick.json");
  assert.deepEqual(JSON.parse(rendered), {
    serviceName: "",
    internalEnvVars: [],
    externalEnvVars: [],
    internalDomains: [],
    externalDomains: [],
  });
});

test("every template renders and lands somewhere named", () => {
  for (const name of TEMPLATE_NAMES) {
    assert.ok(templateSource(name).length > 0, `${name} is empty`);
    assert.ok(TEMPLATE_PATHS[name], `${name} has no path`);
  }
  const files = scaffoldFiles();
  assert.deepEqual(
    files.map((file) => file.path).sort(),
    [".github/workflows/carrick.yml", "carrick.json"],
  );
});

test("a template that still holds a placeholder is an error, not a file", () => {
  assert.throws(
    () => renderTemplate("workflow", { ACTION_REF: "{{ACTION_REF}}" }),
    /still holds/,
  );
});
