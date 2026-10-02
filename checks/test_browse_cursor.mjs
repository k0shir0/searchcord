// Behavioral checks without browser layout or network dependencies.
// node checks/test_browse_cursor.mjs
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

class Element {
  value = ''; children = []; attributes = {}; hidden = false;
  classList = {toggle() {}, add() {}, remove() {}};
  setAttribute(key, value) { this.attributes[key] = value; }
  removeAttribute(key) { delete this.attributes[key]; }
  setCustomValidity() {} reportValidity() {} focus() {} scrollIntoView() {}
  addEventListener() {}
  appendChild(child) { child.parent = this; this.children.push(child); }
  append(...children) { children.forEach(child => this.appendChild(child)); }
  replaceChildren(...children) { this.children = []; this.append(...children); }
}
const nodes = new Map();
const document = {
  addEventListener() {}, querySelectorAll: () => [],
  getElementById(id) { if (!nodes.has(id)) nodes.set(id, new Element()); return nodes.get(id); },
  createElement: () => new Element(),
};
const requests = [];
const responses = [];
const context = vm.createContext({document, URLSearchParams, AbortController, Intl,
  Image: Element, location: {pathname: '/', search: '', hash: ''},
  history: {replaceState() {}}, matchMedia: () => ({matches: true}),
  fetch: async url => {
    requests.push(new URL(url, 'http://localhost'));
    return {ok: true, json: async () => responses.shift()};
  },
});
vm.runInContext(readFileSync(new URL('../static/app.js', import.meta.url), 'utf8'), context);
vm.runInContext('switchView = async () => {}; renderResults = data => { globalThis.result = data; };', context);
const run = code => vm.runInContext(code, context);
const page = next => ({total: null, pages: null, page: 1, limit: 50, messages: [], has_more: !!next, next_cursor: next});
document.getElementById('searchInput').value = 'hello';
responses.push(page('100')); await run('doSearch(1)');
assert.equal(requests.at(-1).searchParams.get('cursor'), 'true');
assert.equal(requests.at(-1).searchParams.has('before'), false);
responses.push(page('50')); await run('doSearch(2, false, true)');
assert.equal(requests.at(-1).searchParams.get('before'), '100');
responses.push(page('100')); await run('doSearch(1, false, true)');
assert.equal(requests.at(-1).searchParams.has('before'), false);
document.getElementById('searchInput').value = 'new query';
responses.push(page(null)); await run('doSearch(1)');
assert.equal(requests.at(-1).searchParams.get('q'), 'new query');
assert.equal(requests.at(-1).searchParams.has('before'), false);
run("submittedSearch = new URLSearchParams('q=hello&page=3');");
responses.push({total: 200, pages: 4, page: 3, messages: []}); await run('doSearch(3, false, true)');
assert.equal(requests.at(-1).searchParams.get('page'), '3');
assert.equal(requests.at(-1).searchParams.has('cursor'), false);
run('renderPagination(1, null, true)');
assert.equal(nodes.get('pagination').children.at(-1).disabled, false);
run('renderPagination(2, null, false)');
assert.equal(nodes.get('pagination').children.at(-1).disabled, true);
assert.equal(nodes.get('pagination').children[0].disabled, false);
for (const path of ['../static/app.js', '../static/search/search.js']) {
  const source = readFileSync(new URL(path, import.meta.url), 'utf8');
  run('globalThis.element = ce;');
  const start = source.indexOf('function messageAvatar(');
  const end = source.indexOf('\n}\n', start) + 3;
  vm.runInContext(source.slice(start, end), context);
  const avatar = run("messageAvatar('50', 'https://cdn.discordapp.com/test.png')");
  assert.equal(avatar.children.length, 1, path + ': lazy image is attached before loading');
  const image = avatar.children[0]; image.onload();
  assert.equal(avatar.children[0], image);
}
console.log('PASS: cursor next/previous/reset, legacy page links, pagination controls and attached lazy avatars in both frontends. DOM model only; no browser layout validation.');
