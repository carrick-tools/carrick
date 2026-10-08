export interface Removal {
  removed: boolean;
}

export function removeItem(): Removal {
  return { removed: true };
}
