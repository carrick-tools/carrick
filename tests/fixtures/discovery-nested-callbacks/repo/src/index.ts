export function go() {
  return Promise.resolve(0).then((v) => {
    return fetch("/api/leaf");
  });
}
