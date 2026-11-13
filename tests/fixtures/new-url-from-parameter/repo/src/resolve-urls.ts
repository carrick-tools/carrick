// `new URL(input, base)` resolves by the URL standard (carrick#4124).
declare function apiBase(): string;

// An absolute input is itself; the base plays no part.
export async function absoluteInput() {
  return fetch(new URL("https://api.example.test/v1/things", apiBase()));
}

// A relative input on a base ending in `/` lands under the base's path.
export async function relativeUnderBase() {
  return fetch(new URL("things/list", "https://api.example.test/v2/"));
}

// Without the trailing `/`, the base's last segment is replaced.
export async function relativeReplacesLastSegment() {
  return fetch(new URL("things/list", "https://api.example.test/v2"));
}

// A relative input on a base the source does not write whole.
export async function relativeOnRuntimeBase() {
  return fetch(new URL("things/list", apiBase()));
}

// Control: the same base as a template prefix.
export async function templateOnRuntimeBase() {
  return fetch(`${apiBase()}/things/list`);
}
