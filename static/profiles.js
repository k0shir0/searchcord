(() => {
  const dialog=document.createElement('dialog');dialog.className='profile-dialog';dialog.setAttribute('aria-label','Archived user profile');
  const close=document.createElement('button');close.type='button';close.className='profile-dialog-close';close.textContent='close';
  const host=document.createElement('div');dialog.append(close,host);document.body.append(dialog);
  const showMessages=(uid,guild)=>{
    dialog.close(); document.getElementById('fUser').value=uid;
    document.getElementById('searchInput').value='';document.getElementById('fGuild').value=guild||'';
    for(const id of ['fChannel','fDateFrom','fDateTo'])document.getElementById(id).value='';
    doSearch(1,true);
  };
  const view=new SearchcordProfile(host,{writable:true,onMessages:showMessages,onServer:showMessages});
  close.addEventListener('click',()=>dialog.close());dialog.addEventListener('close',()=>view.close());
  document.addEventListener('click',event=>{const button=event.target.closest('[data-profile-id]');if(!button)return;dialog.showModal();view.open(button.dataset.profileId);});
  const panel=document.createElement('section');panel.className='profile-backfill';
  panel.innerHTML='<h3>Archived profiles</h3><p class="profile-status">Collect missing profiles or refresh snapshots older than 24 hours using the selected Discord account. Discord may omit online and custom status.</p><button class="btn-mint" type="button" id="backfillProfiles">collect missing profiles</button><button class="btn-ghost" type="button" id="refreshStaleProfiles">refresh stale profiles</button><button class="btn-ghost" type="button" id="stopProfileBackfill" hidden>stop</button><p class="profile-status" id="profileBackfillStatus" role="status"></p>';
  document.querySelector('#view-scrape .collection-toolbar').after(panel);
  const start=panel.querySelector('#backfillProfiles'),refresh=panel.querySelector('#refreshStaleProfiles'),stop=panel.querySelector('#stopProfileBackfill'),status=panel.querySelector('#profileBackfillStatus');let timer;
  const display=data=>{start.disabled=data.running;refresh.disabled=data.running;stop.hidden=!data.running;status.textContent=data.status==='idle'?'':`${data.status} ${data.saved} saved · ${data.failed} unavailable`;clearTimeout(timer);if(data.running)timer=setTimeout(poll,1000);};
  async function poll(){try{display(await view.json('/api/profile-backfill'));}catch{status.textContent='Could not load progress; reconnecting…';clearTimeout(timer);timer=setTimeout(poll,1000);}}
  async function collect(stale){start.disabled=true;refresh.disabled=true;try{display(await view.json(`/api/profile-backfill${stale?'?refresh_stale=true':''}`,{method:'POST'}));}catch(error){start.disabled=false;refresh.disabled=false;status.textContent=error.message;}}
  start.addEventListener('click',()=>collect(false));refresh.addEventListener('click',()=>collect(true));
  stop.addEventListener('click',async()=>{try{display(await view.json('/api/profile-backfill/stop',{method:'POST'}));}catch(error){status.textContent=error.message;}});
  poll();
})();
