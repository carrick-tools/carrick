export interface Flag {
  enabled: boolean;
}

export function readFlag(): Flag {
  return { enabled: true };
}
