// One same-origin application frame. This is a development UI, not an isolation boundary.
(() => {
  'use strict';
  const select = document.querySelector('#project');
  const frame = document.querySelector('#project-preview');
  const reload = document.querySelector('#reload');
  const status = document.querySelector('#status');
  const origin = location.origin;
  let shellOrigin = window.location.ancestorOrigins?.[0];
  if (!shellOrigin && document.referrer) {
    try { shellOrigin = new URL(document.referrer).origin; } catch {}
  }
  const postShell = data => { if (window.parent !== window) window.parent.postMessage(data, shellOrigin || '*'); };
  // Preserve the project selection in Sandstorm's address bar and on a shell refresh.
  postShell({setPath: location.pathname + location.search});
  select.addEventListener('change', () => {
    location.assign('/__spki/open/' + encodeURIComponent(select.value));
  });
  if (!frame) return;

  const pending = new Map();
  const currentDocument = () => { try { return frame.contentDocument; } catch { return null; } };
  function prune() {
    const doc = currentDocument();
    for (const [id, request] of pending) if (request.document !== doc) pending.delete(id);
  }
  function loaded() {
    prune();
    // A grain restart loses the server-side selection. Reopen the project before
    // loading the app again, rather than nesting a workspace page in its own frame.
    if (currentDocument()?.documentElement.hasAttribute('data-spki-workspace')) {
      location.reload();
      return;
    }
    status.textContent = 'Loaded';
    reload.disabled = false;
  }
  status.textContent = 'Loading…';
  frame.addEventListener('load', loaded);
  reload.addEventListener('click', () => {
    status.textContent = 'Loading…';
    try { frame.contentWindow.location.reload(); }
    catch { frame.src = frame.dataset.initialPath; }
  });

  function localPath(value) {
    if (typeof value !== 'string' || value.length > 8192 || !value.startsWith('/') || value.startsWith('//') || /[\\\u0000-\u001f\u007f]/.test(value)) return false;
    try {
      const url = new URL(value, origin);
      return url.origin === origin && !decodeURIComponent(url.pathname).startsWith('/__spki');
    } catch { return false; }
  }
  function replyError(rpcId, message) {
    frame.contentWindow.postMessage({rpcId, error: message}, origin);
  }
  window.addEventListener('message', event => {
    const data = event.data;
    if (!data || typeof data !== 'object' || Array.isArray(data)) return;
    if (event.source === window.parent && window.parent !== window) {
      if (shellOrigin && event.origin !== shellOrigin) return;
      const request = pending.get(data.rpcId);
      if (!request) return;
      if (request.document !== currentDocument()) { pending.delete(data.rpcId); return; }
      if (!request.subscribe || data.error) pending.delete(data.rpcId);
      frame.contentWindow.postMessage({...data, rpcId: request.rpcId}, origin);
      return;
    }
    if (event.source !== frame.contentWindow || event.origin !== origin || !currentDocument()) return;
    prune();
    if (Object.hasOwn(data, 'setPath')) {
      if (!localPath(data.setPath)) return;
      // Avoid Sandstorm's initial-URL decoding changing nested query/hash delimiters.
      const encoded = btoa(String.fromCharCode(...new TextEncoder().encode(data.setPath))).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
      const path = '/__spki/open/' + encodeURIComponent(select.value) + '?path=' + encoded;
      history.replaceState(null, '', path);
      postShell({setPath: path});
      return;
    }
    // These calls carry no reply capability; Sandstorm validates their payloads.
    const notices = ['setTitle', 'startSharing', 'overlaySignin', 'requestLogout', 'showConnectionGraph'];
    const notice = notices.find(key => Object.hasOwn(data, key));
    if (notice) { postShell({[notice]: data[notice]}); return; }
    const kind = ['powerboxRequest', 'renderTemplate', 'getGrainTitle'].find(key => Object.hasOwn(data, key));
    if (!kind || !data[kind] || typeof data[kind] !== 'object' || Array.isArray(data[kind])) return;
    const rpcId = kind === 'getGrainTitle' ? data.rpcId : data[kind].rpcId;
    if (typeof rpcId !== 'string' || rpcId.length > 256) return;
    if (pending.size >= 64) { replyError(rpcId, 'Too many pending Sandstorm requests'); return; }
    const relayId = Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, '0')).join('');
    pending.set(relayId, {rpcId, document: currentDocument(), subscribe: kind === 'getGrainTitle' && data.subscribe === true});
    if (kind === 'getGrainTitle') postShell({getGrainTitle: data.getGrainTitle, rpcId: relayId, subscribe: data.subscribe === true});
    else postShell({[kind]: {...data[kind], rpcId: relayId}});
  });
  // Install the relay before app scripts can send their first messages.
  frame.src = frame.dataset.initialPath;
})();
