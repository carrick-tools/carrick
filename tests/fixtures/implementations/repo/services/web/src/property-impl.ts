export interface Hook {
  fire(name: string): unknown;
}

function makeFire(): (name: string) => unknown {
  return (name) => fetch(`/hooks/${name}`, { method: "POST" });
}

class PropertyHook implements Hook {
  fire = makeFire();
}

const hook: Hook = new PropertyHook();

export function fireOnce(): unknown {
  return hook.fire("only-property");
}
