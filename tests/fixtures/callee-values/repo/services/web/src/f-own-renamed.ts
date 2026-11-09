function send2(p: string) {
  return fetch(`https://api.example.test${p}`);
}

const d = { go: send2 };

export function f() {
  return d.go("/p/f-own-renamed");
}
