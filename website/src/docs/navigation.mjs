// Each page's `description` is one plain sentence saying what the page is for. It is the page's meta
// description and its line in /llms.txt, where an agent reads it to decide whether to open the page.
// `isOptional` lists a page under "Optional" in /llms.txt: background an agent can skip for a task.
export const sections = [
  {title:'Start here', pages:[
    {slug:'', title:'Introduction', source:'docs/introduction.md', description:'What Lemma is: an IDE for coding agents that finds code through language servers and catches what a change breaks across services and repositories.'},
    {slug:'getting-started', title:'Getting started', source:'docs/getting-started.md', description:'Install Lemma, index a repository, look up references and connect an agent over MCP.'},
    {slug:'quickstart', title:'Quickstart', source:'docs/quickstart.md', description:'Describe two repositories, break one from the other, and catch it with lemma check in about ten minutes.'},
    {slug:'onboarding', title:'Onboarding', source:'docs/onboarding.md', description:'Roll Lemma out to a team: describe services, generated clients and calling code, calibrate on past changes, and check every pull request in CI.'},
  ]},
  {title:'Tutorials', pages:[
    {slug:'tutorials', title:'Tutorial overview', source:'docs/tutorials/README.md', isUnlisted:true, description:'How the tutorials fit together: each starts from the quickstart\'s end state, a billing and a web repository.'},
    {slug:'tutorials/1-links', title:'1. Declare a link', source:'docs/tutorials/1-links.md', description:'Declare which operation calls which, for a call inference cannot see, such as over a queue, and compare both sides\' types field by field.'},
    {slug:'tutorials/2-requirements', title:'2. Write a requirement', source:'docs/tutorials/2-requirements.md', description:'Write your own requirement: a rule over the facts Lemma knows about a system, reported wherever it matches.'},
    {slug:'tutorials/3-allowances', title:'3. Allow a finding', source:'docs/tutorials/3-allowances.md', description:'Accept a known finding for now, with a reason and an expiry date, without hiding it.'},
    {slug:'tutorials/4-behaviors', title:'4. Test a behavior', source:'docs/tutorials/4-behaviors.md', badge:'Beta', description:'Check what code does, not what its schemas say, by running it: a behavior, a checker that produces evidence, and the cases it runs.'},
    {slug:'tutorials/5-features', title:'5. Add a feature', source:'docs/tutorials/5-features.md', description:'Confirm a new feature is specified, served and called before it ships, from the findings about what a change adds.'},
    {slug:'tutorials/6-editors', title:'6. Set up your editor', source:'docs/tutorials/6-editors.md', description:'Get highlighting, diagnostics, hover, go-to-definition, completion and formatting for .lm modules in your editor.'},
  ]},
  {title:'Check a system', pages:[
    {slug:'check', title:'Check links', source:'docs/check.md', description:'How lemma check works: mod.lm descriptions, workspaces, operations declared by hand or read from OpenAPI, links, and the findings it reports.'},
    {slug:'verify', title:'Verify a description', source:'docs/verify.md', description:'Check that a .lm description still matches the code with lemma verify.'},
    {slug:'source-contracts', title:'Verify an implementation', source:'docs/source-contracts.md', badge:'Experimental', description:'Check a selected TypeScript, Python or Go function body against a contract.'},
    {slug:'grammar', title:'Requirements and rules', source:'docs/grammar.md', description:'Write requirements and rules in .lm over the relations Lemma fills with facts from documents and code.'},
    {slug:'adapters', title:'Read contracts from code', source:'docs/adapters.md', description:'Read a service\'s operations from its code instead of a committed OpenAPI document: built-in adapters, generators and your own scripts.'},
  ]},
  {title:'Work with agents', pages:[
    {slug:'agent-skills', title:'Agent skills', source:'skills/README.md', description:'The skills that teach coding agents when and how to use Lemma\'s tools.'},
    {slug:'lemma-mode', title:'Lemma mode', source:'docs/lemma-mode.md', description:'The workflow an agent follows to make a change other code depends on and show it is safe, and the stop gate that enforces the check.'},
    {slug:'providers', title:'Language servers', source:'docs/providers.md', description:'The language servers Lemma uses for each language, how to install them and how to configure your own.'},
    {slug:'editors', title:'Editor integrations', source:'editors/README.md', description:'Syntax highlighting, parsing and language server clients for .lm in each supported editor.'},
  ]},
  {title:'Understand Lemma', pages:[
    {slug:'concepts', title:'Concepts', source:'docs/concepts.md', description:'Snapshots, incremental indexing, stale results, caching, the daemon and sentry, and what "no references" does and does not mean.'},
    {slug:'architecture', title:'Architecture', source:'docs/architecture.md', description:'How Lemma is built: the operations every interface offers, its four layers, and how checking works.'},
    {slug:'troubleshooting', title:'Troubleshooting', source:'docs/troubleshooting.md', description:'What each common error message means and how to fix it.'},
    {slug:'pre-release', title:'Release readiness', source:'docs/pre-release.md', description:'What the current release candidate is and is not ready for, known limitations and workarounds, and the release notes.'},
    {slug:'comparison', title:'Search comparison', source:'docs/comparison.md', isOptional:true, description:'Lemma measured against grep, ripgrep, git grep and ast-grep on finding real references.'},
    {slug:'agent-benchmark', title:'Agent benchmark', source:'docs/agent-benchmark.md', isOptional:true, description:'Coding agents measured with and without Lemma on the same tasks, for answer quality, time and tokens.'},
    {slug:'performance', title:'Performance', source:'docs/performance.md', isOptional:true, description:'Where Lemma\'s time goes on each step, and what was tried to make it fast.'},
  ]},
  {title:'Reference', pages:[
    {slug:'language', title:'The .lm language', source:'docs/language.md', description:'The .lm language: services, operations, links, relations, rules, requirements, allowances and behaviors.'},
    {slug:'standard-library', title:'Standard library', source:'docs/standard-library.md', description:'Every module and signature in Lemma\'s standard library, generated from the library itself.'},
    {slug:'commands', title:'CLI commands', source:'docs/commands.md', description:'Every lemma and lm command, option and exit code, and the MCP server\'s tools.'},
    {slug:'json-output', title:'JSON output', source:'docs/json-output.md', description:'The JSON shapes commands print with --json: the output contract for scripts and agents.'},
  ]},
];
// `markdownHref` is the page's Markdown for agents, beside its HTML: /docs/check.md for /docs/check/.
export const pages = sections.flatMap(section=>section.pages.map(page=>({...page,section:section.title,href:`/docs/${page.slug ? page.slug+'/' : ''}`,markdownHref:`/docs/${page.slug || 'index'}.md`})));
