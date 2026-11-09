// Shared matte material and batched geometry for the three product diagrams.
// SVG carries the language; transparent WebGL canvases carry relief and motion.
import {changeStory} from './change-story.js';

const vertexSource = `
precision highp float;
attribute vec2 a_position;
attribute vec2 a_uv;
attribute vec4 a_meta;
attribute vec2 a_size;
uniform float u_time;
uniform vec2 u_pointer;
uniform vec2 u_viewbox;
uniform vec4 u_fit;
uniform float u_mode;
varying vec2 v_uv;
varying vec2 v_size;
varying vec4 v_meta;
varying vec2 v_position;
float stage(float index) {
  float t = u_mode < 0.5 ? mod(u_time, 16.0) - index * 4.0 : mod(u_time, 4.0);
  return smoothstep(0.0, 0.6, t) * (1.0 - smoothstep(3.1, 4.0, t));
}
void main() {
  vec2 p = a_position;
  float depth = a_meta.z;
  float active = stage(a_meta.y);
  // Small height changes make propagation readable without orbiting the camera.
  if (a_meta.x > 0.5 && a_meta.x < 1.5) {
    p.y -= active * (1.0 + depth * 2.4);
    p += u_pointer * depth * vec2(2.0, 1.2);
  }
  if (a_meta.x > 1.5 && a_meta.x < 2.5) {
    p.y += sin(a_meta.w * 12.0 + u_time * 2.5 + depth * 5.0)
      * sin(a_meta.w * 3.14159) * active * 2.8;
  }
  if (a_meta.x > 2.5 && a_meta.x < 3.5) {
    float settle = stage(3.0);
    p.x += (1.0 - settle) * sin(depth * 32.0) * 2.4;
    p.y += (1.0 - settle) * cos(depth * 21.0) * 1.5;
  }
  v_uv = a_uv; v_size = a_size; v_meta = a_meta; v_position = p;
  vec2 placed = u_fit.xy + (p / u_viewbox) * u_fit.zw;
  gl_Position = vec4(placed.x * 2.0 - 1.0, 1.0 - placed.y * 2.0, 0.0, 1.0);
}`;

const fragmentSource = `
precision highp float;
uniform float u_time;
uniform vec2 u_pointer;
uniform float u_mode;
uniform float u_state;
uniform float u_alpha;
uniform vec3 u_ink;
uniform vec3 u_steel;
uniform vec3 u_silver;
uniform vec3 u_copper;
uniform vec3 u_sage;
varying vec2 v_uv;
varying vec2 v_size;
varying vec4 v_meta;
varying vec2 v_position;
#define MARK_SCALE 1.4
float hash(vec2 p) {
  vec2 q = mod(p, 127.0);
  return fract(sin(dot(q, vec2(12.9898,78.233))) * 437.5854);
}
float box(vec2 p, vec2 center, vec2 half_size) {
  vec2 d = abs(p - center) - half_size;
  return length(max(d, 0.0)) + min(max(d.x, d.y), 0.0);
}
// The scope mark as one distance field, in the 44-unit space it was drawn in.
float markDistance(vec2 p) {
  float w = 2.4;
  float d = min(box(p, vec2(-22.0, -8.0 - w * 0.5), vec2(w, 12.0 + w * 0.5)),
    box(p, vec2(-10.0 - w * 0.5, -20.0), vec2(12.0 + w * 0.5, w)));
  d = min(d, box(p, vec2(10.0 + w * 0.5, 20.0), vec2(12.0 + w * 0.5, w)));
  d = min(d, box(p, vec2(22.0, 8.0 + w * 0.5), vec2(w, 12.0 + w * 0.5)));
  vec2 q = p - vec2(0.0, -3.0);
  vec2 along = vec2(0.7071, -0.7071);
  return min(d, box(vec2(dot(q, along), dot(q, vec2(-along.y, along.x))), vec2(0.0), vec2(14.14, w)));
}
// A slow domain-warped height field: the liquid surface the mark reflects with.
float liquid(vec2 p, float t) {
  vec2 q = p * 0.075;
  q += 0.7 * vec2(sin(q.y * 1.3 + t * 0.52), cos(q.x * 1.1 - t * 0.41));
  q += 0.4 * vec2(cos(q.y * 2.6 - t * 0.33 + 1.7), sin(q.x * 2.2 + t * 0.47 + 0.6));
  return sin(q.x * 2.1 + q.y * 1.4 + t * 0.21) + 0.55 * sin(q.y * 3.3 - q.x * 2.4 - t * 0.29);
}
vec4 liquidMark() {
  vec2 p = (v_position - v_meta.zw * vec2(1000.0)) / MARK_SCALE;
  float t = mod(u_time, 628.3185);
  // The silhouette breathes by a fraction of its stroke, so it stays the logo.
  vec2 ripple = vec2(sin(p.y * 0.31 + t * 0.6), cos(p.x * 0.27 - t * 0.5)) * 0.14;
  vec2 at = p + ripple;
  float d = markDistance(at);
  float alpha = 1.0 - smoothstep(-0.28, 0.28, d);
  if (alpha < 0.004) discard;
  float e = 0.35;
  vec2 outward = vec2(markDistance(at + vec2(e, 0.0)) - markDistance(at - vec2(e, 0.0)),
    markDistance(at + vec2(0.0, e)) - markDistance(at - vec2(0.0, e))) / (2.0 * e);
  float depth = clamp(-d / 2.4, 0.0, 1.0);
  float tube = sqrt(max(1.0 - (1.0 - depth) * (1.0 - depth), 0.0));
  float height = liquid(p, t);
  vec2 slope = vec2(liquid(p + vec2(0.9, 0.0), t) - height,
    liquid(p + vec2(0.0, 0.9), t) - height) / 0.9;
  vec2 light = normalize(vec2(-0.42, -0.91));
  float relief = dot(outward * (1.0 - tube) * 0.5 + slope * 0.42 + u_pointer * 0.05, light);
  // Matte like the plates: one soft drift of tone between steel and silver.
  float tone = smoothstep(-0.55, 0.55, relief + height * 0.1);
  vec3 color = mix(u_steel, mix(u_steel, u_silver, 0.62), tone);
  color = mix(color, u_copper, (1.0 - tone) * 0.05);
  float crossing = exp(-pow(((p.x - p.y) / 64.0 + 0.5 - fract(t / 8.0) * 1.6 + 0.3) * 5.0, 2.0));
  color += crossing * 0.018 * tube;
  // The same ink edge the plates have keeps it legible on the paper ground.
  color = mix(color, u_ink, smoothstep(-0.8, -0.1, d) * 0.8);
  return vec4(color + (hash(floor(v_position * 3.0)) - 0.5) * 0.065, alpha * u_alpha);
}
void main() {
  float kind = v_meta.x;
  if (kind > 7.5) { gl_FragColor = liquidMark(); return; }
  float t = u_mode < 0.5 ? mod(u_time, 16.0) - v_meta.y * 4.0 : mod(u_time, 4.0);
  float envelope = smoothstep(0.0, 0.5, t) * (1.0 - smoothstep(3.3, 4.0, t));
  float progress = clamp(t / 3.5, 0.0, 1.0);
  float front = exp(-pow((v_meta.w - progress) * 9.0, 2.0)) * envelope;
  float grain = (hash(floor(v_position * 3.0)) - 0.5) * 0.065;
  vec3 color = u_ink;
  float alpha = 1.0;
  if (kind < 0.5 || (kind > 1.5 && kind < 2.5) || (kind > 3.5 && kind < 5.5) || kind > 6.5) {
    // Each link carries a continuous traveling front, rather than blinking dots.
    float edge = 1.0 - smoothstep(0.68, 1.0, abs(v_uv.y * 2.0 - 1.0));
    float scan = front * (0.65 + 0.35 * sin(v_meta.z * 23.0));
    color = mix(u_ink, mix(u_copper * 0.58, u_sage * 0.48, step(1.5, kind)), scan);
    alpha = edge * (0.42 + envelope * 0.18 + scan * 0.38);
    if (kind > 3.5) alpha = edge * (0.58 + scan * 0.35);
    if (kind > 6.5) { alpha = edge * front * 0.95; color = u_copper * 0.5; }
  } else {
    vec2 q = abs(v_uv * 2.0 - 1.0);
    float rim = min((1.0-q.x)*v_size.x, (1.0-q.y)*v_size.y);
    float cut = max(q.x + q.y - 1.87, max(q.x, q.y) - 1.0);
    if (cut > 0.0) discard;
    float border = 1.0 - smoothstep(0.5, 1.5, rim);
    float bevel = (1.0 - smoothstep(1.0, 4.5, rim));
    float raking = (v_uv.x - v_uv.y) * 0.06 + u_pointer.x * 0.012;
    vec3 metal = mix(u_steel, u_silver, 0.16 + v_meta.z * 0.25);
    float affected = step(v_meta.z, progress) * envelope;
    metal = mix(metal, u_copper, affected * 0.65);
    if (kind > 2.5 && kind < 3.5) {
      // Some cells stay outlined: the illustration does not imply every check passes.
      affected *= step(0.17, v_meta.z);
      vec3 outcome = u_mode > 1.5 && u_state < 0.5 ? u_copper : u_sage;
      metal = mix(u_silver, outcome, affected * 0.9);
    }
    if (kind > 5.5) metal = mix(u_silver, u_steel, 0.12);
    color = metal + grain + raking + bevel * (0.035 - v_uv.y * 0.065);
    color = mix(color, u_ink, border * 0.84);
    float etching = step(0.34, v_uv.x) * step(v_uv.x, 0.78)
      * (1.0 - smoothstep(0.035, 0.065, abs(v_uv.y - 0.48)));
    if (kind < 5.5) color = mix(color, u_ink, etching * 0.28);
    alpha = 0.91;
  }
  if (u_mode > 1.5 && u_state < 0.5 && v_position.x > 385.0 && v_position.y > 240.0) {
    color = mix(color, u_copper * 0.52, 0.45);
  }
  gl_FragColor = vec4(color + grain * 0.3, alpha * u_alpha);
}`;

function createBuilder() {
  const vertices = [];
  const corners = [[0,0],[1,0],[1,1],[0,0],[1,1],[0,1]];
  function quad(points, kind, phase, seed, flow = [seed,seed], size = [12,12]) {
    for (const [u,v] of corners) {
      const a = points[0], b = points[1], d = points[3];
      vertices.push(a[0]+(b[0]-a[0])*u+(d[0]-a[0])*v,
        a[1]+(b[1]-a[1])*u+(d[1]-a[1])*v,
        u,v,kind,phase,seed,flow[0]+(flow[1]-flow[0])*u,...size);
    }
  }
  function line(a,b,width,kind,phase,seed,flow) {
    const dx=b[0]-a[0],dy=b[1]-a[1],length=Math.hypot(dx,dy);
    if (!length) return;
    const nx=-dy/length*width/2,ny=dx/length*width/2;
    quad([[a[0]+nx,a[1]+ny],[b[0]+nx,b[1]+ny],
      [b[0]-nx,b[1]-ny],[a[0]-nx,a[1]-ny]],kind,phase,seed,flow,[length,width]);
  }
  function path(points,width,kind,phase,seed=0.5) {
    const lengths=points.slice(1).map((p,i)=>Math.hypot(p[0]-points[i][0],p[1]-points[i][1]));
    const total=lengths.reduce((a,b)=>a+b,0);let distance=0;
    points.slice(1).forEach((p,i)=>{
      line(points[i],p,width,kind,phase,seed,[distance/total,(distance+lengths[i])/total]);
      distance+=lengths[i];
    });
  }
  function tile(x,y,width,height,kind,phase,seed) {
    quad([[x,y],[x+width,y],[x+width,y+height],[x,y+height]],kind,phase,seed);
  }
  const iso=(x,y,z,origin)=>[origin[0]+(x-y)*0.83,origin[1]+(x+y)*0.36-z];
  function plate(x,y,w,h,z,origin,phase,seed) {
    const points=[[x,y],[x+w,y],[x+w,y+h],[x,y+h]].map(([a,b])=>iso(a,b,z,origin));
    // A fine underside and a printed top face give the metal its shallow relief.
    const lower=point=>[point[0],point[1]+(w>60?3.5:2.0)];
    quad([points[3],points[2],lower(points[2]),lower(points[3])],5,phase,seed);
    quad([points[1],points[2],lower(points[2]),lower(points[1])],5,phase,seed);
    quad(points,1,phase,seed,[seed,seed],[w,h]);
  }
  return {quad,line,path,tile,iso,plate,finish:()=>new Float32Array(vertices)};
}

// A chevron narrows to its tip, so anything arriving off-center must stop at
// the chevron's arm or it pokes through. `runPerOffset` is the arm's slope:
// how far back along the pipe the arm is, per unit away from the center.
function stopAtArrow(points,offset,runPerOffset) {
  const end=points[points.length-1],before=points[points.length-2];
  const length=Math.hypot(end[0]-before[0],end[1]-before[1]);
  const back=(Math.abs(offset)*runPerOffset+.5)/length;
  return [...points.slice(0,-1),[end[0]+(before[0]-end[0])*back,end[1]+(before[1]-end[1])*back]];
}
const HERO_ARROW_RUN=14/12,REPLAY_ARROW_RUN=12/10,PULSE_HALF_WIDTH=3;

function createHeroMesh() {
  const mesh=createBuilder();
  const {line,path,tile,iso,plate}=mesh;
  // Three etched folios: one proposed patch inside a much larger body of code.
  const proposal=[140,158];
  for(let layer=0;layer<3;layer++) {
    plate(-57,-35,114,70,layer*9,proposal,0,0.18+layer*0.24);
  }
  for(let row=0;row<7;row++)for(let col=0;col<10;col++) {
    const x=-45+col*9,y=-25+row*7;
    const changed=row>2&&row<5&&col>3&&col<8;
    const a=iso(x,y,29,proposal),b=iso(x+6,y,29,proposal);
    line(a,b,changed?2.4:0.8,changed?4:0,0,(row*10+col)/70,[col/10,(col+1)/10]);
  }
  // Repo/service groups: shared traces connect individual symbols across plates.
  const repos=[[387,138],[519,137],[455,188]];
  path([[408,143],[432,143],[447,128],[500,128]],1.4,0,1,0.3);
  path([[405,151],[428,151],[443,178],[453,178]],1.4,0,1,0.6);
  path([[519,151],[541,166],[485,191]],1.1,0,1,0.8);
  repos.forEach((origin,index)=>{
    plate(-29,-24,58,48,0,origin,1,0.2+index*.25);
    for(let row=0;row<4;row++)for(let col=0;col<5;col++){
      const seed=.04+index*.26+(row+col)/9*.25;
      plate(-24+col*10,-18+row*10,7.4,7.4,3+seed*4,origin,1,seed);
    }
  });
  // A family of replay trajectories: branches separate, oscillate, and reconverge.
  for(let branch=0;branch<13;branch++) {
    const points=[];
    for(let sample=0;sample<=48;sample++) {
      const u=sample/48;
      const spread=Math.sin(Math.PI*Math.min(u*1.1,1));
      const y=469+(branch-6)*6.3*spread
        +Math.sin(u*11+branch*.55)*Math.sin(Math.PI*u)*4;
      points.push([354+u*209,y]);
    }
    path(points,branch%6===0?2.4:1.1,2,3,branch/13);
    tile(558,435+branch*5.5,4,4,1,3,branch/13);
  }
  // Constraint cells settle into registration as the declared requirements run.
  for(let row=0;row<4;row++)for(let col=0;col<9;col++) {
    const seed=((row*13+col*7)%37)/37;
    tile(37+col*23,426+row*22,15,15,3,2,seed);
  }
  for(let row=0;row<4;row++)path([[35,444+row*22],[240,444+row*22]],0.65,0,2,row/4);
  // Smaller stage graphics leave separate space for headings and captions.
  // Keep the original geometry and animation phases, changing only placement.
  const graphics=mesh.finish();
  const placements=[
    {from:[140,158],to:[150,178],scaleX:.88},
    {from:[460,158],to:[510,178],scaleX:.88},
    {from:[140,470],to:[510,386],scaleX:.88},
    // Mirrored, so the arriving arrow meets the point where the replays branch.
    {from:[460,470],to:[150,386],scaleX:-.88},
  ];
  for(let index=0;index<graphics.length;index+=10){
    const placement=placements[graphics[index+5]];
    graphics[index]=placement.to[0]+(graphics[index]-placement.from[0])*placement.scaleX;
    graphics[index+1]=placement.to[1]+(graphics[index+1]-placement.from[1])*.88;
    graphics[index+8]*=.88;graphics[index+9]*=.88;
  }
  // Reroute the same three-lane loop through the open space between stages.
  const buses=createBuilder();
  for(let lane=-1;lane<=1;lane++) {
    buses.path(stopAtArrow([[245,178+lane*4],[302,178+lane*4],[320,160+lane*4],[356,160+lane*4],[374,178+lane*4],[404,178+lane*4]],lane*4,HERO_ARROW_RUN),1.1,4,0,.5+lane*.1);
    buses.path(stopAtArrow([[510+lane*4,256],[510+lane*4,268],[498+lane*4,280],[498+lane*4,284],[510+lane*4,296],[510+lane*4,308]],lane*4,HERO_ARROW_RUN),1.1,4,1,.5+lane*.1);
    buses.path(stopAtArrow([[409,386+lane*4],[360,386+lane*4],[342,404+lane*4],[306,404+lane*4],[288,386+lane*4],[250,386+lane*4]],lane*4,HERO_ARROW_RUN),1.1,4,2,.5+lane*.1);
    buses.path(stopAtArrow([[150+lane*4,308],[150+lane*4,296],[162+lane*4,284],[162+lane*4,280],[150+lane*4,268],[150+lane*4,256]],lane*4,HERO_ARROW_RUN),1.1,4,3,.5+lane*.1);
  }
  // One quad: the mark's outline and relief are computed in the fragment shader.
  const [markX,markY]=[330,282],markExtent=38;
  buses.tile(markX-markExtent,markY-markExtent,markExtent*2,markExtent*2,8,0,0);
  const connectors=buses.finish();
  // A vertex's flow and seed slots carry where the mark is centered.
  for(let index=0;index<connectors.length;index+=10)if(connectors[index+4]===8){
    connectors[index+6]=markX/1000;connectors[index+7]=markY/1000;
  }
  const combined=new Float32Array(graphics.length+connectors.length);
  combined.set(graphics);combined.set(connectors,graphics.length);
  return combined;
}

function proposedStack(mesh,origin=[300,175]) {
  for(let layer=0;layer<4;layer++)mesh.plate(-70,-48,140,96,layer*9,origin,0,.14+layer*.18);
}

function changeTrace(mesh,points,width=1.3,seed=.5) {
  mesh.path(points,width,4,0,seed);
  mesh.path(points,6,7,0,seed);
}

// The two repositories of the quickstart, in the same place in every scene.
const WORKFLOW_Y=240,BILLING_X=150,WEB_X=450;
// A repository plate's left or right corner, where a pipe meets it.
const workflowCorner=(x,side)=>[x+side*63,WORKFLOW_Y+side*3];

function workflowRepository(mesh,x,seed,isChanged=false,y=WORKFLOW_Y) {
  const origin=[x,y];
  mesh.plate(-42,-34,84,68,0,origin,0,seed);
  for(let row=0;row<5;row++)for(let col=0;col<6;col++){
    mesh.plate(-33+col*11,-26+row*11,8,8,4+(col%3)*2,origin,0,seed+(row+col)*.015);
  }
  if(!isChanged)return;
  for(let layer=1;layer<3;layer++)mesh.plate(-42,-34,84,68,layer*7,origin,0,.2+layer*.1);
}

function createFindingMesh() {
  const mesh=createBuilder();
  const end=workflowCorner(WEB_X,-1);
  const contract=replayPipe(workflowCorner(BILLING_X,1),[end[0]-4,end[1]]);
  for(let lane=-1;lane<=1;lane++){
    // The contract changed under the call: the pipe stops, and only a trace reaches web.
    mesh.path(replayLanes(contract.slice(0,2),lane),1.2,4,0,.35+lane*.12);
    mesh.path(stopAtArrow(replayLanes(contract.slice(2),lane),lane*4,REPLAY_ARROW_RUN),.7,0,0,.7);
  }
  mesh.path(contract.slice(0,2),6,7,0,.6);
  workflowRepository(mesh,BILLING_X,.12,true);
  workflowRepository(mesh,WEB_X,.38);
  return mesh.finish();
}

// The setup scene: both repositories feeding one Lemma index below them.
const SETUP_REPOSITORIES=[{x:110,side:-1,seed:.12},{x:490,side:1,seed:.38}];
const SETUP_REPOSITORY_Y=130,STORE_X=300,STORE_Y=370,STORE_HALF=60,STORE_LAYERS=4,STORE_LAYER_HEIGHT=9;
// Square, so its left and right corners sit level and both pipes arrive alike.
const STORE_CORNER_RUN=STORE_HALF*2*.83,STORE_PIPE_Y=STORE_Y-13;

// One lane of a pipe: down from under the repository, then level into the store.
// Lanes stay nested around the corner, as far apart as the other pipes' lanes.
function setupPipe({x,side},lane) {
  const y=STORE_PIPE_Y+side*lane*4;
  return [[x+lane*4,SETUP_REPOSITORY_Y+10],[x+lane*4,y],[STORE_X+side*(STORE_CORNER_RUN+4),y]];
}

function createSetupMesh() {
  const mesh=createBuilder();
  for(const repository of SETUP_REPOSITORIES){
    for(let lane=-1;lane<=1;lane++){
      mesh.path(stopAtArrow(setupPipe(repository,lane),lane*4,REPLAY_ARROW_RUN),1.1,4,0,.35+lane*.12);
    }
    mesh.path(stopAtArrow(setupPipe(repository,0),PULSE_HALF_WIDTH,REPLAY_ARROW_RUN),6,7,0,repository.seed+.3);
    workflowRepository(mesh,repository.x,repository.seed,false,SETUP_REPOSITORY_Y);
  }
  for(let layer=0;layer<STORE_LAYERS;layer++){
    mesh.plate(-STORE_HALF,-STORE_HALF,STORE_HALF*2,STORE_HALF*2,layer*STORE_LAYER_HEIGHT,[STORE_X,STORE_Y],0,.14+layer*.18);
  }
  return mesh.finish();
}

function createDeprecatedCallMesh() {
  const mesh=createBuilder();
  const end=workflowCorner(BILLING_X,1);
  const call=replayPipe(workflowCorner(WEB_X,-1),[end[0]+4,end[1]]);
  for(let lane=-1;lane<=1;lane++){
    mesh.path(stopAtArrow(replayLanes(call,lane),lane*4,REPLAY_ARROW_RUN),1.1,4,0,.35+lane*.12);
  }
  mesh.path(stopAtArrow(call,PULSE_HALF_WIDTH,REPLAY_ARROW_RUN),6,7,0,.5);
  workflowRepository(mesh,BILLING_X,.12);
  workflowRepository(mesh,WEB_X,.38);
  return mesh.finish();
}

function createWorkflowMeshes() {
  return {impact:createFindingMesh(),setup:createSetupMesh(),model:createDeprecatedCallMesh()};
}

// One row's nodes, by column: center, plate width and depth. Pipes join the
// corners of these plates, so the geometry below follows from this table.
const REPLAY_NODES=[{x:85,w:82,h:46},{x:300,w:120,h:56},{x:510,w:92,h:50}];
const replayRow=index=>288+index*120;
// A plate's left or right corner, halfway up its three-layer stack.
const replayCorner=(node,y,side)=>[node.x+side*(node.w+node.h)/2*.83,y+side*(node.w-node.h)/2*.36-4];
// A pipe from one corner to the next: level, a short slope, level. Its three
// lanes are as far apart as the stack's layers, so they meet it edge to edge.
function replayPipe(from,to){
  const span=to[0]-from[0];
  return [from,[from[0]+span*.3,from[1]],[from[0]+span*.55,to[1]],to];
}
const replayLanes=(points,lane)=>points.map(([x,y])=>[x,y+lane*4]);

function createVerificationMeshes() {
  const scenes={};
  for(const state of ['before','after']){
    const mesh=createBuilder();
    proposedStack(mesh,[300,145]);
    changeTrace(mesh,[[300,192],[300,206]],1.2,.2);
    changeStory.behavior.cases.forEach((value,index)=>{
      const y=replayRow(index);
      const [billing,orders,caller]=REPLAY_NODES;
      const incoming=replayPipe(replayCorner(billing,y,1),[replayCorner(orders,y,-1)[0]-4,replayCorner(orders,y,-1)[1]]);
      const outgoing=replayPipe(replayCorner(orders,y,1),[replayCorner(caller,y,-1)[0]-4,replayCorner(caller,y,-1)[1]]);
      for(let lane=-1;lane<=1;lane++){
        mesh.path(stopAtArrow(replayLanes(incoming,lane),lane*4,REPLAY_ARROW_RUN),1.1,4,0,.35+lane*.12);
        if(state==='after'){
          mesh.path(stopAtArrow(replayLanes(outgoing,lane),lane*4,REPLAY_ARROW_RUN),1.1,4,0,.35+lane*.12);
        }else{
          // The flag is lost on the way out: the pipe stops, and only a trace reaches the caller.
          mesh.path(replayLanes(outgoing.slice(0,2),lane),1.2,4,0,.35+lane*.12);
          mesh.path(stopAtArrow(replayLanes(outgoing.slice(2),lane),lane*4,REPLAY_ARROW_RUN),.7,0,0,.7);
        }
      }
      mesh.path(stopAtArrow(incoming,PULSE_HALF_WIDTH,REPLAY_ARROW_RUN),6,7,0,.3);
      if(state==='after')mesh.path(stopAtArrow(outgoing,PULSE_HALF_WIDTH,REPLAY_ARROW_RUN),6,7,0,.6);
      for(const node of REPLAY_NODES){
        for(let layer=0;layer<3;layer++)mesh.plate(-node.w/2,-node.h/2,node.w,node.h,layer*4,[node.x,y],0,.2+layer*.24);
      }
      // The caller's received fields: cells in the plate's own plane, along its front edge.
      const edge=caller.h/2+9,side=7;
      for(let cell=0;cell<6;cell++){
        const along=-36+cell*13;
        const corners=[[along,edge],[along+side,edge],[along+side,edge+side],[along,edge+side]].map(([u,v])=>mesh.iso(u,v,0,[caller.x,y]));
        const seed=.2+cell*.13;
        mesh.quad(corners,3,0,seed,[seed,seed],[side,side]);
      }
    });
    scenes[state]=mesh.finish();
  }
  return scenes;
}

function createStoryEntries() {
  const workflow=createWorkflowMeshes(),verification=createVerificationMeshes();
  return {
    hero:{mesh:createHeroMesh(),mode:0,size:[680,566],layer:'hero'},
    impact:{mesh:workflow.impact,mode:1,size:[600,540],layer:'workflow'},
    setup:{mesh:workflow.setup,mode:1,size:[600,540],layer:'workflow'},
    model:{mesh:workflow.model,mode:1,size:[600,540],layer:'workflow'},
    'verification-before':{mesh:verification.before,mode:2,size:[600,540],layer:'verification'},
    'verification-after':{mesh:verification.after,mode:2,size:[600,540],layer:'verification'},
  };
}

function initStory(figure,entries) {
  const canvas=figure.querySelector('.diagram-webgl');
  const artwork=figure.querySelector('.story-art');
  const layers=Object.fromEntries([...figure.querySelectorAll('[data-layer]')].map(layer=>[layer.dataset.layer,layer]));
  const labels=Object.fromEntries(Object.entries(layers).map(([name,layer])=>[name,[...layer.querySelectorAll('text')]]));
  const lastLabelOpacity={};
  const selectedKey=()=>figure.dataset.scene==='verification'?`verification-${figure.dataset.state}`:figure.dataset.scene;
  let selected=selectedKey();
  const enteredAt=Object.fromEntries(Object.keys(entries).map(key=>[key,0]));
  let weights=Object.fromEntries(Object.keys(entries).map(key=>[key,key===selected?1:0]));
  let gl,gpu=false,program,uniforms,buffers={},frame=0,lastFrame=0,elapsed=0,visible=false;
  let viewport=[1,1],pointer=[0,0],targetPointer=[0,0];
  const reduced=window.matchMedia('(prefers-reduced-motion: reduce)');
  const styles=getComputedStyle(figure);
  const color=name=>{
    const hex=styles.getPropertyValue(`--lm-color-${name}`).trim();
    return [1,3,5].map(index=>parseInt(hex.slice(index,index+2),16)/255);
  };
  const mixing=()=>Object.keys(weights).some(key=>Math.abs(weights[key]-(key===selected?1:0))>.003);
  const snap=()=>{weights=Object.fromEntries(Object.keys(entries).map(key=>[key,key===selected?1:0]));};
  function setRenderer() {
    figure.dataset.renderer=gpu?'webgl':'svg';
    Object.values(layers).forEach(layer=>{layer.dataset.renderer=figure.dataset.renderer;});
  }
  function compile(type,source) {
    const shader=gl.createShader(type);gl.shaderSource(shader,source);gl.compileShader(shader);
    if(!gl.getShaderParameter(shader,gl.COMPILE_STATUS)){
      const error=gl.getShaderInfoLog(shader);gl.deleteShader(shader);throw new Error(error);
    }
    return shader;
  }
  function setup() {
    const high=gl.getShaderPrecisionFormat(gl.FRAGMENT_SHADER,gl.HIGH_FLOAT).precision>0;
    const source=text=>high?text:text.replace('precision highp float;','precision mediump float;');
    const vert=compile(gl.VERTEX_SHADER,source(vertexSource)),frag=compile(gl.FRAGMENT_SHADER,source(fragmentSource));
    program=gl.createProgram();gl.attachShader(program,vert);gl.attachShader(program,frag);gl.linkProgram(program);
    gl.deleteShader(vert);gl.deleteShader(frag);
    if(!gl.getProgramParameter(program,gl.LINK_STATUS))throw new Error(gl.getProgramInfoLog(program));
    gl.useProgram(program);
    uniforms=Object.fromEntries(['time','pointer','viewbox','fit','mode','state','alpha','ink','steel','silver','copper','sage'].map(name=>[name,gl.getUniformLocation(program,`u_${name}`)]));
    for(const name of ['ink','steel','silver','copper','sage'])gl.uniform3fv(uniforms[name],color(name));
    buffers={};
    for(const [key,entry]of Object.entries(entries)){
      const buffer=gl.createBuffer();gl.bindBuffer(gl.ARRAY_BUFFER,buffer);gl.bufferData(gl.ARRAY_BUFFER,entry.mesh,gl.STATIC_DRAW);buffers[key]=buffer;
    }
    gl.enable(gl.BLEND);gl.blendFunc(gl.SRC_ALPHA,gl.ONE_MINUS_SRC_ALPHA);gl.clearColor(0,0,0,0);
    gpu=true;setRenderer();
  }
  function drawEntry(key,weight) {
    const entry=entries[key];
    gl.bindBuffer(gl.ARRAY_BUFFER,buffers[key]);
    for(const [name,size,offset]of[['a_position',2,0],['a_uv',2,2],['a_meta',4,4],['a_size',2,8]]){
      const location=gl.getAttribLocation(program,name);gl.enableVertexAttribArray(location);gl.vertexAttribPointer(location,size,gl.FLOAT,false,40,offset*4);
    }
    const width=Math.min(viewport[0],viewport[1]*entry.size[0]/entry.size[1]);
    const height=width*entry.size[1]/entry.size[0];
    gl.uniform1f(uniforms.time,entry.mode===0?elapsed:elapsed-enteredAt[key]);
    gl.uniform2fv(uniforms.viewbox,entry.size);
    gl.uniform4f(uniforms.fit,(viewport[0]-width)/2/viewport[0],(viewport[1]-height)/2/viewport[1],width/viewport[0],height/viewport[1]);
    gl.uniform1f(uniforms.mode,entry.mode);gl.uniform1f(uniforms.state,key==='verification-before'?0:1);
    gl.uniform1f(uniforms.alpha,weight);gl.drawArrays(gl.TRIANGLES,0,entry.mesh.length/10);
  }
  function paint() {
    if(gpu){
      gl.uniform1f(uniforms.time,elapsed);gl.uniform2fv(uniforms.pointer,pointer);gl.clear(gl.COLOR_BUFFER_BIT);
      for(const key of Object.keys(entries))if(weights[key]>.001)drawEntry(key,weights[key]);
    }
    const amounts={hero:0,workflow:0,verification:0};
    for(const [key,weight]of Object.entries(weights))amounts[entries[key].layer]+=weight;
    for(const [name,layer]of Object.entries(layers)){
      layer.style.opacity=String(amounts[name]);
      layer.setAttribute('aria-hidden',String(name!==entries[selected].layer));
      layer.inert=name!==entries[selected].layer;
      // Geometry blends continuously; labels leave before the next set enters.
      const labelOpacity=amounts[name]>.5?(amounts[name]-.5)*2/amounts[name]:0;
      if(lastLabelOpacity[name]!==labelOpacity){
        labels[name].forEach(label=>{label.style.opacity=String(labelOpacity);});
        lastLabelOpacity[name]=labelOpacity;
      }
    }
    layers.workflow.dataset.phase=figure.dataset.phase;
    layers.verification.dataset.state=figure.dataset.state;
    if(gpu){
      const phase=Math.floor(elapsed%16/4);
      layers.hero.querySelectorAll('[data-loop-stage]').forEach((stage,index)=>stage.style.setProperty('--active-stage',index===phase?'1':'0'));
    }
    figure.dataset.renderScene=selected;
    figure.dataset.transitioning=String(mixing());
  }
  const canRun=()=>visible&&!document.hidden&&document.documentElement.dataset.motion==='on'&&(gpu||mixing());
  function tick(now) {
    frame=0;
    if(!canRun()){lastFrame=0;return;}
    if(!lastFrame)lastFrame=now;
    const delta=now-lastFrame;
    if(delta>=1000/30){
      const seconds=Math.min(delta,100)/1000;elapsed+=seconds;lastFrame=now;
      const blend=1-Math.exp(-seconds/0.15);
      for(const key of Object.keys(weights))weights[key]+=((key===selected?1:0)-weights[key])*blend;
      if(!mixing())snap();
      pointer=pointer.map((value,index)=>value+(targetPointer[index]-value)*.09);
      paint();
    }
    frame=requestAnimationFrame(tick);
  }
  function syncMotion() {
    if(reduced.matches&&mixing()){snap();paint();}
    if(canRun()){if(!frame)frame=requestAnimationFrame(tick);}
    else{cancelAnimationFrame(frame);frame=0;lastFrame=0;}
  }
  function resize() {
    const bounds=artwork.getBoundingClientRect();viewport=[Math.max(1,bounds.width),Math.max(1,bounds.height)];
    const dpr=Math.min(window.devicePixelRatio||1,1.75);
    canvas.width=Math.max(1,Math.round(viewport[0]*dpr));canvas.height=Math.max(1,Math.round(viewport[1]*dpr));
    if(gpu)gl.viewport(0,0,canvas.width,canvas.height);
    paint();
  }
  function choose() {
    const next=selectedKey();if(!entries[next])return;
    if(next!==selected)enteredAt[next]=elapsed;
    selected=next;
    if(document.documentElement.dataset.motion!=='on'||!visible||document.hidden)snap();
    paint();syncMotion();
  }
  canvas.addEventListener('webglcontextlost',event=>{
    event.preventDefault();gpu=false;setRenderer();snap();paint();syncMotion();
  });
  canvas.addEventListener('webglcontextrestored',()=>{
    try{setup();resize();syncMotion();}catch{gpu=false;setRenderer();paint();}
  });
  figure.addEventListener('pointermove',event=>{
    if(event.pointerType==='touch')return;
    const rect=artwork.getBoundingClientRect();targetPointer=[(event.clientX-rect.left)/rect.width*2-1,(event.clientY-rect.top)/rect.height*2-1];
  },{passive:true});
  figure.addEventListener('pointerleave',()=>{targetPointer=[0,0];},{passive:true});
  try{
    gl=canvas.getContext('webgl',{alpha:true,antialias:true,premultipliedAlpha:false,depth:false,stencil:false,powerPreference:'low-power'});
    if(gl)setup();
  }catch{gpu=false;}
  setRenderer();resize();
  new ResizeObserver(resize).observe(artwork);
  new IntersectionObserver(entries=>{visible=entries[0].isIntersecting;syncMotion();}).observe(figure);
  new MutationObserver(syncMotion).observe(document.documentElement,{attributes:true,attributeFilter:['data-motion']});
  new MutationObserver(choose).observe(figure,{attributes:true,attributeFilter:['data-scene','data-phase','data-state']});
  document.addEventListener('visibilitychange',syncMotion);
  window.addEventListener('pagehide',()=>{cancelAnimationFrame(frame);frame=0;lastFrame=0;});
  window.addEventListener('pageshow',syncMotion);
}

// Copied ids get a suffix, so labels and controls point inside their own copy.
function makeIdsUnique(root,suffix) {
  for(const attribute of ['id','aria-labelledby','aria-controls']){
    root.querySelectorAll(`[${attribute}]`).forEach(element=>{
      const ids=element.getAttribute(attribute).split(/\s+/);
      element.setAttribute(attribute,ids.map(id=>`${id}-${suffix}`).join(' '));
    });
  }
}

// Inline, these scenes show only the band their content fills, phone labels included.
const CHAPTER_CROPS={impact:{top:156,height:280},model:{top:156,height:280}};

function cropEntry(entry,{top,height}) {
  const mesh=entry.mesh.slice();
  for(let index=1;index<mesh.length;index+=10)mesh[index]-=top;
  return {...entry,mesh,size:[entry.size[0],height]};
}

function chapterEntries(scene,entries) {
  const crop=CHAPTER_CROPS[scene];
  if(crop)return {[scene]:cropEntry(entries[scene],crop)};
  const keys=scene==='verification'?['verification-before','verification-after']:[scene];
  return Object.fromEntries(keys.map(key=>[key,entries[key]]));
}

function cropFigure(figure,{top,height}) {
  figure.querySelector('[data-layer="workflow"] svg').setAttribute('viewBox',`0 ${top} 600 ${height}`);
  figure.querySelector('.story-art').style.aspectRatio=`600/${height}`;
}

// Only the behavior chapter's copy keeps its switch, and there it is a live control.
function prepareChapterSwitch(figure,scene) {
  const replay=figure.querySelector('.story-replay');
  if(scene!=='verification'){replay.remove();return;}
  replay.removeAttribute('aria-hidden');
  replay.inert=false;
}

function mountChapterDiagram(slot,source,entries) {
  const scene=slot.dataset.chapterScene;
  const figure=source.cloneNode(true);
  makeIdsUnique(figure,scene);
  figure.dataset.diagram='chapter';
  figure.dataset.scene=scene;
  figure.dataset.phase=scene;
  figure.setAttribute('aria-label',slot.dataset.caption);
  figure.querySelector('figcaption').textContent=slot.dataset.caption;
  prepareChapterSwitch(figure,scene);
  if(CHAPTER_CROPS[scene])cropFigure(figure,CHAPTER_CROPS[scene]);
  slot.replaceChildren(figure);
  initStory(figure,chapterEntries(scene,entries));
}

const storyFigure=document.querySelector('[data-diagram="story"]');
if(storyFigure){
  const entries=createStoryEntries();
  initStory(storyFigure,entries);
  // Phones read each diagram inside its chapter. Desktop pins one instead, so
  // the copies are only made once the page is at phone width.
  const phoneWidth=window.matchMedia('(max-width: 850px)');
  const mountChapterDiagrams=()=>document.querySelectorAll('[data-chapter-scene]').forEach(slot=>mountChapterDiagram(slot,storyFigure,entries));
  if(phoneWidth.matches)mountChapterDiagrams();
  else phoneWidth.addEventListener('change',mountChapterDiagrams,{once:true});
}
