import router from "fixture-router";
import { regA, regB, regC } from "./registries";

// A: the destructured key at the top level of a function body.
export async function startA(name: keyof typeof regA): Promise<void> {
  const app = router();
  const { default: init } = await regA[name]();
  init(app);
}

// B: the destructured key in a for-of body inside a function, the key
// narrowed with `as`, the call handed to another call.
export async function startB(names: string[]): Promise<void> {
  const app = router();
  for (const name of names) {
    const { default: init } = await regB[name as keyof typeof regB]();
    await Promise.resolve(init(app));
  }
}

// C: the destructured key in a callback written inside a function, the app
// captured from the enclosing body.
export function startC(names: (keyof typeof regC)[]): void {
  const app = router();
  names.forEach(async (name) => {
    const { default: init } = await regC[name]();
    init(app);
  });
}
