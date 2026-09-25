import http from "@example/http";

const client = http.create({ baseURL: process.env.API_URL });

export default client;
