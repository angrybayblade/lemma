export const sections = [
  {title:'Start here', pages:[
    {slug:'', title:'Introduction', source:'docs/introduction.md'},
    {slug:'quickstart', title:'Quickstart', source:'docs/quickstart.md'},
    {slug:'onboarding', title:'Onboarding', source:'docs/onboarding.md'},
  ]},
  {title:'Tutorials', pages:[
    {slug:'tutorials', title:'Tutorial overview', source:'docs/tutorials/README.md', isUnlisted:true},
    {slug:'tutorials/1-links', title:'1. Declare a link', source:'docs/tutorials/1-links.md'},
    {slug:'tutorials/2-requirements', title:'2. Write a requirement', source:'docs/tutorials/2-requirements.md'},
    {slug:'tutorials/3-allowances', title:'3. Allow a finding', source:'docs/tutorials/3-allowances.md'},
    {slug:'tutorials/4-behaviors', title:'4. Test a behavior', source:'docs/tutorials/4-behaviors.md'},
    {slug:'tutorials/5-features', title:'5. Add a feature', source:'docs/tutorials/5-features.md'},
    {slug:'tutorials/6-editors', title:'6. Set up your editor', source:'docs/tutorials/6-editors.md'},
  ]},
  {title:'Check a system', pages:[
    {slug:'check', title:'Check links', source:'docs/check.md'},
    {slug:'verify', title:'Verify a description', source:'docs/verify.md'},
    {slug:'grammar', title:'Requirements and rules', source:'docs/grammar.md'},
    {slug:'adapters', title:'Read contracts from code', source:'docs/adapters.md'},
  ]},
  {title:'Work with agents', pages:[
    {slug:'getting-started', title:'Code navigation', source:'docs/getting-started.md'},
    {slug:'agent-skills', title:'Agent skills', source:'skills/README.md'},
    {slug:'lemma-mode', title:'Lemma mode', source:'docs/lemma-mode.md'},
    {slug:'providers', title:'Language servers', source:'docs/providers.md'},
    {slug:'editors', title:'Editor integrations', source:'editors/README.md'},
  ]},
  {title:'Understand Lemma', pages:[
    {slug:'concepts', title:'Concepts', source:'docs/concepts.md'},
    {slug:'architecture', title:'Architecture', source:'docs/architecture.md'},
    {slug:'troubleshooting', title:'Troubleshooting', source:'docs/troubleshooting.md'},
    {slug:'pre-release', title:'Release readiness', source:'docs/pre-release.md'},
    {slug:'comparison', title:'Search comparison', source:'docs/comparison.md'},
    {slug:'agent-benchmark', title:'Agent benchmark', source:'docs/agent-benchmark.md'},
    {slug:'performance', title:'Performance', source:'docs/performance.md'},
  ]},
  {title:'Reference', pages:[
    {slug:'language', title:'The .lm language', source:'docs/language.md'},
    {slug:'standard-library', title:'Standard library', source:'docs/standard-library.md'},
    {slug:'commands', title:'CLI commands', source:'docs/commands.md'},
    {slug:'json-output', title:'JSON output', source:'docs/json-output.md'},
  ]},
];
export const pages = sections.flatMap(section=>section.pages.map(page=>({...page,section:section.title,href:`/docs/${page.slug ? page.slug+'/' : ''}`})));
