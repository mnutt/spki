import { exportCapnp, sandstorm } from "sandstorm:api";
import { ObjectStore } from "capnp:./object-store.capnp";
import { WebSession } from "capnp:/sandstorm/web-session.capnp";

const OBJECTS = Object.freeze({
  photos: Object.freeze({
    "2026/cover.txt": {
      contentType: "text/plain; charset=utf-8",
      body: "cover image placeholder\n",
    },
    "2026/thumb.txt": {
      contentType: "text/plain; charset=utf-8",
      body: "thumbnail placeholder\n",
    },
  }),
  docs: Object.freeze({
    "readme.txt": {
      contentType: "text/plain; charset=utf-8",
      body: "object-store example\n",
    },
  }),
});

function findObject(bucket, key) {
  if (!Object.hasOwn(OBJECTS, bucket)) return null;
  const objects = OBJECTS[bucket];
  return Object.hasOwn(objects, key) ? objects[key] : null;
}

function objectPath(bucket, key) {
  return `/objects/${encodeURIComponent(bucket)}/${encodeURIComponent(key)}`;
}

function objectInfo(key, object) {
  return {
    key,
    size: BigInt(new TextEncoder().encode(object.body).byteLength),
    contentType: object.contentType,
  };
}

function jsonObjectInfo(info) {
  return {
    key: info.key,
    size: Number(info.size),
    contentType: info.contentType,
  };
}

function jsonObjectListing(listing) {
  return {
    objects: Array.from(listing.objects || []).map(jsonObjectInfo),
    nextCursor: listing.nextCursor,
  };
}

function makeObjectStore() {
  return {
    async listObjects({ bucket = "", prefix = "", cursor = "" } = {}) {
      const entries = Object.entries(OBJECTS[bucket] || {})
        .filter(([key]) => key.startsWith(prefix))
        .sort(([left], [right]) => left.localeCompare(right));
      const start = cursor ? Math.max(0, entries.findIndex(([key]) => key > cursor)) : 0;
      const page = entries.slice(start, start + 100);
      return {
        objects: page.map(([key, object]) => objectInfo(key, object)),
        nextCursor: "",
      };
    },

    async openObject({ bucket = "", key = "" } = {}) {
      const object = findObject(bucket, key);
      if (!object) {
        throw new Error(`object not found: ${bucket}/${key}`);
      }
      return {object: new WebSession.Server({
        get({path = '', ignoreBody = false}) {
          if (path !== '') throw new Error('Object capability only exposes its root');
          return {content: {
            statusCode: WebSession.Response.SuccessCode.OK,
            mimeType: object.contentType,
            body: {bytes: ignoreBody ? new Uint8Array() : new TextEncoder().encode(object.body)},
          }};
        },
      }).client()};
    },
  };
}

function serveObject(url) {
  const match = url.pathname.match(/^\/objects\/([^/]+)\/([^/]+)$/);
  if (!match) return null;

  const bucket = decodeURIComponent(match[1]);
  const key = decodeURIComponent(match[2]);
  const object = findObject(bucket, key);
  if (!object) {
    return Response.json({ ok: false, error: "object not found" }, { status: 404 });
  }

  return new Response(object.body, {
    headers: {
      "content-type": object.contentType,
      "etag": `"${bucket}/${key}"`,
    },
  });
}

export default {
  connect(request, env) {
    const api = sandstorm(request, env);
    return exportCapnp(api, ObjectStore, makeObjectStore());
  },
  async fetch(request, env) {
    const api = sandstorm(request, env);

    const url = new URL(request.url);
    const objectResponse = serveObject(url);
    if (objectResponse) return objectResponse;

    const store = new ObjectStore.Server(makeObjectStore()).client();
    const listing = await store.listObjects({
      bucket: "photos",
      prefix: "2026/",
      cursor: "",
    });
    const object = await store.openObject({
      bucket: "photos",
      key: "2026/cover.txt",
    });
    const response = await object.object.get({
      path: "",
      context: {},
      ignoreBody: false,
    });
    const content = response.content;
    const body = content.body.bytes;
    const bodyBytes = typeof body.toUint8Array === "function" ? body.toUint8Array() : body;

    return Response.json({
      ok: true,
      interfaceName: "ObjectStore",
      interfaceId: `0x${ObjectStore._capnp.typeIdHex}`,
      listing: jsonObjectListing(listing),
      object: {
        statusCode: content.statusCode,
        contentType: content.mimeType,
        body: new TextDecoder().decode(bodyBytes),
      },
    });
  },
};
