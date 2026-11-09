import {sandstorm} from 'sandstorm:api';
const version: string = 'first';
export default {
  async fetch(request: Request, env: unknown) {
    const count = (await sandstorm(request, env).storage().getJson('counter')) ?? 0;
    return new Response(`<!doctype html><html><head><meta charset="utf-8"><title>Reload test</title></head><body><h1 id="version">${version}</h1><p id="counter">${count}</p></body></html>`, {
      headers: {'content-type': 'text/html; charset=utf-8'},
    });
  },
};
