import {exportCapnp, sandstorm} from 'sandstorm:api';
import {App} from 'capnp:./schemas/app.capnp';
import {WebSession} from 'capnp:/sandstorm/web-session.capnp';

export default {
  connect(request: Request, env) {
    return exportCapnp(sandstorm(request, env), App, {
      async read({api}) {
        // The browser passes authority, not a URL or bearer token. This call goes
        // through the capability granted by Sandstorm's Powerbox provider.
        const response = await api.get({path: '', context: {}, ignoreBody: false});
        if (response.which() !== WebSession.Response.CONTENT) throw new Error('HTTP provider did not return content');
        return {text: new TextDecoder().decode(response.content.body.bytes.toUint8Array().subarray(0, 8192))};
      },
    });
  },
  fetch() { return new Response('Not Found', {status: 404}); },
};
