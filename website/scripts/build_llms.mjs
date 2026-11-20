// The documentation as agents read it: each page's Markdown beside its HTML, an index at /llms.txt
// (https://llmstxt.org) and every page in reading order at /llms-full.txt. The Markdown is the
// canonical source with what only made sense inside the repository resolved: links become absolute
// URLs, and the comments the HTML build and the docs tests read become plain text or go.
import {mkdirSync,writeFileSync} from 'node:fs';
import path from 'node:path';

const SUMMARY='Lemma gives a coding agent what an IDE gives a person, without an editor open. It finds code through language servers (TypeScript, Python, Go and Rust) and catches what a change breaks in the code that depends on it, across services and repositories, fast enough to ask on every step of a task.';
const DETAILS=[
  'Agents use Lemma through MCP tools (`lemma mcp`) or the `lemma` command line. Each repository describes its services, operations and links in a `.lm` module, `mod.lm`; `lemma check --change <repository>` reports what a change breaks in the others.',
  'Every link below is a page\'s Markdown; the same pages, as HTML, are at https://lemma.systems/docs/.',
];

const FENCE=/^\s*(`{3,}|~{3,})/;
const INLINE_CODE=/(`+)[\s\S]*?\1/g;
const MARKDOWN_LINK=/(\]\()([^)\s]+)((?:\s+"[^"]*")?\))/g;
const COMMENT_LINE=/^\s*<!--\s*([\w-]+):?\s*([\s\S]*?)\s*-->\s*$/;

// Splits Markdown into prose and fenced code, so rewriting never touches an example.
function splitFences(markdown) {
  const parts=[];let prose=[],code=null,fence='';
  for(const line of markdown.split('\n')){
    const opening=line.match(FENCE);
    if(code){code.push(line);if(opening&&opening[1][0]===fence[0]&&opening[1].length>=fence.length&&!line.trim().slice(opening[1].length).trim()){parts.push({isCode:true,lines:code});code=null;}continue;}
    if(opening){parts.push({isCode:false,lines:prose});prose=[];code=[line];fence=opening[1];continue;}
    prose.push(line);
  }
  parts.push(code?{isCode:true,lines:code}:{isCode:false,lines:prose});
  return parts;
}

function rewriteLinks(line,resolve) {
  let result='',offset=0;
  for(const span of line.matchAll(INLINE_CODE)){
    result+=line.slice(offset,span.index).replace(MARKDOWN_LINK,(_,open,href,close)=>open+resolve(href)+close)+span[0];
    offset=span.index+span[0].length;
  }
  return result+line.slice(offset).replace(MARKDOWN_LINK,(_,open,href,close)=>open+resolve(href)+close);
}

// The comments a page carries for the HTML build and the docs tests, as an agent should read them: a
// code block's file, as a line before it. The rest go: a diagram's words are in the prose around it,
// and a tutorial's `after` repeats the prerequisite its text states.
function commentLine(line) {
  const comment=line.match(COMMENT_LINE);
  if(!comment)return line;
  const [,kind,value]=comment;
  if(kind==='file')return `File \`${value}\`:`;
  if(kind==='append')return `Append to \`${value}\`:`;
  return null;
}

/**
 * One page's Markdown for agents. `resolve` turns a link written in the page into an absolute URL.
 */
export function agentMarkdown(markdown,resolve) {
  const lines=[];let isAfterDropped=false;
  for(const part of splitFences(markdown)){
    if(part.isCode){lines.push(...part.lines);isAfterDropped=false;continue;}
    for(const line of part.lines){
      const readable=commentLine(line);
      if(readable===null){isAfterDropped=true;continue;}
      // A dropped comment between blank lines would leave two; examples keep theirs as written.
      const isRepeatedBlank=isAfterDropped&&!readable.trim()&&(!lines.length||!lines.at(-1).trim());
      isAfterDropped=false;
      if(!isRepeatedBlank)lines.push(rewriteLinks(readable,resolve));
    }
  }
  return lines.join('\n').trim()+'\n';
}

function indexEntry(page,siteUrl) {
  const badge=page.badge?` (${page.badge.toLowerCase()})`:'';
  return `- [${page.title}](${siteUrl}${page.markdownHref})${badge}: ${page.description}`;
}

/**
 * The /llms.txt index: the summary, then the pages of each section in reading order with their
 * descriptions, and the pages an agent can skip under "Optional".
 */
export function llmsIndex(pages,siteUrl) {
  const blocks=[`# Lemma\n\n> ${SUMMARY}\n\n${DETAILS.join('\n\n')}\n\nAll of it in one file: ${siteUrl}/llms-full.txt`];
  const sectionTitles=[...new Set(pages.map(page=>page.section))];
  const optional=pages.filter(page=>page.isOptional);
  for(const title of sectionTitles){
    const required=pages.filter(page=>page.section===title&&!page.isOptional);
    if(required.length)blocks.push(`## ${title}\n\n${required.map(page=>indexEntry(page,siteUrl)).join('\n')}`);
  }
  if(optional.length)blocks.push(`## Optional\n\n${optional.map(page=>indexEntry(page,siteUrl)).join('\n')}`);
  return blocks.join('\n\n')+'\n';
}

/**
 * Writes each page's Markdown beside its HTML, and /llms.txt and /llms-full.txt at the site's root.
 * `documents` holds each page with its Markdown for agents, in reading order.
 */
export function writeLlmsFiles({output,documents,siteUrl}) {
  for(const {page,markdown} of documents){
    const file=path.join(output,page.markdownHref);
    mkdirSync(path.dirname(file),{recursive:true});writeFileSync(file,markdown);
  }
  writeFileSync(path.join(output,'llms.txt'),llmsIndex(documents.map(({page})=>page),siteUrl));
  const full=documents.map(({page,markdown})=>`Source: ${siteUrl}${page.href}\n\n${markdown}`);
  writeFileSync(path.join(output,'llms-full.txt'),`# Lemma documentation\n\n> ${SUMMARY}\n\nEvery page of ${siteUrl}/docs/, in reading order. The index is ${siteUrl}/llms.txt.\n\n${full.join('\n---\n\n')}`);
}
