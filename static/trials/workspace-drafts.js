'use strict';
// Interaction sketches use only synthetic, in-page state. No API requests.
const status = document.querySelector('#draftStatus');
const say = text => { if (status) status.textContent = text; };
document.querySelector('#draftSearch')?.addEventListener('input', event => {
  const query = event.target.value.toLowerCase();
  let count = 0;
  document.querySelectorAll('[data-filter-row]').forEach(row => {
    row.hidden = !row.textContent.toLowerCase().includes(query);
    if (!row.hidden) count++;
  });
  say(`${count} conversations`);
});
document.querySelectorAll('[data-queue]').forEach(button => button.addEventListener('click', () => {
  const selected = button.getAttribute('aria-pressed') !== 'true';
  button.setAttribute('aria-pressed', String(selected));
  button.textContent = selected ? 'queued' : 'queue';
  const count = document.querySelectorAll('[data-queue][aria-pressed="true"]').length;
  const counter = document.querySelector('#queueTotal');
  if (counter) counter.textContent = count;
  say(`${count} channels in this draft queue`);
}));
document.querySelector('#pauseDraft')?.addEventListener('click', event => {
  const paused = event.target.getAttribute('aria-pressed') !== 'true';
  event.target.setAttribute('aria-pressed', String(paused));
  event.target.textContent = paused ? 'resume feed' : 'pause feed';
  document.querySelector('#feedState').textContent = paused ? 'paused' : 'live';
  say(paused ? 'Draft feed paused' : 'Draft feed resumed');
});
document.querySelector('#draftRange')?.addEventListener('change', event => {
  const values = event.target.value === 'week' ? [42,66,38,81,57,92,70] : [60,44,80,58,95,69,85];
  document.querySelectorAll('.draft-chart > div').forEach((bar,i) => {
    bar.style.setProperty('--bar', `${values[i]}%`);
    bar.querySelector('span').textContent = event.target.value === 'week' ? ['mon','tue','wed','thu','fri','sat','sun'][i] : String(i * 4 + 1);
  });
  say(event.target.value === 'week' ? 'Showing the sample week' : 'Showing the sample month');
});
document.querySelector('#draftSettings')?.addEventListener('submit', event => {
  event.preventDefault();
  document.querySelector('#draftToken').value = '';
  say('Draft only. No credential was stored or sent.');
});
document.querySelector('#draftReveal')?.addEventListener('click', event => {
  const input = document.querySelector('#draftToken');
  const show = input.type === 'password';
  input.type = show ? 'text' : 'password';
  event.target.textContent = show ? 'hide' : 'show';
});
document.querySelectorAll('input[name="assistant"]').forEach(input => input.addEventListener('change', () => {
  document.querySelector('#draftPreview').textContent = JSON.stringify({messages:[{role:'system',content:`You are ${input.value}. Respond in your natural style.`},{role:'user',content:'Ready to review the layout?'},{role:'assistant',content:'Yes. Let’s start with the search controls.'}]}, null, 2);
  say(`Preview updated for ${input.value}`);
}));
document.querySelector('#draftExport')?.addEventListener('click', () => say('Export preview ready. This draft does not write a file.'));
document.querySelector('#draftStop')?.addEventListener('click', event => {
  event.target.disabled = true;
  document.querySelector('#progressState').textContent = 'Stopped safely';
  say('Draft stopped. The saved cursor is ready for the next run.');
});
