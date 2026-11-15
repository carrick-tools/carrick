declare function make(): { run(): void };

const tools = { run: (): void => {} };

// A key destructured in a block, read in part: one branch is a definition,
// the other a call nothing in the repo answers.
export function partial(flag: boolean): void {
  if (flag) {
    const { run } = flag ? tools : make();
    run();
  }
}
