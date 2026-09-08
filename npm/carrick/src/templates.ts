// The files Carrick asks a repo to hold, as files.
//
// One renderer for both of the places that write them: `carrick init` here,
// and the hosted `scaffold` tool, which depends on this package at a pinned
// version and renders the same bytes (carrick#710). Scaffold parity used to be
// a rule and a review check; a shared template makes it structural, and a
// public template is one a user can read before running anything.
//
// Placeholders are `{{NAME}}`. Not `${{ }}`, which is GitHub Actions'
// own syntax and appears in the workflow this renders.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export type TemplateName = "workflow" | "carrick.json";

type Template = { file: string; variables: string[] };

const TEMPLATES: Record<TemplateName, Template> = {
  // Written to .github/workflows/carrick.yml.
  workflow: { file: "carrick.yml", variables: ["ACTION_REF", "DEFAULT_BRANCH"] },
  // Written to the repo root, for the scanning agent to fill in.
  "carrick.json": { file: "carrick.json", variables: [] },
};

export const TEMPLATE_NAMES = Object.keys(TEMPLATES) as TemplateName[];

/** Where a repo puts each one. */
export const TEMPLATE_PATHS: Record<TemplateName, string> = {
  workflow: ".github/workflows/carrick.yml",
  "carrick.json": "carrick.json",
};

export const DEFAULTS: Record<string, string> = {
  ACTION_REF: "carrick-tools/carrick@v1",
  DEFAULT_BRANCH: "main",
};

function templatesDir(): string {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "templates");
}

export function templateSource(name: TemplateName): string {
  return fs.readFileSync(path.join(templatesDir(), TEMPLATES[name].file), "utf8");
}

/**
 * Render one template.
 *
 * Every placeholder must be filled: an unrendered `{{NAME}}` in a workflow is a
 * file that looks written and does not run, so it throws rather than shipping.
 */
export function renderTemplate(
  name: TemplateName,
  variables: Record<string, string> = {},
): string {
  const template = TEMPLATES[name];
  if (!template) throw new Error(`no carrick template named ${name}`);
  const values = { ...DEFAULTS, ...variables };
  let rendered = templateSource(name);
  for (const key of template.variables) {
    const value = values[key];
    if (value === undefined) throw new Error(`${name} needs a value for ${key}`);
    rendered = rendered.split(`{{${key}}}`).join(value);
  }
  const left = /\{\{([A-Z_]+)\}\}/.exec(rendered);
  if (left) throw new Error(`${name} still holds ${left[0]} after rendering`);
  return rendered;
}

/** Every file, ready to write, for a repo in a project. */
export function scaffoldFiles(
  variables: Record<string, string> = {},
): Array<{ path: string; contents: string }> {
  return TEMPLATE_NAMES.map((name) => ({
    path: TEMPLATE_PATHS[name],
    contents: renderTemplate(name, variables),
  }));
}
