const searchDialog=document.getElementById('docs-search');
const menuDialog=document.getElementById('docs-menu');
const queryInput=document.getElementById('docs-query');
const results=document.getElementById('docs-search-results');
const count=document.getElementById('search-count');
let searchIndex;
let indexPromise;
let searchRevision=0;
const normalize=text=>text.normalize('NFKD').replace(/\p{Diacritic}/gu,'').toLowerCase();
function loadIndex(){
  if(!indexPromise)indexPromise=fetch('/docs/search-index.json').then(response=>{if(!response.ok)throw new Error('Search index unavailable');return response.json();}).then(records=>records.map(record=>({...record,haystack:normalize(record.title+' '+record.section+' '+record.content)})));
  return indexPromise;
}
function highlight(parent,text,query){
  const at=text.toLowerCase().indexOf(query.toLowerCase());
  if(!query||at<0){parent.textContent=text;return;}
  parent.append(document.createTextNode(text.slice(0,at)));const mark=document.createElement('mark');mark.textContent=text.slice(at,at+query.length);parent.append(mark,document.createTextNode(text.slice(at+query.length)));
}
function searchScore(record,query,words){
  if(!words.every(word=>record.haystack.includes(word)))return 0;
  const title=normalize(record.title),section=normalize(record.section);
  return 1+(title===query?40:0)+(section===query?30:0)+(title.includes(query)?16:0)+(section.includes(query)?12:0)+(record.haystack.includes(query)?6:0)+words.reduce((score,word)=>score+(title.includes(word)?4:0)+(section.includes(word)?3:0),0);
}
async function renderSearch(){
  const revision=++searchRevision,query=queryInput.value.trim(),normalized=normalize(query),words=normalized.split(/\s+/).filter(Boolean);
  try{
    searchIndex=await loadIndex();if(revision!==searchRevision)return;
    const matches=query?searchIndex.map(record=>({record,score:searchScore(record,normalized,words)})).filter(item=>item.score>0).sort((a,b)=>b.score-a.score||a.record.title.localeCompare(b.record.title)).slice(0,8).map(item=>item.record):searchIndex.filter(record=>record.section==='Overview'&&['Quickstart','Introduction','The .lm language'].includes(record.title));
    results.replaceChildren();
    for(const record of matches){
      const anchor=document.createElement('a');anchor.className='search-result';anchor.href=record.href;
      const category=document.createElement('small');category.textContent=record.category+' / '+record.title;
      const title=document.createElement('strong');highlight(title,record.section==='Overview'?record.title:record.section,query);
      const preview=document.createElement('p'),at=record.content.toLowerCase().indexOf(words[0]||''),start=Math.max(0,at-48),excerpt=(start?'…':'')+record.content.slice(start,start+175)+(record.content.length>start+175?'…':'');highlight(preview,excerpt,query);
      anchor.append(category,title,preview);results.append(anchor);
    }
    count.textContent=query?(matches.length?`${matches.length} ${matches.length===1?'result':'results'} for “${query}”`:`No results for “${query}”. Try a command or a shorter phrase.`):'Start with a guide, or search for a command or concept.';
  }catch{count.textContent='Search is unavailable. Browse the documentation navigation.';}
}
function openSearch(){
  if(menuDialog.open)menuDialog.close();
  if(!searchDialog.open)searchDialog.showModal();queryInput.focus();queryInput.select();renderSearch();
}
searchDialog.addEventListener('keydown',event=>{if(event.key==='Escape'){event.preventDefault();searchDialog.close();}});
document.querySelectorAll('[data-open-search]').forEach(button=>button.addEventListener('click',openSearch));
document.querySelector('[data-close-search]').addEventListener('click',()=>searchDialog.close());
queryInput.addEventListener('input',renderSearch);
queryInput.addEventListener('keydown',event=>{if(event.key==='ArrowDown'){event.preventDefault();results.querySelector('a')?.focus();}});
results.addEventListener('keydown',event=>{
  const links=[...results.querySelectorAll('a')],index=links.indexOf(document.activeElement);
  if(event.key==='ArrowDown'){event.preventDefault();links[Math.min(index+1,links.length-1)]?.focus();}
  if(event.key==='ArrowUp'){event.preventDefault();if(index<=0)queryInput.focus();else links[index-1].focus();}
});
document.addEventListener('keydown',event=>{
  const editing=event.target instanceof Element&&(event.target.matches('input,textarea,select')||event.target.isContentEditable);
  if((event.key.toLowerCase()==='k'&&(event.metaKey||event.ctrlKey))||(event.key==='/'&&!editing&&!event.metaKey&&!event.ctrlKey)){event.preventDefault();openSearch();}
});
document.querySelector('[data-open-menu]').addEventListener('click',()=>menuDialog.showModal());
document.querySelector('[data-close-menu]').addEventListener('click',()=>menuDialog.close());
for(const dialog of [searchDialog,menuDialog])dialog.addEventListener('click',event=>{const rect=dialog.getBoundingClientRect();if(event.target===dialog&&(event.clientX<rect.left||event.clientX>rect.right||event.clientY<rect.top||event.clientY>rect.bottom))dialog.close();});
window.matchMedia('(min-width: 851px)').addEventListener('change',event=>{if(event.matches&&menuDialog.open)menuDialog.close();});
const copyStatus=document.getElementById('docs-copy-status');let copyTimer;
document.querySelectorAll('[data-copy-target]').forEach(button=>button.addEventListener('click',async()=>{
  const code=document.getElementById(button.dataset.copyTarget);
  try{await navigator.clipboard.writeText(code.textContent);copyStatus.textContent='Example copied.';}
  catch{const selection=window.getSelection(),range=document.createRange();range.selectNodeContents(code);selection.removeAllRanges();selection.addRange(range);copyStatus.textContent='Select and copy this example.';}
  clearTimeout(copyTimer);copyTimer=setTimeout(()=>{copyStatus.textContent='';},3000);
}));
const outlineLinks=[...document.querySelectorAll('.docs-outline a[href^="#"]')];
const headings=[...document.querySelectorAll('.docs-prose h2[id],.docs-prose h3[id]')];
let outlineFrame;
function updateOutline(){
  outlineFrame=0;let active=headings[0];for(const heading of headings){if(heading.getBoundingClientRect().top<=140)active=heading;}
  for(const link of outlineLinks){if(active&&decodeURIComponent(link.hash.slice(1))===active.id)link.setAttribute('aria-current','location');else link.removeAttribute('aria-current');}
}
window.addEventListener('scroll',()=>{if(!outlineFrame)outlineFrame=requestAnimationFrame(updateOutline);},{passive:true});updateOutline();
