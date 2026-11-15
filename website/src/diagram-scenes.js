// Scenes composed from the diagram kit's units (`diagram-kit.js`), one per
// idea the docs show. Each is a list of units and the words that go with them,
// with a caption saying what it shows; `build` returns the canvas's geometry
// and the SVG over it. Pure, so the docs build can render the SVG ahead of time.
import {createScene, fields, node, pipe, repository, stack} from './diagram-kit.js';

// The agent's loop with a stop gate: its change is checked, sent back once,
// fixed, and let through. Four stages of four seconds.
function gate() {
  const scene=createScene({width:600,height:400,mode:0});
  const agent=node(scene,{at:[100,160],name:'your agent',seed:.9,phase:2});
  const check=node(scene,{at:[300,160],name:'lemma check --change',code:true,note:'what the change breaks',w:96,h:56,seed:.9,phase:0});
  const stop=node(scene,{at:[440,160],name:'stop gate',w:28,h:28,layers:6,seed:.9,phase:1});
  const done=node(scene,{at:[540,160],name:'done',note:'no errors left',w:56,h:40,seed:.9,phase:3});
  // 1: the change is checked. 2: the gate finds errors. 3: back to the agent. 4: fixed, it passes.
  pipe(scene,{from:agent.right,to:check.left,phase:0});
  pipe(scene,{from:check.right,to:stop.left,phase:1,arrow:false});
  pipe(scene,{from:stop.bottom,to:agent.bottom,shape:'under',low:300,phase:2});
  pipe(scene,{from:agent.right,to:check.left,phase:3,lanes:false,span:[0,.4]});
  pipe(scene,{from:check.right,to:stop.left,phase:3,lanes:false,arrow:false,span:[.4,.7]});
  pipe(scene,{from:stop.right,to:done.left,phase:3,span:[.7,1]});
  scene.label([270,330],'errors it introduced: sent back',`flow-meta`);
  return scene.finish();
}

// One symbol's references, as the language server finds them, beside what a
// text search for its name also matches.
function references() {
  const scene=createScene({width:600,height:420});
  const symbol=node(scene,{at:[110,210],name:'charge()',code:true,note:'billing/charge.ts',seed:.05});
  const targets=[
    {y:70,name:'web/checkout.ts:12',note:'calls charge()',real:true},
    {y:140,name:'refund.charge()',note:'another symbol, same name'},
    {y:210,name:'orders/pay.py:40',note:'calls charge(), through an alias',real:true},
    {y:280,name:'# charge the card',note:'a comment'},
    {y:350,name:'"charge failed"',note:'a string'},
  ];
  for(const target of targets){
    const at=[330,target.y];
    const end=node(scene,{at,name:target.name,note:target.note,code:true,w:56,h:32,layers:2,seed:.92,quiet:!target.real,labels:'right'});
    pipe(scene,{from:symbol.right,to:end.left,faint:!target.real,span:[0,1]});
    if(!target.real)scene.cross([end.left[0]-16,end.left[1]],5);
  }
  return scene.finish();
}

// A check reads only what a change reaches: the changed repository, the code
// that calls it, and nothing else.
function reach() {
  const scene=createScene({width:600,height:440});
  const billing=repository(scene,{at:[110,130],name:'billing',note:'changed POST /v1/charges',changed:true,seed:.05,labels:'above'});
  const web=repository(scene,{at:[330,130],name:'web',note:'calls it, and breaks',seed:.55,labels:'above'});
  repository(scene,{at:[510,130],name:'admin',note:'not read',quiet:true,labels:'above'});
  pipe(scene,{from:web.left,to:billing.right,broken:true,span:[0,.5]});
  const lemma=stack(scene,{at:[220,340],half:46,name:'lemma',note:'reads only what the change reaches',seed:.8});
  pipe(scene,{from:billing.bottom,to:lemma.left,shape:'down',span:[0,.75]});
  pipe(scene,{from:web.bottom,to:lemma.right,shape:'down',span:[.2,.75]});
  return scene.finish();
}

// A broken call, and what it affects further up: the handler that makes it,
// and the code in another repository that calls that handler.
function trace() {
  const scene=createScene({width:600,height:340});
  const checkout=node(scene,{at:[90,150],name:'checkout',code:true,note:'web: calls the handler',seed:.92,labels:'above'});
  const handler=node(scene,{at:[300,150],name:'place_order',code:true,note:'orders: the handler',seed:.5,labels:'above'});
  const operation=node(scene,{at:[510,150],name:'POST /v1/charges',code:true,note:'billing: changed',seed:.05,labels:'above'});
  pipe(scene,{from:checkout.right,to:handler.left,pulse:false});
  pipe(scene,{from:handler.right,to:operation.left,broken:true,span:[0,.1]});
  // What it affects travels back up the calls, nearest first.
  pipe(scene,{from:operation.bottom,to:handler.bottom,shape:'under',low:270,span:[0,.5],arrow:true});
  pipe(scene,{from:[handler.bottom[0]-16,handler.bottom[1]],to:checkout.bottom,shape:'under',low:270,span:[.5,.95],arrow:true});
  scene.label([300,310],'affects, nearest first','flow-meta');
  return scene.finish();
}

// A requirement of your own: every call to a deprecated operation is flagged.
function requirement() {
  const scene=createScene({width:600,height:400});
  const operation=node(scene,{at:[450,210],name:'POST /v1/charges',code:true,note:'in Deprecated',seed:.95});
  scene.flag([450+10,210-62]);
  const callers=[{y:130,name:'web: checkout'},{y:320,name:'orders: pay'}];
  for(const caller of callers){
    const end=node(scene,{at:[140,caller.y],name:caller.name,note:'warning',seed:.9});
    pipe(scene,{from:end.right,to:operation.left,span:[0,1]});
  }
  scene.label([300,34],'no_calls_to_deprecated_operations','kit-code');
  return scene.finish();
}

// A behavior's checker answers the call in billing's place, runs the real
// handler, and keeps what the caller received as evidence.
function replay() {
  const scene=createScene({width:600,height:420});
  const faked=node(scene,{at:[100,150],name:'billing, faked',note:'retriable: true, then false',quiet:true,labels:'above'});
  const handler=node(scene,{at:[300,150],name:'place_order',code:true,note:'orders: the real handler',seed:.45,labels:'above'});
  const caller=node(scene,{at:[500,150],name:'its response',note:'carries retriable?',seed:.9,labels:'above'});
  fields(scene,{at:[500,204],count:5});
  pipe(scene,{from:faked.right,to:handler.left,span:[0,.45]});
  pipe(scene,{from:handler.right,to:caller.left,span:[.45,.9]});
  const evidence=stack(scene,{at:[300,330],half:36,layers:3,name:'evidence',note:'kept until the code changes',seed:.98});
  pipe(scene,{from:caller.right,to:evidence.right,shape:'down',span:[.9,1]});
  return scene.finish();
}

/** Every scene, by the name a docs page marks it with, and what its caption says. */
export const SCENES={
  gate:{build:gate,caption:'The stop gate: the agent’s change is checked, sent back while it introduces errors, and let through once it is fixed.'},
  references:{build:references,caption:'The references to charge() a language server finds, beside the three a text search for its name also matches.'},
  reach:{build:reach,caption:'Billing’s change, the call in web it breaks, and the repository the check never had to read.'},
  trace:{build:trace,caption:'A broken call, and what it affects further up: the handler that makes it and the code in another repository that calls that handler.'},
  requirement:{build:requirement,caption:'A requirement of your own: every call to an operation in Deprecated is flagged.'},
  replay:{build:replay,caption:'The checker answers in billing’s place, the real handler runs, and what the caller received is kept as evidence.'},
};
