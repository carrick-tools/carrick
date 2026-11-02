import type { Reader } from "./ports.js";

function makeReader(): Reader {
  function get() {
    return 1;
  }
  return { get };
}

// A binding beside a definition: the program names both.
const { get } = makeReader();
export const reader: Reader = { get };
