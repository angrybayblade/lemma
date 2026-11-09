import {highlightLm} from './highlight_lm.mjs';
import {cpSync,existsSync,mkdirSync,readFileSync,readdirSync,rmSync,writeFileSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import path from 'node:path';
import {Marked} from '../vendor/marked/marked.mjs';
import {pages,sections} from '../src/docs/navigation.mjs';

const escape=value=>String(value).replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;').replaceAll('"','&quot;').replaceAll("'",'&#39;');
const icon=name=>`<svg class="lm-icon" aria-hidden="true"><use href="/design-system/assets/symbols.svg#${name}"/></svg>`;
const plain=value=>String(value).replace(/<!--[\s\S]*?-->/g,'').replace(/```[^\n]*\n/g,'').replace(/`/g,'').replace(/\[([^\]]+)\]\([^)]+\)/g,'$1').replace(/[\n|#*>]+/g,' ').replace(/\s+/g,' ').trim();
const slug=value=>plain(value).toLowerCase().replace(/[^\p{L}\p{N}\s_-]/gu,'').trim().replace(/\s/g,'-');

function snapshotSources(root) {
  const repository=path.dirname(root),snapshot=path.join(root,'content');
  if(existsSync(path.join(repository,'Cargo.toml'))&&existsSync(path.join(repository,'docs/introduction.md'))){
    mkdirSync(snapshot,{recursive:true});
    for(const name of ['docs','skills','editors']){
      const target=path.join(snapshot,name);rmSync(target,{recursive:true,force:true});mkdirSync(target,{recursive:true});
      if(name==='docs')cpSync(path.join(repository,name),target,{recursive:true,filter:source=>!path.extname(source)||source.endsWith('.md')});
      else cpSync(path.join(repository,name,'README.md'),path.join(target,'README.md'));
    }
    const revision=execFileSync('git',['log','-1','--format=%H','--','docs','skills/README.md','editors/README.md'],{cwd:repository,encoding:'utf8'}).trim();
    writeFileSync(path.join(snapshot,'source.json'),JSON.stringify({repository:'angrybayblade/lemma',revision},null,2)+'\n');
  }
  if(!existsSync(path.join(snapshot,'docs/introduction.md')))throw new Error('Documentation sources missing. Build from the Lemma repository or include website/content.');
  return {directory:snapshot,...JSON.parse(readFileSync(path.join(snapshot,'source.json'),'utf8'))};
}

function rewriteLink(href,source,sourceInfo) {
  if(!href||href.startsWith('#')||/^(https?:|mailto:)/i.test(href))return href;
  if(/^[a-z][a-z0-9+.-]*:/i.test(href))return '#';
  const [location,fragment]=href.split('#');
  const normalized=path.posix.normalize(path.posix.join(path.posix.dirname(source==='@overview'?'docs/index.md':source),location));
  const target=pages.find(page=>page.source===normalized);
  if(target)return target.href+(fragment?'#'+fragment:'');
  return `https://github.com/${sourceInfo.repository}/blob/${sourceInfo.revision}/${normalized}${fragment?'#'+fragment:''}`;
}

function highlight(source,language) {
  if(language==='lm')return highlightLm(source);
  if(['text','console',''].includes(language))return escape(source);
  const pattern=/"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|\/\/[^\n]*|#[^\n]*|\b(?:module|workspace|repo|config|import|from|as|export|service|operations|implementation|relation|graph|rule|require|requirement|behavior|checker|check|run|expect|because|allow|let|for|in|need|return|const|async|await|function|def|class|if|else|true|false|null|strict|local|git)\b|\b\d+(?:\.\d+)?\b/g;
  let result='',offset=0;
  for(const match of source.matchAll(pattern)){
    result+=escape(source.slice(offset,match.index));
    const value=match[0],kind=/^(\/\/|#)/.test(value)?'comment':/^["']/.test(value)?'string':/^\d/.test(value)?'number':'keyword';
    result+=`<span class="syntax-${kind}">${escape(value)}</span>`;offset=match.index+value.length;
  }
  return result+escape(source.slice(offset));
}

function renderDocument(markdown,page,sourceInfo) {
  const headings=[],counts=new Map();let fileLabel='',currentSection=page.title,codeNumber=0;
  const parser=new Marked({gfm:true,breaks:false,renderer:{
    heading({depth,text,tokens}){
      if(depth===1)return '';
      const base=slug(text),count=counts.get(base)||0;counts.set(base,count+1);const id=base+(count?'-'+count:'');
      currentSection=plain(text);headings.push({id,title:currentSection,depth});
      return `<h${depth} id="${escape(id)}">${this.parser.parseInline(tokens)}<a class="heading-anchor" href="#${escape(id)}" aria-label="Link to ${escape(currentSection)}">#</a></h${depth}>\n`;
    },
    link({href,title,tokens}){return `<a href="${escape(rewriteLink(href,page.source,sourceInfo))}"${title?` title="${escape(title)}"`:''}>${this.parser.parseInline(tokens)}</a>`;},
    html({text}){
      const match=text.match(/<!--\s*(?:file|append):\s*([^\n>]+?)\s*-->/);if(match)fileLabel=match[1];
      return text.trim().startsWith('<!--')?'':escape(text);
    },
    code({text,lang}){
      const language=(lang||'').split(/\s/)[0],label=fileLabel||({lm:'Lemma',sh:'Shell',console:'Terminal',ts:'TypeScript',json:'JSON',yaml:'YAML',toml:'TOML',python:'Python',text:text.startsWith('$ ')?'Terminal':/^(lemma|lm)\b/.test(text)?'Usage':'Output'}[language]||'Example');fileLabel='';codeNumber++;
      return `<figure class="docs-code" aria-label="${escape(label)}"><figcaption><span>${escape(label)}</span><button type="button" class="copy-code" aria-label="Copy ${escape(label)} example" data-copy-target="code-${codeNumber}">${icon('copy')}</button></figcaption><pre tabindex="0" role="region" aria-label="${escape(label)} example"><code id="code-${codeNumber}" class="language-${escape(language)}">${highlight(text,language)}</code></pre></figure>\n`;
    },
    table({header,rows}){
      const cell=(item,tag)=>`<${tag}${tag==='th'?' scope="col"':''}>${this.parser.parseInline(item.tokens)}</${tag}>`;
      return `<div class="docs-table" role="region" aria-label="${escape(currentSection)} table" tabindex="0"><table><thead><tr>${header.map(item=>cell(item,'th')).join('')}</tr></thead><tbody>${rows.map(row=>`<tr>${row.map(item=>cell(item,'td')).join('')}</tr>`).join('')}</tbody></table></div>\n`;
    },
  }});
  const tokens=parser.lexer(markdown),heading=tokens.find(token=>token.type==='heading'&&token.depth===1);
  const title=heading?parser.parseInline(heading.text):escape(page.title);
  const description=page.description||plain(tokens.find(token=>token.type==='paragraph')?.text||'');
  const body=parser.parse(markdown);
  const records=[];let current={title:page.title,section:'Overview',href:page.href,content:''},headingIndex=0;
  for(const token of tokens){
    if(token.type==='heading'&&token.depth>1){records.push(current);const item=headings[headingIndex++];current={title:page.title,section:item.title,href:page.href+'#'+item.id,content:''};}
    else current.content+=' '+(token.raw||'');
  }
  records.push(current);
  return {title,description,body,headings,records:records.map(record=>({...record,content:plain(record.content).slice(0,7000),category:page.section}))};
}

function navigation(page) {
  return sections.map(section=>`<section class="nav-group"><h2>${escape(section.title)}</h2><ul>${section.pages.map(item=>{const entry=pages.find(candidate=>candidate.slug===item.slug);return `<li><a href="${entry.href}"${entry.slug===page.slug?' aria-current="page"':''}>${escape(entry.title)}</a></li>`;}).join('')}</ul></section>`).join('');
}
function outline(headings) {
  return `<ul>${headings.filter(item=>item.depth<=3).map(item=>`<li class="toc-depth-${item.depth}"><a href="#${escape(item.id)}">${escape(item.title)}</a></li>`).join('')}</ul>`;
}
function pagination(page) {
  const index=pages.findIndex(item=>item.slug===page.slug);
  return `<nav class="docs-pagination" aria-label="Page navigation">${[pages[index-1],pages[index+1]].map((item,position)=>item?`<a href="${item.href}" class="${position?'next':'previous'}"><span>${position?'Next':'Previous'}</span><strong>${escape(item.title)}</strong>${icon(position?'arrow':'arrow-diagonal')}</a>`:'<span></span>').join('')}</nav>`;
}
// Link previews need absolute URLs; crawlers do not resolve relative ones.
const SITE_URL='https://lemma.systems';
function socialMeta(title,description,href) {
  const image=`${SITE_URL}/assets/social-card.png`,alt='Lemma. Move fast. See what breaks.';
  const url=`${SITE_URL}${href}`;
  return `<link rel="canonical" href="${escape(url)}"><meta property="og:type" content="article"><meta property="og:site_name" content="Lemma"><meta property="og:url" content="${escape(url)}"><meta property="og:title" content="${title}"><meta property="og:description" content="${description}"><meta property="og:image" content="${image}"><meta property="og:image:type" content="image/png"><meta property="og:image:width" content="1200"><meta property="og:image:height" content="630"><meta property="og:image:alt" content="${alt}"><meta name="twitter:card" content="summary_large_image"><meta name="twitter:title" content="${title}"><meta name="twitter:description" content="${description}"><meta name="twitter:image" content="${image}"><meta name="twitter:image:alt" content="${alt}">`;
}
function template(page,document,sourceInfo) {
  const nav=navigation(page),toc=outline(document.headings),sourceLink=page.source==='@overview'?'':`<a href="${escape(`https://github.com/${sourceInfo.repository}/blob/${sourceInfo.revision}/${page.source}`)}">View Markdown source ↗</a>`;
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="theme-color" content="#F3F3EE"><title>${escape(page.title)} — Lemma docs</title><meta name="description" content="${escape(document.description.slice(0,200))}">${socialMeta(`${escape(page.title)} — Lemma docs`,escape(document.description.slice(0,200)),page.href)}<link rel="icon" href="/design-system/assets/mark.svg" type="image/svg+xml"><link rel="stylesheet" href="/design-system/tokens.css"><link rel="stylesheet" href="/design-system/components.css"><link rel="stylesheet" href="/docs/docs.css"><script defer src="/docs/docs.js"></script></head>
<body class="lm docs"><a class="docs-skip" href="#main">Skip to content</a>
<header class="docs-header"><div class="docs-header-inner"><a class="docs-brand" href="/" aria-label="Lemma home">${icon('mark')}<span>lemma</span></a><a class="docs-header-label" href="/docs/">Documentation</a><div class="docs-header-actions"><button type="button" class="docs-search-button" aria-label="Search docs" data-open-search>${icon('search')}<span>Search docs</span><kbd>⌘ K</kbd></button><a class="docs-github" href="https://github.com/angrybayblade/lemma">GitHub ↗</a><button class="docs-menu-button" type="button" data-open-menu aria-label="Open documentation navigation">${icon('repository')}</button></div></div></header>
<div class="docs-layout"><aside class="docs-sidebar" aria-label="Documentation navigation"><nav aria-label="Documentation">${nav}</nav></aside>
<main class="docs-main" id="main"><div class="docs-page-heading"><p class="docs-eyebrow">${escape(page.section)}</p><h1>${document.title}</h1></div><details class="docs-mobile-outline"><summary>On this page</summary><nav aria-label="Article sections">${toc}</nav></details><article class="docs-prose">${document.body}</article>${pagination(page)}<footer class="docs-page-footer"><span>Built for people and agents.</span>${sourceLink}</footer></main>
<aside class="docs-outline"><nav aria-label="On this page"><p>On this page</p>${toc}</nav><a href="/docs/troubleshooting/" class="docs-help-link">Need a hand? ↗</a></aside></div>
<dialog id="docs-search" class="docs-search-dialog" aria-labelledby="search-label"><div class="search-heading"><label id="search-label" for="docs-query">Search documentation</label><button type="button" data-close-search aria-label="Close search">${icon('close')}</button></div><div class="search-field">${icon('search')}<input id="docs-query" type="search" placeholder="Find a guide, command, or concept…" autocomplete="off" spellcheck="false" aria-controls="docs-search-results"></div><p id="search-count" role="status">Try “behavior”, “MCP”, or “lm check”.</p><div id="docs-search-results"></div><p class="search-hint"><span><kbd>↓</kbd> to results</span><span><kbd>Esc</kbd> to close</span></p></dialog>
<dialog id="docs-menu" class="docs-menu-dialog" aria-labelledby="menu-label"><div class="menu-heading"><span id="menu-label">Documentation</span><button type="button" data-close-menu aria-label="Close navigation">${icon('close')}</button></div><nav aria-label="Mobile documentation">${nav}</nav></dialog>
<p class="docs-copy-status" id="docs-copy-status" role="status"></p><noscript><style>@media(max-width:850px){.docs-sidebar{display:block;position:static;height:auto;margin-bottom:32px}.docs-menu-button,.docs-search-button{display:none}}</style></noscript></body></html>`;
}

export function buildDocs({root,output}) {
  const sourceInfo=snapshotSources(root),destination=path.join(output,'docs'),search=[];
  mkdirSync(destination,{recursive:true});
  for(const name of ['docs.css','docs.js'])cpSync(path.join(root,'src/docs',name),path.join(destination,name));
  const covered=new Set(pages.map(page=>page.source));
  function checkCoverage(directory){for(const entry of readdirSync(directory,{withFileTypes:true})){const file=path.join(directory,entry.name);if(entry.isDirectory())checkCoverage(file);else if(entry.name.endsWith('.md')){const relative=path.relative(sourceInfo.directory,file).split(path.sep).join('/');if(!covered.has(relative))throw new Error(`Document missing from navigation: ${relative}`);}}}
  checkCoverage(path.join(sourceInfo.directory,'docs'));
  for(const page of pages){
    const markdown=readFileSync(page.source==='@overview'?path.join(root,'src/docs/index.md'):path.join(sourceInfo.directory,page.source),'utf8');
    const document=renderDocument(markdown,page,sourceInfo),directory=path.join(destination,page.slug);mkdirSync(directory,{recursive:true});writeFileSync(path.join(directory,'index.html'),template(page,document,sourceInfo));search.push(...document.records);
  }
  writeFileSync(path.join(destination,'search-index.json'),JSON.stringify(search));
  writeFileSync(path.join(destination,'pages.json'),JSON.stringify(pages.map(({title,href,source})=>({title,href,source})),null,2));
  console.log(`Built ${pages.length} documentation pages and ${search.length} searchable sections.`);
}
