/* Optional progressive enhancement for static Lemma component markup.
   Load with defer, after tokens.css and components.css. */
(() => {
  document.querySelectorAll('[data-lm-tabs]').forEach(group => {
    const tabs = [...group.querySelectorAll('[role="tab"]')];
    function activate(tab, focus) {
      tabs.forEach(item => {
        const selected = item === tab;
        item.setAttribute('aria-selected', String(selected));
        item.tabIndex = selected ? 0 : -1;
        const panel = document.getElementById(item.getAttribute('aria-controls'));
        if (panel) panel.hidden = !selected;
      });
      if (focus) tab.focus();
    }
    tabs.forEach((tab, index) => {
      tab.addEventListener('click', () => activate(tab, false));
      tab.addEventListener('keydown', event => {
        let next;
        if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
        if (event.key === 'ArrowLeft') next = (index - 1 + tabs.length) % tabs.length;
        if (event.key === 'Home') next = 0;
        if (event.key === 'End') next = tabs.length - 1;
        if (next === undefined) return;
        event.preventDefault();
        activate(tabs[next], true);
      });
    });
  });
  document.querySelectorAll('[data-lm-copy]').forEach(button => {
    const statusId = button.getAttribute('aria-controls');
    let status = statusId ? document.getElementById(statusId) : null;
    if (!status) {
      status = document.createElement('span');
      status.className = 'lm-sr-only';
      status.setAttribute('role', 'status');
      button.after(status);
    }
    button.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(button.dataset.lmCopy);
        status.textContent = `Copied ${button.dataset.lmCopy}`;
      } catch {
        status.textContent = `Copy this value: ${button.dataset.lmCopy}`;
      }
    });
  });
})();
