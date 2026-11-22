'use strict';

// Shared by Browse and the read-only search viewer.
class SearchPages {
  constructor(pagination, status, fetchPage) {
    this.pagination = pagination;
    this.status = status;
    this.fetchPage = fetchPage;
    this.pages = new Map();
    this.number = new Intl.NumberFormat();
  }

  reset() {
    this.cancelPrefetch();
    this.countRequest?.abort();
    this.countRequest = null;
    this.countUnavailable = false;
    this.total = null;
    this.pages.clear();
  }

  cancelPrefetch() {
    this.observer?.disconnect();
    this.observer = null;
    this.pending?.controller.abort();
    this.pending = null;
  }

  key(params) {
    const sorted = new URLSearchParams(params);
    sorted.sort();
    return sorted.toString();
  }

  prefetch(params) {
    if (navigator.connection?.saveData || !('IntersectionObserver' in window)) return;
    const key = this.key(params);
    if (this.pages.has(key)) return;
    const observer = new IntersectionObserver(entries => {
      if (this.observer !== observer || !entries.some(entry => entry.isIntersecting)) return;
      observer.disconnect();
      this.observer = null;
      const controller = new AbortController();
      const promise = this.fetchPage(params, {signal: controller.signal, priority: 'low'})
        .catch(() => null);
      this.pending = {key, controller, promise};
    }, {rootMargin: '0px 0px 900px'});
    this.observer = observer;
    observer.observe(this.pagination);
  }

  async load(params, signal) {
    signal.throwIfAborted();
    const key = this.key(params);
    const pending = this.pending?.key === key ? this.pending : null;
    if (!pending) this.cancelPrefetch();
    if (this.pages.has(key)) return this.pages.get(key);
    let data;
    if (pending) {
      const abort = () => pending.controller.abort();
      signal.addEventListener('abort', abort, {once: true});
      this.pending = null;
      try {
        data = await pending.promise;
      } finally {
        signal.removeEventListener('abort', abort);
      }
    }
    signal.throwIfAborted();
    data ??= await this.fetchPage(params, {signal});
    signal.throwIfAborted();
    this.pages.set(key, data);
    if (this.pages.size > 3) this.pages.delete(this.pages.keys().next().value);
    return data;
  }

  describe(data, page, limit) {
    const format = value => this.number.format(value);
    const numbered = !data.scan || (page === 1 && !data.has_more);
    const pages = this.total == null || !numbered ? null : Math.ceil(this.total / limit);
    const position = pages === 0 ? '0 pages'
      : pages != null && page <= pages ? `Page ${format(page)} of ${format(pages)}`
      : `Page ${format(page)}`;
    const count = this.total == null ? `${format(data.messages.length)} shown`
      : `${format(this.total)} ${this.total === 1 ? 'result' : 'results'}`;
    const detail = data.partial ? ' · More history remains'
      : this.countUnavailable && this.total == null ? ' · Total unavailable'
      : this.total == null ? ' · Counting…' : '';
    this.status.textContent = `${count} · ${position}${detail}`;
  }

  summarize(data, page, params, {scopeBefore = null} = {}) {
    const limit = Number(params.get('limit'));
    if (data.total != null) {
      this.total = data.total;
    } else if (page === 1 && !data.has_more && params.get('before') === scopeBefore) {
      this.total = data.messages.length;
    } else if (this.total == null && !this.countRequest && !this.countUnavailable) {
      const query = new URLSearchParams(params);
      for (const key of ['page', 'limit', 'scan', 'cursor', 'before', 'search', 'profile']) query.delete(key);
      if (scopeBefore) query.set('before', scopeBefore);
      const request = new AbortController();
      this.countRequest = request;
      fetch(`/api/search/count?${query}`, {signal: request.signal, priority: 'low', cache: 'no-store'})
        .then(response => response.ok ? response.json() : null)
        .then(count => {
          if (!request.signal.aborted) this.total = count?.total ?? null;
        })
        .catch(() => {})
        .finally(() => {
          if (request.signal.aborted) return;
          this.countRequest = null;
          this.countUnavailable = this.total == null;
          this.describe(this.visible.data, this.visible.page, this.visible.limit);
        });
    }
    this.visible = {data, page, limit};
    this.describe(data, page, limit);
  }
}
