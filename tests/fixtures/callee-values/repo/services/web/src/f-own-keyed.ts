function send2(p: string) {
  return fetch(`https://api.example.test${p}`);
}

const d = { send2 };

export function f() {
  return d["send2"]("/p/f-own-keyed");
}
