// An action that reads the table only through the members it is handed.
export type ActionDeps = {
  loadRow(key: string): Promise<unknown>;
  listRows(): Promise<unknown>;
  mint?: () => string;
};

export async function runAction(key: string, deps: ActionDeps): Promise<unknown> {
  await deps.loadRow(key);
  return deps.listRows();
}

// Run from one caller that hands it a value the resolver does not read.
export async function runParsedAction(key: string, deps: ActionDeps): Promise<unknown> {
  return deps.loadRow(key);
}

// Runs the loader it is handed.
export async function runLoader(key: string, load: (key: string) => Promise<unknown>): Promise<unknown> {
  return load(key);
}
