// A module whose entire surface is one object: the runtime imports the
// default export and calls the member it names. Plain `.js`, no build step,
// no framework import — the only function in the file is a method on an
// object literal.
const UPSTREAM = "upstream.example.invalid";

export default {
  async fetch(request) {
    const url = new URL(request.url);
    url.host = UPSTREAM;
    url.protocol = "https:";
    const headers = new Headers(request.headers);
    headers.delete("cookie");
    return fetch(new Request(url, { method: request.method, headers }));
  },
};
