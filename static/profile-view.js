/* Shared profile card for collection and read-only display. All archive text stays inert. */
window.SearchcordProfile = class {
  constructor(host, {writable=false, onMessages=()=>{}, onServer=()=>{}}={}) {
    this.host=host; this.writable=writable; this.onMessages=onMessages; this.onServer=onServer;
    this.version=0; this.serverVersion=0;
  }
  node(tag, cls='', text='') {
    const node=document.createElement(tag); node.className=cls; node.textContent=text; return node;
  }
  async json(url, options={}) {
    const r=await fetch(url,{...options,cache:'no-store'}); const data=await r.json();
    if(!r.ok) throw new Error(typeof data.detail==='string'?data.detail:'Profile could not be loaded.');
    return data;
  }
  close() { this.version++;this.serverVersion++;this.controller?.abort();this.serverController?.abort();clearTimeout(this.timer);this.host.hidden=true; }
  async open(uid) {
    const version=++this.version; this.serverVersion++;this.serverController?.abort();clearTimeout(this.timer);
    this.controller?.abort();this.controller=new AbortController();this.uid=uid;
    this.host.hidden=false;this.host.replaceChildren(this.node('p','profile-status','Loading profile…'));
    try {
      const data=await this.json(`/api/profiles/${encodeURIComponent(uid)}`,{signal:this.controller.signal});
      if(version!==this.version)return;
      this.render(data);await this.servers();
    } catch(error) {
      if(version!==this.version||error.name==='AbortError')return;
      this.host.replaceChildren(this.node('p','profile-status',error.message));
    }
  }
  render(data) {
    this.data=data;this.host.replaceChildren();
    const card=this.node('article','profile-card');card.setAttribute('aria-label',`Profile of ${data.display_name}`);
    const identity=this.node('div','profile-identity');
    const banner=this.node('div','profile-banner');
    if(data.banner_url){const image=this.node('img');image.alt='';image.src=data.banner_url;image.referrerPolicy='no-referrer';image.onerror=()=>image.remove();banner.append(image);}
    const portrait=this.node('div','profile-portrait',data.display_name.slice(0,1).toUpperCase());
    if(data.avatar_url){const image=this.node('img');image.alt=`${data.display_name}'s avatar`;image.src=data.avatar_url;image.referrerPolicy='no-referrer';image.onload=()=>portrait.replaceChildren(image);}
    const copy=this.node('div','profile-copy');
    copy.append(this.node('h2','profile-name',data.display_name));
    copy.append(this.node('p','profile-handle',`@${data.username}${data.pronouns?' · '+data.pronouns:''}`));
    const badges=this.node('div','profile-badges');
    if(data.bot)badges.append(this.node('span','','APP'));
    for(const badge of data.badges){const label=badge.description||badge.id;if(label)badges.append(this.node('span','',label));}
    if(badges.childElementCount)copy.append(badges);
    const actions=this.node('div','profile-actions');
    const messages=this.node('button','','Search messages');messages.type='button';messages.addEventListener('click',()=>this.onMessages(this.uid));actions.append(messages);
    if(this.writable){
      const collect=this.node('button','profile-fetch',data.extended?'Refresh profile':'Scrape extended profile');collect.type='button';
      collect.addEventListener('click',async()=>{
        collect.disabled=true;status.textContent='Fetching profile…';const version=this.version;
        try{await this.json(`/api/profiles/${encodeURIComponent(this.uid)}/fetch`,{method:'POST'});if(version===this.version)await this.open(this.uid);}
        catch(error){if(version===this.version){status.textContent=error.message;collect.disabled=false;}}
      });actions.append(collect);
    }
    copy.append(actions);
    const status=this.node('p','profile-status',!data.extended?(data.scraped?'Basic profile saved. Extended details have not been scraped.':'Profile has not been scraped. Showing archived identity.'):'');
    status.setAttribute('role','status');copy.append(status);
    if(data.bio)copy.append(this.node('p','profile-bio',data.bio));
    const dates=this.node('dl','profile-facts');
    for(const [label,value] of [['Member since',data.created_at],['Profile saved',data.fetched_at],['Subscriber since',data.premium_since]]){
      if(!value)continue;const parsed=new Date(value);if(Number.isNaN(parsed.valueOf()))continue;
      dates.append(this.node('dt','',label),this.node('dd','',parsed.toLocaleDateString(undefined,{dateStyle:'medium'})));
    }
    dates.append(this.node('dt','','User ID'),this.node('dd','',data.id),this.node('dt','','Archived messages'),this.node('dd','',new Intl.NumberFormat().format(data.message_count)));
    copy.append(dates);
    if(data.connections.length){
      const connections=this.node('section','profile-connections');connections.append(this.node('h3','','Connections'));
      for(const connection of data.connections){const row=this.node('div','profile-connection');row.append(this.node('span','connection-type',connection.type||'Account'),this.node('strong','',connection.name||connection.id||''));if(connection.verified)row.append(this.node('span','','Verified'));connections.append(row);}copy.append(connections);
    }
    identity.append(banner,portrait,copy);
    const history=this.node('section','profile-history');history.append(this.node('h3','','Seen in servers'),this.node('p','profile-order','Most messages first'));
    const label=this.node('label','profile-server-search','Search servers');this.serverInput=this.node('input');this.serverInput.type='search';this.serverInput.placeholder='Server name';label.append(this.serverInput);history.append(label);
    this.serverInput.addEventListener('input',()=>{clearTimeout(this.timer);this.serverVersion++;this.serverController?.abort();this.timer=setTimeout(()=>this.servers(),180);});
    this.serverList=this.node('div','profile-server-list');this.serverList.setAttribute('role','list');this.serverList.setAttribute('aria-label','Servers seen in archive');history.append(this.serverList);
    this.serverStatus=this.node('p','profile-status');this.serverStatus.setAttribute('role','status');history.append(this.serverStatus);
    this.more=this.node('button','profile-more','More servers');this.more.type='button';this.more.hidden=true;this.more.addEventListener('click',()=>this.servers(true));history.append(this.more);
    card.append(identity,history);this.host.append(card);
  }
  async servers(more=false) {
    const version=++this.serverVersion;const profileVersion=this.version;
    this.serverController?.abort();this.serverController=new AbortController();
    if(!more){this.offset=0;this.serverList.replaceChildren();}
    this.more.disabled=true;this.serverStatus.textContent='Loading servers…';
    try {
      const query=new URLSearchParams({q:this.serverInput.value,offset:this.offset,limit:30});
      const data=await this.json(`/api/profiles/${encodeURIComponent(this.uid)}/servers?${query}`,{signal:this.serverController.signal});
      if(version!==this.serverVersion||profileVersion!==this.version)return;
      for(const server of data.servers){
        const row=this.node('div','profile-server');row.setAttribute('role','listitem');
        const button=this.node('button','');button.type='button';button.append(this.node('span','server-monogram',server.name.slice(0,1)),this.node('strong','',server.name),this.node('span','server-count',`${new Intl.NumberFormat().format(server.messages)} messages`));
        button.addEventListener('click',()=>this.onServer(this.uid,server.id));row.append(button);this.serverList.append(row);
      }
      this.offset+=data.servers.length;this.more.hidden=!data.has_more;
      this.serverStatus.textContent=this.offset?'':'No archived servers match.';
    } catch(error) {if(version===this.serverVersion&&profileVersion===this.version&&error.name!=='AbortError')this.serverStatus.textContent=error.message;}
    finally {if(version===this.serverVersion&&profileVersion===this.version)this.more.disabled=false;}
  }
};
