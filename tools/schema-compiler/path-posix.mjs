// Only POSIX paths are used in CodeGeneratorRequest schema names. The generator
// needs no filesystem or working directory; all names share one logical root.
export function normalize(path) {
  const absolute = path.startsWith('/');
  const parts = [];
  for (const part of path.split('/')) {
    if (!part || part === '.') continue;
    if (part === '..' && parts.length && parts.at(-1) !== '..') parts.pop();
    else if (part !== '..' || !absolute) parts.push(part);
  }
  return (absolute ? '/' : '') + parts.join('/') || '.';
}
export function dirname(path) {
  const value = normalize(path);
  const slash = value.lastIndexOf('/');
  return slash < 0 ? '.' : value.slice(0, slash) || '/';
}
export function join(...parts) { return normalize(parts.filter(Boolean).join('/')); }
export function relative(from, to) {
  const a = normalize('/' + from).split('/').filter(Boolean);
  const b = normalize('/' + to).split('/').filter(Boolean);
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) ++i;
  return [...Array(a.length - i).fill('..'), ...b.slice(i)].join('/');
}
