// This helper has only the browser session's read-only revision endpoint.
(() => {
  const script = new URL(document.currentScript.src);
  const loaded = script.searchParams.get('revision');
  if (!/^[a-f0-9]{64}$/.test(loaded || '')) return;
  const endpoint = new URL('revision', script);
  let delay = 1000;
  let stopped = false;
  let timer;
  let pending;
  let generation = 0;
  async function poll() {
    if (stopped) return;
    const epoch = generation;
    const controller = new AbortController();
    pending = controller;
    const timeout = setTimeout(() => controller.abort(), 5000);
    try {
      const response = await fetch(endpoint, {
        cache: 'no-store', credentials: 'same-origin', signal: controller.signal,
      });
      if (!response.ok) throw new Error('Revision unavailable');
      const {revision} = await response.json();
      if (!/^[a-f0-9]{64}$/.test(revision || '')) throw new Error('Invalid revision');
      if (!stopped && epoch === generation && revision !== loaded) {
        stopped = true;
        location.reload();
        return;
      }
      delay = 1000;
    } catch {
      // A suspended grain or network interruption must not cause a reload loop.
      delay = Math.min(delay * 2, 10000);
    } finally {
      clearTimeout(timeout);
      if (pending === controller) pending = undefined;
      if (!stopped && epoch === generation) timer = setTimeout(poll, delay);
    }
  }
  addEventListener('pagehide', () => {
    stopped = true;
    generation++;
    clearTimeout(timer);
    pending?.abort();
  });
  // A document restored from the back/forward cache must resume polling.
  addEventListener('pageshow', event => {
    if (event.persisted) { stopped = false; timer = setTimeout(poll, 0); }
  });
  timer = setTimeout(poll, 1000);
})();
