// A task factory run only through a value (`kit.make`) and from a
// builder's method that hands its own parameter on through a spread
// (`make(options) { return makeTask({ ...options, tag }) }`).
export type TaskOptions = { handle(input: string): string; tag?: string };

function makeTask(options: TaskOptions): () => string {
  const { handle: onInput, ...rest } = options;
  return () => onInput(rest.tag || 'none');
}

function makeBuilder(tag: string): { make(options: TaskOptions): () => string } {
  return {
    make(options: TaskOptions) {
      return makeTask({ ...options, tag });
    },
  };
}

export const kit = { make: makeTask, builder: makeBuilder };

export function boot(): string {
  const task = makeBuilder('boot').make({ handle: (input) => input.toUpperCase() });
  return task();
}
