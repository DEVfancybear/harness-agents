(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const state = {
    scope: new URL(location.href).searchParams.get('scope') === 'global' ? 'global' : 'project',
    status: null, pages: [], categories: [], currentPage: null, selectedPath: '',
    view: 'document', search: '', searchTimer: null, searchRequest: null,
    loadVersion: 0, pageVersion: 0, graphVersion: 0, busy: false,
    selecting: false, selected: new Set(), expanded: new Set(),
    graph: null,
  };
  const pageIconPath = 'M14 3H5v18h14V8Zm0 0v5h5M8 12h8M8 16h6';
  const arrowPath = 'm9 5 7 7-7 7';
  const svgNS = 'http://www.w3.org/2000/svg';

  function node(tag, className, text) {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = String(text);
    return element;
  }

  function icon(path = pageIconPath) {
    const svg = document.createElementNS(svgNS, 'svg');
    svg.setAttribute('viewBox', '0 0 24 24');
    svg.setAttribute('aria-hidden', 'true');
    const child = document.createElementNS(svgNS, 'path');
    child.setAttribute('d', path);
    svg.append(child);
    return svg;
  }

  function announce(message) { $('live-status').textContent = message; }

  function showError(error) {
    $('notice').replaceChildren(node('span', '', error instanceof Error ? error.message : error));
    const retry = node('button', '', 'Refresh');
    retry.type = 'button';
    retry.addEventListener('click', () => reload());
    $('notice').append(retry);
    $('notice').hidden = false;
  }

  function clearError() { $('notice').hidden = true; }

  async function api(path, options = {}) {
    const response = await fetch(path, {
      credentials: 'same-origin', cache: 'no-store',
      ...options,
      headers: { Accept: 'application/json', ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...options.headers },
    });
    let body;
    try { body = await response.json(); } catch { throw new Error('The local console returned an invalid response. Refresh and try again.'); }
    if (!response.ok) throw new Error(body.error || `Request failed (${response.status}).`);
    return body;
  }

  function apiUrl(path, values = {}) {
    const params = new URLSearchParams({ scope: state.scope, ...values });
    return `${path}?${params}`;
  }

  function enabled() {
    return !!state.status && state.status.project?.enabled !== false && state.status[state.scope]?.enabled !== false;
  }

  function route(path, scope = state.scope) {
    const params = new URLSearchParams({ scope });
    if (path) params.set('page', path);
    return `/?${params}`;
  }

  function updateRoute(path, replace = false) {
    const target = route(path);
    if (`${location.pathname}${location.search}` !== target) history[replace ? 'replaceState' : 'pushState']({ scope: state.scope, page: path }, '', target);
    document.title = path && state.currentPage ? `${state.currentPage.title || path} · HA Knowledge` : 'Knowledge · HA';
  }

  function plainContent(content) {
    return String(content || '').replace(/^\uFEFF?<!--\s*ha-knowledge:[^\r\n]*-->\r?\n?/, '');
  }

  function setEmpty(title, description) {
    $('empty-state').hidden = false;
    $('empty-state').querySelector('h2').textContent = title;
    $('empty-state').querySelector('p').textContent = description;
    $('document').hidden = true;
  }

  function updateSelection() {
    $('selection-actions').hidden = !state.selecting;
    $('selection-count').textContent = `${state.selected.size} selected`;
    $('move-selected').disabled = !state.selected.size || !enabled() || state.busy;
    $('delete-selected').disabled = !state.selected.size || !enabled() || state.busy;
    $('select-documents').setAttribute('aria-pressed', String(state.selecting));
    $('select-documents').textContent = state.selecting ? 'Done' : 'Select';
  }

  function updateControls() {
    const active = enabled();
    const layer = state.status?.[state.scope];
    $('scope').value = state.scope;
    $('scope').disabled = state.busy;
    $('scope-label').textContent = state.scope === 'global' ? 'Global library' : 'Project library';
    $('enabled').setAttribute('aria-checked', String(layer?.enabled !== false));
    $('enabled').setAttribute('aria-label', `Enable ${state.scope} knowledge`);
    $('enabled').disabled = state.busy || !state.status || (state.scope === 'global' && state.status.project?.enabled === false);
    $('enabled-label').textContent = layer?.enabled === false ? 'Knowledge is off' : 'Knowledge is on';
    $('enabled-detail').textContent = state.scope === 'global' && state.status?.project?.enabled === false ? 'Enable project knowledge first' : active ? 'Available to your agents' : 'Documents stay on your disk';
    $('search').disabled = !active;
    for (const id of ['new-document', 'import-documents', 'manage-categories', 'select-documents']) $(id).disabled = !active || state.busy;
    for (const id of ['edit-document', 'move-document', 'delete-document']) $(id).disabled = !active || state.busy || !state.currentPage;
    $('show-connections').disabled = !active;
    $('page-count').textContent = String(active ? (layer?.pages ?? state.pages.length) : 0);
    $('link-count').textContent = String(active ? (layer?.links ?? 0) : 0);
    updateSelection();
  }

  function pageLink(page, detailed = false) {
    const anchor = node('a', 'page-link');
    anchor.href = route(page.path);
    anchor.title = `${page.title || page.path}\n${page.path}`;
    anchor.dataset.path = page.path;
    if (page.path === state.selectedPath) anchor.setAttribute('aria-current', 'page');
    anchor.append(icon());
    const text = node('span', '', page.title || page.path.split('/').pop());
    if (detailed) text.append(node('small', '', page.path));
    anchor.append(text);
    anchor.addEventListener('click', (event) => {
      if (event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      event.preventDefault();
      openPage(page.path);
    });
    return anchor;
  }

  function selectablePage(page) {
    if (!state.selecting) return pageLink(page);
    const row = node('div', 'page-row');
    const checkbox = node('input', 'select-check');
    checkbox.type = 'checkbox';
    checkbox.checked = state.selected.has(page.path);
    checkbox.setAttribute('aria-label', `Select ${page.title || page.path}`);
    checkbox.addEventListener('change', () => {
      if (checkbox.checked) state.selected.add(page.path); else state.selected.delete(page.path);
      updateSelection();
    });
    row.append(checkbox, pageLink(page));
    return row;
  }

  function renderDirectory() {
    const directory = $('directory');
    directory.replaceChildren();
    directory.setAttribute('aria-busy', 'false');
    if (!enabled()) {
      $('library-count').textContent = 'Knowledge is disabled';
      directory.append(node('p', 'directory-message', state.scope === 'global' && state.status?.project?.enabled === false ? 'Project knowledge is off. Enable it to browse either library.' : 'Enable knowledge to browse your documents.'));
      return;
    }
    $('library-count').textContent = `${state.pages.length} document${state.pages.length === 1 ? '' : 's'} · ${state.scope === 'global' ? 'Shared across projects' : 'This project only'}`;
    if (!state.pages.length && !state.categories.length) {
      directory.append(node('p', 'directory-message', 'No documents yet. Create a document, import Markdown, or ask an agent to capture knowledge.'));
      return;
    }
    const root = { folders: new Map(), pages: [] };
    const getFolder = (parts) => {
      let branch = root;
      let path = '';
      for (const part of parts) {
        if (!part) continue;
        path = path ? `${path}/${part}` : part;
        if (!branch.folders.has(part)) branch.folders.set(part, { name: part, path, folders: new Map(), pages: [] });
        branch = branch.folders.get(part);
      }
      return branch;
    };
    for (const category of state.categories) getFolder(String(category).split('/'));
    for (const page of state.pages) {
      const parts = page.path.split('/');
      parts.pop();
      getFolder(parts).pages.push(page);
    }
    const appendBranch = (branch, container, level) => {
      const items = [...[...branch.folders.values()].sort((a, b) => a.name.localeCompare(b.name)), ...branch.pages.sort((a, b) => (a.title || a.path).localeCompare(b.title || b.path))];
      let shown = 0;
      let more;
      const appendBatch = () => {
        if (more) more.remove();
        const end = Math.min(shown + 100, items.length);
        const fragment = document.createDocumentFragment();
        for (; shown < end; shown++) {
          const item = items[shown];
          const li = node('li');
          if (item.folders) {
            const button = node('button', 'folder-toggle');
            button.type = 'button';
            const childList = node('ul');
            const expanded = state.expanded.has(item.path) || state.selectedPath.startsWith(`${item.path}/`);
            button.setAttribute('aria-expanded', String(expanded));
            button.dataset.folder = item.path;
            button.append(icon(arrowPath), node('span', '', item.name));
            childList.hidden = !expanded;
            let rendered = false;
            if (expanded) { appendBranch(item, childList, level + 1); rendered = true; }
            button.addEventListener('click', () => {
              const opening = button.getAttribute('aria-expanded') !== 'true';
              button.setAttribute('aria-expanded', String(opening));
              childList.hidden = !opening;
              if (opening) {
                state.expanded.add(item.path);
                if (!rendered) { appendBranch(item, childList, level + 1); rendered = true; }
              } else state.expanded.delete(item.path);
            });
            li.append(button, childList);
          } else li.append(selectablePage(item));
          fragment.append(li);
        }
        container.append(fragment);
        if (shown < items.length) {
          more = node('li');
          const button = node('button', 'text-button more-button', `Show more (${items.length - shown})`);
          button.type = 'button';
          button.addEventListener('click', appendBatch);
          more.append(button);
          container.append(more);
        }
      };
      appendBatch();
    };
    const list = node('ul');
    appendBranch(root, list, 0);
    directory.append(list);
  }

  async function performSearch(query) {
    state.searchRequest?.abort();
    state.search = query.trim();
    if (!state.search || !enabled()) { renderDirectory(); $('retrieval-label').textContent = ''; return; }
    const controller = new AbortController();
    state.searchRequest = controller;
    const scope = state.scope;
    $('directory').setAttribute('aria-busy', 'true');
    $('library-count').textContent = 'Searching…';
    try {
      const data = await api(apiUrl('/api/search', { query: state.search }), { signal: controller.signal });
      if (controller.signal.aborted || state.scope !== scope) return;
      const results = data.results || [];
      const fragment = document.createDocumentFragment();
      for (const result of results) {
        const row = selectablePage(result);
        const anchor = row.matches('a') ? row : row.querySelector('a');
        anchor.classList.add('search-result');
        anchor.replaceChildren(node('strong', '', result.title || result.path), node('small', '', result.snippet || result.path));
        fragment.append(row);
      }
      if (!results.length) fragment.append(node('p', 'directory-message', 'No matching documents. Try a different phrase or an exact name.'));
      $('directory').replaceChildren(fragment);
      $('directory').setAttribute('aria-busy', 'false');
      $('library-count').textContent = `${results.length} search result${results.length === 1 ? '' : 's'}`;
      $('retrieval-label').textContent = data.retrieval === 'hybrid' || data.retrieval === 'hybrid_rrf' ? 'Hybrid search' : 'Search results';
      announce(`${results.length} documents found.`);
    } catch (error) {
      if (error.name === 'AbortError') return;
      $('directory').setAttribute('aria-busy', 'false');
      $('library-count').textContent = 'Search could not complete';
      showError(error);
    }
  }

  function safeExternalUrl(value) {
    try {
      const url = new URL(value);
      return ['https:', 'http:', 'mailto:'].includes(url.protocol) ? url.href : null;
    } catch { return null; }
  }

  function documentReference(value) {
    return typeof value === 'string' ? value : value.path || value.target || value.reference || '';
  }

  function renderReferences(target, items, kind) {
    target.replaceChildren();
    if (!items?.length) { target.append(node('p', '', kind === 'sources' ? 'No source recorded' : 'None yet')); return; }
    for (const item of items) {
      const reference = typeof item === 'string' ? item : item.path || item.url || item.uri || item.reference || item.target || '';
      const label = typeof item === 'string' ? item : item.title || item.label || item.name || reference;
      if (kind !== 'sources') {
        const link = pageLink({ path: documentReference(item), title: label });
        link.className = '';
        target.append(link);
      } else {
        const url = safeExternalUrl(reference);
        if (url) {
          const link = node('a', '', label);
          link.href = url;
          link.target = '_blank';
          link.rel = 'noopener noreferrer';
          target.append(link);
        } else target.append(node('span', '', label || 'Source'));
      }
    }
  }

  function renderMarkdown(content) {
    const markdown = $('markdown');
    const raw = plainContent(content);
    if (window.marked?.parse && window.DOMPurify?.sanitize) {
      const html = window.marked.parse(raw, { gfm: true, breaks: false, async: false });
      markdown.innerHTML = window.DOMPurify.sanitize(html, {
        USE_PROFILES: { html: true },
        FORBID_TAGS: ['img', 'style', 'form', 'input', 'button', 'video', 'audio', 'iframe', 'object', 'embed'],
        FORBID_ATTR: ['style', 'id'],
      });
      const usedIds = new Set();
      for (const heading of markdown.querySelectorAll('h1,h2,h3,h4,h5,h6')) {
        const base = heading.textContent.toLowerCase().trim().replace(/[^\p{L}\p{N}\s_-]/gu, '').replace(/\s+/g, '-') || 'section';
        let id = base;
        let number = 1;
        while (usedIds.has(id)) id = `${base}-${number++}`;
        usedIds.add(id);
        heading.id = id;
      }
      for (const anchor of markdown.querySelectorAll('a')) {
        const href = anchor.getAttribute('href') || '';
        if (/^https?:\/\//i.test(href) || /^mailto:/i.test(href)) {
          anchor.rel = 'noopener noreferrer';
          anchor.target = '_blank';
        } else if (href.startsWith('#')) {
          anchor.addEventListener('click', (event) => {
            event.preventDefault();
            let id;
            try { id = decodeURIComponent(href.slice(1)); } catch { return; }
            const heading = [...markdown.querySelectorAll('[id]')].find((item) => item.id === id);
            heading?.scrollIntoView({ block: 'start' });
          });
        } else if (!/^[a-z][a-z\d+.-]*:/i.test(href) && !href.startsWith('//')) {
          try {
            const basePath = state.selectedPath.split('/').map(encodeURIComponent).join('/');
            const url = new URL(href, `https://knowledge.invalid/${basePath}`);
            const path = decodeURIComponent(url.pathname.replace(/^\//, ''));
            if (/\.(md|txt)$/i.test(path)) {
              anchor.href = route(path);
              anchor.addEventListener('click', (event) => {
                if (event.button || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
                event.preventDefault();
                openPage(path, { anchor: url.hash.slice(1) });
              });
            } else anchor.removeAttribute('href');
          } catch { anchor.removeAttribute('href'); }
        } else anchor.removeAttribute('href');
      }
    } else {
      const pre = node('pre', '', raw);
      markdown.replaceChildren(pre);
    }
  }

  async function openPage(path, options = {}) {
    if (!enabled()) return;
    const version = ++state.pageVersion;
    state.currentPage = null;
    state.selectedPath = path;
    if (!options.preserveView) setView('document');
    setEmpty('Opening document…', path);
    updateControls();
    clearError();
    try {
      const page = await api(apiUrl('/api/page', { path }));
      if (version !== state.pageVersion || !enabled()) return;
      state.currentPage = page;
      $('document-path').textContent = page.path || path;
      $('document-title').textContent = page.title || path.split('/').pop();
      renderMarkdown(page.content);
      renderReferences($('sources'), page.sources, 'sources');
      renderReferences($('related'), page.related, 'related');
      renderReferences($('backlinks'), page.backlinks, 'backlinks');
      $('empty-state').hidden = true;
      $('document').hidden = false;
      $('document').setAttribute('aria-busy', 'false');
      for (const link of $('directory').querySelectorAll('[data-path]')) {
        if (link.dataset.path === path) link.setAttribute('aria-current', 'page'); else link.removeAttribute('aria-current');
      }
      updateRoute(path, options.replace);
      updateControls();
      if (!state.search) renderDirectory();
      announce(`Opened ${page.title || path}.`);
      if (options.anchor) {
        let id;
        try { id = decodeURIComponent(options.anchor); } catch { return; }
        [...$('markdown').querySelectorAll('[id]')].find((item) => item.id === id)?.scrollIntoView({ block: 'start' });
      }
    } catch (error) {
      if (version !== state.pageVersion) return;
      setEmpty('Document unavailable', 'It may have moved, changed, or been deleted. Refresh the library and choose a document.');
      showError(error);
    }
  }

  function setView(view) {
    state.view = view;
    const graph = view === 'graph';
    $('document-tab').setAttribute('aria-selected', String(!graph));
    $('graph-tab').setAttribute('aria-selected', String(graph));
    $('document-tab').tabIndex = graph ? -1 : 0;
    $('graph-tab').tabIndex = graph ? 0 : -1;
    $('document-panel').hidden = graph;
    $('graph-panel').hidden = !graph;
    if (graph && enabled()) {
      if (!state.graph) loadGraph(); else { resizeGraph(); drawGraph(); }
    }
  }

  async function reload(options = {}) {
    const version = ++state.loadVersion;
    const pageVersion = ++state.pageVersion;
    state.graphVersion++;
    state.searchRequest?.abort();
    state.graph = null;
    state.currentPage = null;
    clearError();
    $('directory').setAttribute('aria-busy', 'true');
    $('directory').replaceChildren(...Array.from({ length: 4 }, () => node('div', 'loading-line')));
    $('library-count').textContent = 'Loading library…';
    $('refresh').disabled = true;
    const path = options.page !== undefined ? options.page : state.selectedPath;
    try {
      const status = await api('/api/state');
      if (version !== state.loadVersion) return;
      state.status = status;
      updateControls();
      if (enabled()) {
        const data = await api(apiUrl('/api/pages'));
        if (version !== state.loadVersion) return;
        state.pages = data.pages || [];
        state.categories = (data.categories || []).map((item) => typeof item === 'string' ? item : item.path);
        const validPaths = new Set(state.pages.map((item) => item.path));
        for (const selected of state.selected) if (!validPaths.has(selected)) state.selected.delete(selected);
        if (state.search) await performSearch(state.search); else renderDirectory();
        if (version !== state.loadVersion) return;
        updateControls();
        if (pageVersion !== state.pageVersion) return;
        if (path) await openPage(path, { replace: true, preserveView: true });
        else setEmpty(state.pages.length ? 'Your connected knowledge' : 'A place for lasting knowledge', state.pages.length ? 'Choose a document from the library, or explore the graph to discover related ideas.' : 'Create a document or import Markdown and text files. Your agents can also capture useful, sourced knowledge here.');
        if (version !== state.loadVersion) return;
        if (state.view === 'graph') setView('graph');
      } else {
        state.pages = [];
        state.categories = [];
        state.graph = null;
        state.selected.clear();
        renderDirectory();
        clearGraph();
        setEmpty('Knowledge is off', state.scope === 'global' && state.status.project?.enabled === false ? 'Enable project knowledge to access your project and global libraries.' : 'Your files are preserved. Enable knowledge to make this library available again.');
      }
      announce(enabled() ? `${state.pages.length} documents in the ${state.scope} library.` : 'Knowledge is disabled.');
    } catch (error) {
      if (version !== state.loadVersion) return;
      $('directory').setAttribute('aria-busy', 'false');
      $('directory').replaceChildren(node('p', 'directory-message', 'Could not load the library. Refresh to try again.'));
      $('library-count').textContent = 'Library unavailable';
      setEmpty('Could not connect', 'Make sure HA is still running, then refresh the console.');
      showError(error);
    } finally {
      if (version === state.loadVersion) $('refresh').disabled = false;
    }
  }

  let dialogSequence = 0;
  function createDialog({ title, description, fields = [], submit = 'Save', destructive = false, onSubmit }) {
    const dialogId = ++dialogSequence;
    const dialogScope = state.scope;
    const dialog = node('dialog', 'dialog');
    const form = node('form');
    const heading = node('h2', '', title);
    heading.id = `dialog-title-${dialogId}`;
    dialog.setAttribute('aria-labelledby', heading.id);
    form.append(heading);
    if (description) form.append(node('p', 'dialog-description', description));
    const inputs = {};
    for (const field of fields) {
      const container = node('div', 'dialog-field');
      const id = `dialog-${dialogId}-${field.name}`;
      const label = node('label', '', field.label);
      label.htmlFor = id;
      const input = node(field.type === 'textarea' ? 'textarea' : field.type === 'select' ? 'select' : 'input');
      if (field.type === 'select') {
        for (const choice of field.options || []) {
          const item = typeof choice === 'string' ? { value: choice, label: choice || 'Library root' } : choice;
          const option = node('option', '', item.label);
          option.value = item.value;
          input.append(option);
        }
      } else if (field.type !== 'textarea') input.type = field.type || 'text';
      input.id = id;
      input.name = field.name;
      input.required = !!field.required;
      if (field.type === 'file') {
        input.multiple = true;
        input.accept = '.md,.txt,text/plain,text/markdown';
        if (field.files?.length) {
          const transfer = new DataTransfer();
          for (const file of field.files) transfer.items.add(file);
          input.files = transfer.files;
        }
      }
      else if (field.value !== undefined) input.value = field.value;
      if (field.placeholder) input.placeholder = field.placeholder;
      if (field.maxLength) input.maxLength = field.maxLength;
      if (field.help) {
        const help = node('p', 'field-help', field.help);
        help.id = `${id}-help`;
        input.setAttribute('aria-describedby', help.id);
        container.append(label, input, help);
      } else container.append(label, input);
      if (field.type === 'file') {
        const preview = node('div', 'import-preview');
        preview.setAttribute('aria-live', 'polite');
        const updatePreview = () => {
          const files = [...input.files];
          preview.replaceChildren();
          preview.hidden = !files.length;
          if (!files.length) return;
          const size = files.reduce((total, file) => total + file.size, 0);
          preview.append(node('p', 'small', `${files.length} file${files.length === 1 ? '' : 's'} selected · ${Math.ceil(size / 1024).toLocaleString()} KB`));
          const list = node('ul');
          for (const file of files.slice(0, 30)) list.append(node('li', '', file.name));
          preview.append(list);
          if (files.length > 30) preview.append(node('p', 'small', `…and ${files.length - 30} more files`));
        };
        input.addEventListener('change', updatePreview);
        updatePreview();
        container.append(preview);
      }
      inputs[field.name] = input;
      form.append(container);
    }
    const error = node('div', 'dialog-error');
    error.setAttribute('role', 'alert');
    error.hidden = true;
    const actions = node('div', 'dialog-actions');
    const cancel = node('button', 'text-button', 'Cancel');
    cancel.type = 'button';
    const save = node('button', `primary-button${destructive ? ' destructive-submit' : ''}`, submit);
    save.type = 'submit';
    actions.append(cancel, save);
    form.append(error, actions);
    dialog.append(form);
    document.body.append(dialog);
    const previouslyFocused = document.activeElement;
    let saving = false;
    const close = () => { if (!saving) { dialog.close(); dialog.remove(); } };
    cancel.addEventListener('click', close);
    dialog.addEventListener('cancel', (event) => { if (saving) event.preventDefault(); });
    dialog.addEventListener('click', (event) => { if (event.target === dialog) { const bounds = dialog.getBoundingClientRect(); if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) close(); } });
    dialog.addEventListener('close', () => { dialog.remove(); if (!document.querySelector('dialog[open]')) previouslyFocused?.focus(); });
    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      if (saving || !form.reportValidity()) return;
      saving = true;
      save.disabled = true;
      cancel.disabled = true;
      save.textContent = 'Working…';
      error.hidden = true;
      try {
        if (dialogScope !== state.scope) throw new Error('The library changed. Close this dialog and try again.');
        const values = Object.fromEntries(Object.entries(inputs).map(([name, input]) => [name, input.type === 'file' ? [...input.files] : input.value]));
        await onSubmit(values, inputs, dialogScope);
        saving = false;
        dialog.close();
      } catch (failure) {
        error.textContent = failure.message || String(failure);
        error.hidden = false;
        saving = false;
        save.disabled = false;
        cancel.disabled = false;
        save.textContent = submit;
      }
    });
    dialog.showModal();
    (Object.values(inputs)[0] || cancel).focus();
    return { dialog, form, inputs, actions, close, scope: dialogScope };
  }

  function categories() {
    const result = new Set(['', ...state.categories]);
    for (const page of state.pages) {
      const parts = page.path.split('/');
      parts.pop();
      while (parts.length) { result.add(parts.join('/')); parts.pop(); }
    }
    return [...result].sort((a, b) => a.localeCompare(b));
  }

  async function hashes(paths, scope = state.scope) {
    if (scope !== state.scope) throw new Error('The library changed. Close this dialog and try again.');
    const result = {};
    const missing = [];
    for (const path of paths) {
      const page = state.pages.find((item) => item.path === path);
      if (page?.hash) result[path] = page.hash;
      else if (state.currentPage?.path === path && state.currentPage.hash) result[path] = state.currentPage.hash;
      else missing.push(path);
    }
    for (let offset = 0; offset < missing.length; offset += 4) {
      const batch = await Promise.all(missing.slice(offset, offset + 4).map(async (path) => [path, (await api(apiUrl('/api/page', { path, scope }))).hash]));
      for (const [path, hash] of batch) if (hash) result[path] = hash;
    }
    return result;
  }

  async function manage(payload, page, scope = state.scope) {
    if (scope !== state.scope) throw new Error('The library changed. Close this dialog and try again.');
    if (!enabled()) throw new Error('Enable knowledge before changing this library.');
    state.busy = true;
    updateControls();
    try {
      const result = await api('/api/manage', { method: 'POST', body: JSON.stringify({ scope, ...payload }) });
      if (scope !== state.scope) return result;
      state.selected.clear();
      state.search = '';
      $('search').value = '';
      state.selectedPath = page || '';
      await reload({ page: page || '' });
      const changed = result.changed?.length;
      const skipped = result.skipped?.length;
      announce(`${changed === undefined ? 'Library updated' : `${changed} document${changed === 1 ? '' : 's'} updated`}${skipped ? `, ${skipped} skipped` : ''}.`);
      return result;
    } finally { state.busy = false; updateControls(); }
  }

  function newDocument() {
    createDialog({
      title: 'New document', description: `Create a Markdown document in your ${state.scope} library.`, submit: 'Create document',
      fields: [
        { name: 'category', type: 'select', label: 'Folder', options: categories() },
        { name: 'filename', label: 'File name', required: true, placeholder: 'useful-topic.md', help: 'A Markdown file name. Use folders to organize related topics.', maxLength: 200 },
        { name: 'content', type: 'textarea', label: 'Document', value: '# Untitled\n\n' },
      ],
      onSubmit: async ({ category, filename, content }, _inputs, scope) => {
        const name = filename.trim();
        if (!name) throw new Error('Enter a file name.');
        if (/[\\/]/.test(name) || name === '.' || name === '..') throw new Error('Enter a file name without folder separators.');
        const path = [category, /\.md$/i.test(name) ? name : `${name}.md`].filter(Boolean).join('/');
        await manage({ action: 'create_document', path, content }, path, scope);
      },
    });
  }

  function editDocument() {
    const page = state.currentPage;
    if (!page) return;
    createDialog({ title: 'Edit document', description: page.path, submit: 'Save changes', fields: [{ name: 'content', type: 'textarea', label: 'Markdown', value: plainContent(page.content) }], onSubmit: async ({ content }, _inputs, scope) => {
      await manage({ action: 'update_document', path: page.path, content, expected_hash: page.hash }, page.path, scope);
    } });
  }

  function deleteDocuments(paths) {
    if (!paths.length) return;
    createDialog({
      title: paths.length === 1 ? 'Delete document?' : `Delete ${paths.length} documents?`,
      description: `${paths.slice(0, 8).join('\n')}${paths.length > 8 ? `\n…and ${paths.length - 8} more` : ''}\n\nThis removes the selected files from this library. Incoming links from other documents prevent deletion.`,
      submit: 'Delete', destructive: true,
      fields: [{ name: 'confirmation', label: 'Type delete to confirm', required: true }],
      onSubmit: async ({ confirmation }, _inputs, scope) => {
        if (confirmation !== 'delete') throw new Error('Type delete to confirm.');
        await manage({ action: 'delete_documents', paths, expected_hashes: await hashes(paths, scope) }, '', scope);
      },
    });
  }

  function moveDocuments(paths) {
    if (!paths.length) return;
    createDialog({
      title: paths.length === 1 ? 'Move document' : `Move ${paths.length} documents`,
      description: 'Choose the destination folder. Links between knowledge documents are updated with the move.', submit: 'Move',
      fields: [{ name: 'target_category', type: 'select', label: 'Destination folder', options: categories() }],
      onSubmit: async ({ target_category }, _inputs, scope) => {
        const next = paths.length === 1 ? [target_category, paths[0].split('/').pop()].filter(Boolean).join('/') : '';
        await manage({ action: 'move_documents', paths, target_category, expected_hashes: await hashes(paths, scope) }, next, scope);
      },
    });
  }

  function manageCategories() {
    const dirs = categories().filter(Boolean);
    const categoryDialog = createDialog({
      title: 'Manage folders', description: 'Organize documents into topic folders.', submit: 'Create folder',
      fields: [{ name: 'path', label: 'New folder path', placeholder: 'research/databases', required: true, help: 'Use / to create nested folders.' }],
      onSubmit: async ({ path }, _inputs, scope) => { await manage({ action: 'create_category', path: path.trim() }, '', scope); },
    });
    if (!dirs.length) return;
    const section = node('div', 'dialog-field');
    const label = node('label', '', 'Existing folder');
    label.htmlFor = 'existing-category';
    const select = node('select');
    select.id = 'existing-category';
    for (const path of dirs) { const option = node('option', '', path); option.value = path; select.append(option); }
    const actions = node('div', 'dialog-category-actions');
    const rename = node('button', 'text-button', 'Rename folder');
    const remove = node('button', 'text-button danger-button', 'Delete folder');
    rename.type = remove.type = 'button';
    const folderScopeValid = () => {
      if (categoryDialog.scope === state.scope) return true;
      categoryDialog.close();
      showError(new Error('The library changed. Open Folders again to manage this library.'));
      return false;
    };
    rename.addEventListener('click', () => {
      if (!folderScopeValid()) return;
      const path = select.value;
      categoryDialog.close();
      createDialog({ title: 'Rename folder', description: `Current folder: ${path}\nDocuments and links inside this folder are updated together.`, submit: 'Rename', fields: [{ name: 'new_path', label: 'New folder path', value: path, required: true }], onSubmit: async ({ new_path }, _inputs, scope) => {
        await manage({ action: 'rename_category', path, new_path: new_path.trim(), expected_hashes: await hashes(state.pages.filter((page) => page.path.startsWith(`${path}/`)).map((page) => page.path), scope) }, '', scope);
      } });
    });
    remove.addEventListener('click', () => {
      if (!folderScopeValid()) return;
      const path = select.value;
      const paths = state.pages.filter((page) => page.path.startsWith(`${path}/`)).map((page) => page.path);
      categoryDialog.close();
      createDialog({ title: 'Delete folder?', description: `${path}\n\nThis removes the folder and ${paths.length} document${paths.length === 1 ? '' : 's'} inside it. Incoming links from outside the folder prevent deletion.`, submit: 'Delete folder', destructive: true, fields: [{ name: 'confirmation', label: `Type ${path} to confirm`, required: true }], onSubmit: async ({ confirmation }, _inputs, scope) => {
        if (confirmation !== path) throw new Error('Enter the exact folder path to confirm.');
        await manage({ action: 'delete_category', path, confirm: true, expected_hashes: await hashes(paths, scope) }, '', scope);
      } });
    });
    actions.append(rename, remove);
    section.append(label, select, actions);
    categoryDialog.form.insertBefore(section, categoryDialog.form.querySelector('.dialog-field'));
  }

  function importDocuments(files = []) {
    if (!enabled() || state.busy) return;
    createDialog({
      title: 'Import documents', description: 'Import Markdown and UTF-8 text files. Files stay in your local knowledge library.', submit: 'Import',
      fields: [
        { name: 'category', type: 'select', label: 'Destination folder', options: categories() },
        { name: 'files', type: 'file', label: 'Markdown or text files', required: true, files, help: 'Choose or drop one or more .md or .txt files. Maximum 256 KB per file, 16 MB total.' },
        { name: 'on_conflict', type: 'select', label: 'When a file already exists', options: [{ value: 'skip', label: 'Skip the existing file' }, { value: 'rename', label: 'Import with a new name' }, { value: 'overwrite', label: 'Replace the existing file' }] },
      ],
      onSubmit: async ({ category, files, on_conflict }, _inputs, scope) => {
        if (!files.length) throw new Error('Choose at least one file.');
        if (files.length > 2048) throw new Error('Choose no more than 2,048 files.');
        if (files.some((file) => !/\.(md|txt)$/i.test(file.name))) throw new Error('Only .md and .txt files can be imported.');
        if (files.some((file) => file.size > 256 * 1024)) throw new Error('Each file must be 256 KB or smaller.');
        if (files.reduce((total, file) => total + file.size, 0) > 16 * 1024 * 1024) throw new Error('The selected files exceed 16 MB. Import a smaller batch.');
        const decoder = new TextDecoder('utf-8', { fatal: true });
        const documents = [];
        for (const file of files) {
          let content;
          try { content = decoder.decode(await file.arrayBuffer()); } catch { throw new Error(`${file.name} is not UTF-8 text. Save it as UTF-8 and try again.`); }
          documents.push({ name: file.name, content });
        }
        await manage({ action: 'import_documents', category, files: documents, on_conflict, ...(on_conflict === 'overwrite' ? { expected_hashes: await hashes(state.pages.map((page) => page.path), scope) } : {}) }, '', scope);
      },
    });
  }

  function clearGraph() {
    state.graph = null;
    $('graph-documents').replaceChildren();
    $('graph-count').textContent = '(0)';
    $('graph-selection').hidden = true;
    $('graph-empty').hidden = false;
    const canvas = $('graph');
    const context = canvas.getContext('2d');
    context.setTransform(1, 0, 0, 1, 0, 0);
    context.clearRect(0, 0, canvas.width, canvas.height);
  }

  async function loadGraph() {
    const version = ++state.graphVersion;
    if (!enabled()) { clearGraph(); return; }
    $('graph-empty').textContent = 'Loading connections…';
    $('graph-empty').hidden = false;
    try {
      const data = await api(apiUrl('/api/graph'));
      if (version !== state.graphVersion || !enabled()) return;
      const nodes = (data.nodes || []).slice(0, 2048).map((item, index, all) => {
        const angle = index * Math.PI * (3 - Math.sqrt(5));
        const radius = all.length === 1 ? 0 : 38 * Math.sqrt(index + 1);
        return { ...item, x: Math.cos(angle) * radius, y: Math.sin(angle) * radius, radius: 6 };
      });
      const byPath = new Map(nodes.map((item) => [item.path, item]));
      const edges = (data.edges || []).slice(0, 12000).map((edge) => ({ source: byPath.get(edge.source), target: byPath.get(edge.target) })).filter((edge) => edge.source && edge.target);
      const totalEdges = data.total_edges ?? (data.edges || []).length;
      const truncated = !!data.truncated || totalEdges > edges.length;
      state.graph = { nodes, edges, byPath, scale: 1, x: 0, y: 0, width: 0, height: 0, selected: byPath.get(state.selectedPath) || null, hovered: null };
      $('graph-empty').textContent = 'No documents yet. Create or import a document to start your graph.';
      $('graph-empty').hidden = nodes.length > 0;
      $('graph-count').textContent = `(${nodes.length})`;
      $('link-count').textContent = String(totalEdges);
      $('graph-help').textContent = `Drag the canvas to pan or a node to move it. Use the controls to zoom. Every document is also available in the list below.${truncated ? ` The graph draws ${edges.length.toLocaleString()} of ${Number(totalEdges).toLocaleString()} connections to keep navigation responsive.` : ''}`;
      const list = $('graph-documents');
      list.replaceChildren();
      let rendered = 0;
      let more;
      const appendBatch = () => {
        if (more) more.remove();
        const fragment = document.createDocumentFragment();
        const end = Math.min(rendered + 100, nodes.length);
        for (; rendered < end; rendered++) fragment.append(pageLink(nodes[rendered], true));
        list.append(fragment);
        if (rendered < nodes.length) {
          more = node('button', 'text-button more-button', `Show more (${nodes.length - rendered})`);
          more.type = 'button';
          more.addEventListener('click', appendBatch);
          list.append(more);
        }
      };
      appendBatch();
      resizeGraph();
      fitGraph();
      updateGraphSelection();
    } catch (error) { if (version === state.graphVersion) { clearGraph(); $('graph-empty').textContent = 'Could not load the graph. Refresh to try again.'; showError(error); } }
  }

  let graphFrame = null;
  function drawGraph() {
    if (graphFrame !== null) return;
    graphFrame = requestAnimationFrame(() => {
      graphFrame = null;
      const graph = state.graph;
      if (!graph || state.view !== 'graph') return;
      const canvas = $('graph');
      const context = canvas.getContext('2d');
      const ratio = graph.ratio || 1;
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
      context.clearRect(0, 0, graph.width, graph.height);
      context.translate(graph.width / 2 + graph.x, graph.height / 2 + graph.y);
      context.scale(graph.scale, graph.scale);
      const colors = getComputedStyle(document.documentElement);
      const primary = colors.getPropertyValue('--primary').trim();
      const muted = colors.getPropertyValue('--muted').trim();
      const border = colors.getPropertyValue('--border').trim();
      const accent = colors.getPropertyValue('--accent').trim();
      const surface = colors.getPropertyValue('--surface').trim();
      const text = colors.getPropertyValue('--text').trim();
      context.lineWidth = 1 / graph.scale;
      context.strokeStyle = border;
      context.beginPath();
      for (const edge of graph.edges) { context.moveTo(edge.source.x, edge.source.y); context.lineTo(edge.target.x, edge.target.y); }
      context.stroke();
      if (graph.selected) {
        context.strokeStyle = primary;
        context.lineWidth = 1.6 / graph.scale;
        context.beginPath();
        for (const edge of graph.edges) if (edge.source === graph.selected || edge.target === graph.selected) { context.moveTo(edge.source.x, edge.source.y); context.lineTo(edge.target.x, edge.target.y); }
        context.stroke();
      }
      const connected = new Set();
      for (const edge of graph.edges) if (edge.source === graph.selected) connected.add(edge.target); else if (edge.target === graph.selected) connected.add(edge.source);
      for (const item of graph.nodes) {
        const sx = item.x * graph.scale + graph.width / 2 + graph.x;
        const sy = item.y * graph.scale + graph.height / 2 + graph.y;
        if (sx < -160 || sx > graph.width + 160 || sy < -40 || sy > graph.height + 40) continue;
        const selected = item === graph.selected || item === graph.hovered;
        context.beginPath();
        context.arc(item.x, item.y, Math.max(item.radius, 3.5 / graph.scale) + (selected ? 2 / graph.scale : 0), 0, Math.PI * 2);
        context.fillStyle = selected ? primary : connected.has(item) ? accent : muted;
        context.fill();
        context.strokeStyle = surface;
        context.lineWidth = 2 / graph.scale;
        context.stroke();
        if (selected || graph.nodes.length < 80 || graph.scale > 2.2) {
          const label = item.title || item.path.split('/').pop();
          context.font = `${12 / graph.scale}px system-ui, sans-serif`;
          context.textAlign = 'center';
          const short = label.length > 36 ? `${label.slice(0, 35)}…` : label;
          const width = context.measureText(short).width;
          context.fillStyle = surface;
          context.fillRect(item.x - width / 2 - 3 / graph.scale, item.y + 10 / graph.scale, width + 6 / graph.scale, 17 / graph.scale);
          context.fillStyle = text;
          context.fillText(short, item.x, item.y + 22 / graph.scale);
        }
      }
      $('zoom-label').textContent = `${Math.round(graph.scale * 100)}%`;
    });
  }

  function resizeGraph() {
    if (!state.graph || $('graph-panel').hidden) return;
    const canvas = $('graph');
    const bounds = canvas.getBoundingClientRect();
    const ratio = Math.min(window.devicePixelRatio || 1, 3);
    state.graph.width = bounds.width;
    state.graph.height = bounds.height;
    canvas.width = Math.max(1, Math.round(bounds.width * ratio));
    canvas.height = Math.max(1, Math.round(bounds.height * ratio));
    state.graph.ratio = ratio;
    drawGraph();
  }

  function fitGraph() {
    const graph = state.graph;
    if (!graph) return;
    const xs = graph.nodes.map((item) => item.x);
    const ys = graph.nodes.map((item) => item.y);
    const minX = Math.min(0, ...xs), maxX = Math.max(0, ...xs), minY = Math.min(0, ...ys), maxY = Math.max(0, ...ys);
    graph.scale = Math.max(.1, Math.min(1.6, (graph.width - 100) / Math.max(maxX - minX, 80), (graph.height - 100) / Math.max(maxY - minY, 80)));
    graph.x = -(minX + maxX) / 2 * graph.scale;
    graph.y = -(minY + maxY) / 2 * graph.scale;
    drawGraph();
  }

  function zoomGraph(factor, x, y) {
    const graph = state.graph;
    if (!graph) return;
    const next = Math.max(.08, Math.min(5, graph.scale * factor));
    const cx = x === undefined ? 0 : x - graph.width / 2;
    const cy = y === undefined ? 0 : y - graph.height / 2;
    graph.x = cx - (cx - graph.x) * next / graph.scale;
    graph.y = cy - (cy - graph.y) * next / graph.scale;
    graph.scale = next;
    drawGraph();
  }

  function graphPosition(event) {
    const bounds = $('graph').getBoundingClientRect();
    return { x: event.clientX - bounds.left, y: event.clientY - bounds.top };
  }

  function hitNode(position) {
    const graph = state.graph;
    if (!graph) return null;
    let result = null;
    let distance = 18;
    for (const item of graph.nodes) {
      const delta = Math.hypot(item.x * graph.scale + graph.width / 2 + graph.x - position.x, item.y * graph.scale + graph.height / 2 + graph.y - position.y);
      if (delta < distance) { result = item; distance = delta; }
    }
    return result;
  }

  function updateGraphSelection() {
    const selected = state.graph?.selected;
    $('graph-selection').hidden = !selected;
    if (selected) {
      $('graph-selection-title').textContent = selected.title || selected.path;
      $('graph-selection-path').textContent = selected.path;
      announce(`Selected ${selected.title || selected.path}. Use Open document to read it.`);
    }
    drawGraph();
  }

  let pointer = null;
  $('graph').addEventListener('pointerdown', (event) => {
    if (event.button !== 0 || !state.graph) return;
    const position = graphPosition(event);
    pointer = { id: event.pointerId, ...position, startX: position.x, startY: position.y, node: hitNode(position), moved: false };
    $('graph').setPointerCapture(event.pointerId);
    $('graph').focus({ preventScroll: true });
  });
  $('graph').addEventListener('pointermove', (event) => {
    const graph = state.graph;
    if (!graph) return;
    const position = graphPosition(event);
    if (pointer?.id === event.pointerId) {
      const dx = position.x - pointer.x, dy = position.y - pointer.y;
      if (Math.hypot(position.x - pointer.startX, position.y - pointer.startY) > 4) pointer.moved = true;
      if (pointer.moved) {
        if (pointer.node) { pointer.node.x += dx / graph.scale; pointer.node.y += dy / graph.scale; }
        else { graph.x += dx; graph.y += dy; }
      }
      pointer.x = position.x; pointer.y = position.y;
    } else graph.hovered = hitNode(position);
    drawGraph();
  });
  $('graph').addEventListener('pointerup', (event) => {
    if (pointer?.id !== event.pointerId) return;
    if (!pointer.moved && state.graph) { state.graph.selected = pointer.node; updateGraphSelection(); }
    pointer = null;
    $('graph').releasePointerCapture(event.pointerId);
  });
  $('graph').addEventListener('pointercancel', () => { pointer = null; });
  $('graph').addEventListener('lostpointercapture', () => { pointer = null; });
  $('graph').addEventListener('pointerleave', () => { if (state.graph && !pointer) { state.graph.hovered = null; drawGraph(); } });
  $('graph').addEventListener('dblclick', (event) => { const item = hitNode(graphPosition(event)); if (item) openPage(item.path); });
  $('graph').addEventListener('wheel', (event) => {
    if (!state.graph || event.ctrlKey || event.metaKey) return;
    event.preventDefault();
    const position = graphPosition(event);
    zoomGraph(Math.exp(-Math.max(-100, Math.min(100, event.deltaY)) * .002), position.x, position.y);
  }, { passive: false });
  $('graph').addEventListener('keydown', (event) => {
    const graph = state.graph;
    if (!graph || event.ctrlKey || event.metaKey || event.altKey) return;
    let handled = true;
    if (event.key === 'ArrowLeft') graph.x += 35;
    else if (event.key === 'ArrowRight') graph.x -= 35;
    else if (event.key === 'ArrowUp') graph.y += 35;
    else if (event.key === 'ArrowDown') graph.y -= 35;
    else if (event.key === '+' || event.key === '=') zoomGraph(1.2);
    else if (event.key === '-') zoomGraph(1 / 1.2);
    else if (event.key === '0' || event.key === 'Home') fitGraph();
    else if (event.key === 'Enter' && graph.selected) openPage(graph.selected.path);
    else if (event.key === 'Escape') { graph.selected = null; updateGraphSelection(); }
    else handled = false;
    if (handled) { event.preventDefault(); drawGraph(); }
  });

  $('graph-open').addEventListener('click', () => { if (state.graph?.selected) openPage(state.graph.selected.path); });
  $('zoom-in').addEventListener('click', () => zoomGraph(1.25));
  $('zoom-out').addEventListener('click', () => zoomGraph(.8));
  $('graph-fit').addEventListener('click', fitGraph);
  new ResizeObserver(resizeGraph).observe($('graph'));
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', drawGraph);

  $('scope').value = state.scope;
  $('scope').addEventListener('change', () => {
    state.scope = $('scope').value;
    state.selectedPath = '';
    state.search = '';
    state.selected.clear();
    state.expanded.clear();
    state.selecting = false;
    $('search').value = '';
    updateRoute('');
    reload({ page: '' });
  });
  $('refresh').addEventListener('click', () => reload());
  $('enabled').addEventListener('click', async () => {
    if (!state.status || state.busy) return;
    const active = state.status[state.scope]?.enabled !== false;
    state.busy = true;
    updateControls();
    try {
      state.status = await api('/api/control', { method: 'POST', body: JSON.stringify({ scope: state.scope, enabled: !active }) });
      await reload();
    } catch (error) { showError(error); }
    finally { state.busy = false; updateControls(); }
  });
  $('search-form').addEventListener('submit', (event) => { event.preventDefault(); clearTimeout(state.searchTimer); performSearch($('search').value); });
  $('search').addEventListener('input', () => { clearTimeout(state.searchTimer); state.searchTimer = setTimeout(() => performSearch($('search').value), 280); });
  $('document-tab').addEventListener('click', () => setView('document'));
  $('graph-tab').addEventListener('click', () => setView('graph'));
  document.querySelector('.tabs').addEventListener('keydown', (event) => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const next = event.key === 'Home' ? 'document' : event.key === 'End' ? 'graph' : state.view === 'graph' ? 'document' : 'graph';
    setView(next);
    $(`${next}-tab`).focus();
  });
  $('directory').addEventListener('keydown', (event) => {
    if (event.ctrlKey || event.altKey || event.metaKey || event.target.matches('input')) return;
    const items = [...$('directory').querySelectorAll('.page-link,.folder-toggle,.more-button')].filter((item) => item.getClientRects().length);
    const index = items.indexOf(document.activeElement);
    if (index < 0) return;
    let target;
    if (event.key === 'ArrowDown') target = items[Math.min(items.length - 1, index + 1)];
    else if (event.key === 'ArrowUp') target = items[Math.max(0, index - 1)];
    else if (event.key === 'Home') target = items[0];
    else if (event.key === 'End') target = items.at(-1);
    else if (event.key === 'ArrowRight' && event.target.matches('.folder-toggle[aria-expanded=false]')) event.target.click();
    else if (event.key === 'ArrowLeft' && event.target.matches('.folder-toggle[aria-expanded=true]')) event.target.click();
    else return;
    event.preventDefault();
    target?.focus();
  });
  $('show-connections').addEventListener('click', () => {
    setView('graph');
    if (state.graph) { state.graph.selected = state.graph.byPath.get(state.selectedPath); updateGraphSelection(); }
  });
  $('copy-link').addEventListener('click', async () => {
    try { await navigator.clipboard.writeText(new URL(route(state.selectedPath), location.origin).href); announce('Document link copied.'); $('copy-link').textContent = 'Copied'; setTimeout(() => { $('copy-link').textContent = 'Copy link'; }, 1600); }
    catch { showError(new Error('Could not copy the link. You can copy this page’s address from the address bar.')); }
  });
  $('new-document').addEventListener('click', newDocument);
  $('edit-document').addEventListener('click', editDocument);
  $('delete-document').addEventListener('click', () => { if (state.currentPage) deleteDocuments([state.currentPage.path]); });
  $('move-document').addEventListener('click', () => { if (state.currentPage) moveDocuments([state.currentPage.path]); });
  $('import-documents').addEventListener('click', () => importDocuments());
  $('manage-categories').addEventListener('click', manageCategories);
  $('select-documents').addEventListener('click', () => { state.selecting = !state.selecting; if (!state.selecting) state.selected.clear(); updateSelection(); if (state.search) performSearch(state.search); else renderDirectory(); });
  $('move-selected').addEventListener('click', () => moveDocuments([...state.selected]));
  $('delete-selected').addEventListener('click', () => deleteDocuments([...state.selected]));
  let fileDragDepth = 0;
  const fileDrag = (event) => [...(event.dataTransfer?.types || [])].includes('Files');
  const importFileInput = () => document.querySelector('dialog[open] input[type=file]');
  const canDropFiles = () => enabled() && !state.busy && (!document.querySelector('dialog[open]') || importFileInput());
  const resetFileDrag = () => { fileDragDepth = 0; $('import-drop-zone').hidden = true; };
  document.addEventListener('dragenter', (event) => {
    if (!fileDrag(event)) return;
    event.preventDefault();
    fileDragDepth++;
    if (canDropFiles() && !importFileInput()) $('import-drop-zone').hidden = false;
  });
  document.addEventListener('dragover', (event) => {
    if (!fileDrag(event)) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = canDropFiles() ? 'copy' : 'none';
  });
  document.addEventListener('dragleave', (event) => {
    if (!fileDrag(event)) return;
    fileDragDepth = Math.max(0, fileDragDepth - 1);
    if (!fileDragDepth) resetFileDrag();
  });
  document.addEventListener('drop', (event) => {
    if (!fileDrag(event)) return;
    event.preventDefault();
    resetFileDrag();
    if (!canDropFiles()) { announce('Close the current dialog and enable knowledge before importing files.'); return; }
    const files = [...event.dataTransfer.files];
    if (!files.length) return;
    const input = importFileInput();
    if (input) {
      const transfer = new DataTransfer();
      for (const file of files) transfer.items.add(file);
      input.files = transfer.files;
      input.dispatchEvent(new Event('change', { bubbles: true }));
    } else importDocuments(files);
  });
  document.addEventListener('dragend', resetFileDrag);
  document.addEventListener('keydown', (event) => { if (event.key === '/' && !event.ctrlKey && !event.metaKey && !event.altKey && !document.querySelector('dialog[open]') && !event.target.closest('input,textarea,select,[contenteditable]')) { event.preventDefault(); $('search').focus(); } });
  window.addEventListener('popstate', () => {
    const url = new URL(location.href);
    const scope = url.searchParams.get('scope') === 'global' ? 'global' : 'project';
    const page = url.searchParams.get('page') || '';
    if (scope !== state.scope) { state.scope = scope; state.selectedPath = page; state.search = ''; $('search').value = ''; state.selected.clear(); state.expanded.clear(); state.selecting = false; reload({ page }); }
    else if (page) openPage(page, { replace: true });
    else { state.loadVersion++; state.pageVersion++; state.selectedPath = ''; state.currentPage = null; setView('document'); setEmpty('Your connected knowledge', 'Choose a document from the library, or explore the graph to discover related ideas.'); renderDirectory(); updateControls(); $('refresh').disabled = false; document.title = 'Knowledge · HA'; }
  });

  state.selectedPath = new URL(location.href).searchParams.get('page') || '';
  updateControls();
  reload({ page: state.selectedPath });
})();
