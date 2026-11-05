// The files Carrick asks a repo to hold, as files.
//
// `carrick templates <name>` prints them, so a file is one command and a
// public template is one a user can read before running anything.
//
// Placeholders are `{{NAME}}`. Not `${{ }}`, which is GitHub Actions'
// own syntax and appears in the workflow this renders.
//
// There is no carrick.json template: the config every scan runs under is the
// one Carrick holds for the project (carrick#3361).

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export type TemplateName = "workflow";

type Template = { file: string; variables: string[] };

const TEMPLATES: Record<TemplateName, Template> = {
  // Written to .github/workflows/carrick.yml.
  workflow: { file: "carrick.yml", variables: ["ACTION_REF", "DEFAULT_BRANCH"] },
};

export const TEMPLATE_NAMES = Object.keys(TEMPLATES) as TemplateName[];

/** Where a repo puts each one. */
export const TEMPLATE_PATHS: Record<TemplateName, string> = {
  workflow: ".github/workflows/carrick.yml",
};

export const DEFAULTS: Record<string, string> = {
  ACTION_REF: "carrick-tools/carrick@v1",
  DEFAULT_BRANCH: "main",
};

/**
 * The directory the shipped template files sit in.
 *
 * Resolved from this module rather than from the process, because the same
 * resolution has to hold in the checkout (`src/../templates`) and in the
 * published emit (`dist/../templates`). `init/task-skills.ts` reads the skill
 * bodies from a subdirectory of it.
 */
export function templatesDir(): string {
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
