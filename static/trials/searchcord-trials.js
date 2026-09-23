(() => {
  const queryForms = document.querySelectorAll('.query-form');
  const queryBlock = document.querySelector('#queryBlock');
  const filterForm = document.querySelector('#filterForm');
  const queryInput = document.querySelector('.query-form input');
  const status = document.querySelector('#resultsStatus');
  const resultsSection = document.querySelector('#resultsSection');
  const surfaceTabs = [...document.querySelectorAll('.surface-tab')];
  const rows = [...document.querySelectorAll('.result-row')];
  const pagination = document.querySelector('#pagination');
  const previousPage = document.querySelector('#previousPage');
  const nextPage = document.querySelector('#nextPage');
  const currentPageLabel = document.querySelector('#currentPage');
  const pageCountLabel = document.querySelector('#pageCount');
  const pageSize = 3;
  let matchedRows = [];
  let currentPage = 1;

  if (!filterForm || !queryInput || !status || !rows.length) return;

  const normalize = value => String(value ?? '').trim().toLowerCase();

  function setFilterTrayExpanded(expanded) {
    queryBlock?.classList.toggle('is-expanded', expanded);
    queryInput.setAttribute('aria-expanded', String(expanded));
  }

  function setActiveView(view) {
    surfaceTabs.forEach(tab => tab.setAttribute('aria-selected', String(tab.dataset.view === view)));
    if (resultsSection) resultsSection.hidden = true;
  }

  function showResults(shouldScroll = false) {
    if (!resultsSection) return;
    resultsSection.hidden = false;
    if (shouldScroll) {
      requestAnimationFrame(() => resultsSection.scrollIntoView({ behavior: 'smooth', block: 'start' }));
    }
  }

  function renderPage() {
    const pageCount = Math.max(1, Math.ceil(matchedRows.length / pageSize));
    currentPage = Math.min(currentPage, pageCount);
    const firstVisible = (currentPage - 1) * pageSize;
    const pageRows = new Set(matchedRows.slice(firstVisible, firstVisible + pageSize));

    rows.forEach(row => {
      row.hidden = !pageRows.has(row);
    });

    if (matchedRows.length) {
      const lastVisible = Math.min(firstVisible + pageSize, matchedRows.length);
      status.textContent = `${firstVisible + 1}-${lastVisible} of ${matchedRows.length} messages`;
    } else {
      status.textContent = 'No messages match these filters';
    }

    if (pagination) pagination.hidden = matchedRows.length <= pageSize;
    if (previousPage) previousPage.disabled = currentPage === 1;
    if (nextPage) nextPage.disabled = currentPage === pageCount;
    if (currentPageLabel) currentPageLabel.textContent = `page ${currentPage}`;
    if (pageCountLabel) pageCountLabel.textContent = `${pageCount} ${pageCount === 1 ? 'page' : 'pages'}`;
  }

  function updateResults() {
    const data = new FormData(filterForm);
    const query = normalize(queryInput.value);
    const server = normalize(data.get('server'));
    const channel = normalize(data.get('channel'));
    const author = normalize(data.get('author'));
    const from = data.get('from') || '';
    const to = data.get('to') || '';

    matchedRows = rows.filter(row => (!query || row.dataset.search.includes(query))
      && (!server || normalize(row.dataset.server) === server)
      && (!channel || normalize(row.dataset.channel) === channel)
      && (!author || normalize(row.dataset.author) === author)
      && (!from || row.dataset.date >= from)
      && (!to || row.dataset.date <= to));
    currentPage = 1;
    renderPage();
  }

  function assignAvatarVariants() {
    const avatars = [...document.querySelectorAll('.message-avatar')];
    const variants = ['avatar-a', 'avatar-b', 'avatar-c', 'avatar-d', 'avatar-e', 'avatar-f'];
    const offset = Math.floor(Math.random() * variants.length);

    avatars.forEach((avatar, index) => {
      avatar.classList.add(variants[(offset + index) % variants.length]);
    });
  }

  queryForms.forEach(form => form.addEventListener('submit', event => {
    event.preventDefault();
    setFilterTrayExpanded(true);
    updateResults();
    showResults(true);
  }));
  queryInput.addEventListener('focus', () => {
    setFilterTrayExpanded(true);
  });
  queryInput.addEventListener('click', () => {
    setFilterTrayExpanded(true);
  });
  queryInput.addEventListener('input', updateResults);
  filterForm.addEventListener('input', updateResults);
  filterForm.addEventListener('change', updateResults);
  filterForm.addEventListener('reset', () => requestAnimationFrame(updateResults));

  previousPage?.addEventListener('click', () => {
    if (currentPage > 1) {
      currentPage -= 1;
      renderPage();
    }
  });
  nextPage?.addEventListener('click', () => {
    const pageCount = Math.max(1, Math.ceil(matchedRows.length / pageSize));
    if (currentPage < pageCount) {
      currentPage += 1;
      renderPage();
    }
  });

  document.addEventListener('pointerdown', event => {
    if (!queryBlock?.contains(event.target)) setFilterTrayExpanded(false);
  });
  document.addEventListener('keydown', event => {
    if (event.key === 'Escape') setFilterTrayExpanded(false);
  });

  surfaceTabs.forEach(tab => {
    tab.addEventListener('click', () => {
      setActiveView(tab.dataset.view);
    });
  });

  assignAvatarVariants();
  updateResults();
})();
