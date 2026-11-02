export interface Store {
  read(): number;
}

export function makeStore(): Store {
  function read(): number {
    return 1;
  }
  return { read };
}
