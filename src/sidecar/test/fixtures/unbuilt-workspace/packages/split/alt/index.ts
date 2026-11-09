export interface Part {
  size: string;
}

export function part(): Part {
  return { size: 'one' };
}
