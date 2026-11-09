export const d = {
  a: (p: string) => fetch(`https://api.example.test${p}`),
};

export function f() {
  return d["a"]("/p/f-exported-arrow-keyed");
}
