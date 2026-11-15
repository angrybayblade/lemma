// The units Lemma's diagrams are composed of. A scene is drawn twice over the
// same coordinates: geometry for the matte WebGL canvas (`createBuilder`), and
// an SVG that keeps the words, arrowheads and marks sharp, with a plain
// drawing for when WebGL is not there. Each unit adds to both, so a scene is a
// list of units and nothing else. Pure: no DOM, so the docs build can render a
// scene's SVG ahead of time.

// Geometry, as the canvas's shaders read it: position, uv, kind, phase, seed,
// flow, and size, ten floats a vertex. `kind` picks the material: 0 a faint
// trace, 1 a plate's face, 3 a field cell, 4 a pipe lane, 5 a plate's edge,
// 6 a plate that never takes the pulse's copper, 7 the pulse, 8 the mark.
export function createBuilder() {
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
  // `span` is the part of the stage's flow this path covers, so a pulse can cross several paths in turn.
  function path(points,width,kind,phase,seed=0.5,span=[0,1]) {
    const lengths=points.slice(1).map((p,i)=>Math.hypot(p[0]-points[i][0],p[1]-points[i][1]));
    const total=lengths.reduce((a,b)=>a+b,0);let distance=0;
    const flow=part=>span[0]+part*(span[1]-span[0]);
    points.slice(1).forEach((p,i)=>{
      line(points[i],p,width,kind,phase,seed,[flow(distance/total),flow((distance+lengths[i])/total)]);
      distance+=lengths[i];
    });
  }
  function tile(x,y,width,height,kind,phase,seed) {
    quad([[x,y],[x+width,y],[x+width,y+height],[x,y+height]],kind,phase,seed);
  }
  const iso=(x,y,z,origin)=>[origin[0]+(x-y)*0.83,origin[1]+(x+y)*0.36-z];
  function plate(x,y,w,h,z,origin,phase,seed,kind=1) {
    const points=[[x,y],[x+w,y],[x+w,y+h],[x,y+h]].map(([a,b])=>iso(a,b,z,origin));
    // A fine underside and a printed top face give the metal its shallow relief.
    const lower=point=>[point[0],point[1]+(w>60?3.5:2.0)];
    quad([points[3],points[2],lower(points[2]),lower(points[3])],5,phase,seed);
    quad([points[1],points[2],lower(points[2]),lower(points[1])],5,phase,seed);
    quad(points,kind,phase,seed,[seed,seed],[w,h]);
  }
  return {quad,line,path,tile,iso,plate,finish:()=>new Float32Array(vertices)};
}

// A chevron narrows to its tip, so anything arriving off-center must stop at
// the chevron's arm or it pokes through. `runPerOffset` is the arm's slope:
// how far back along the pipe the arm is, per unit away from the center.
export function stopAtArrow(points,offset,runPerOffset) {
  const end=points[points.length-1],before=points[points.length-2];
  const length=Math.hypot(end[0]-before[0],end[1]-before[1]);
  const back=(Math.abs(offset)*runPerOffset+.5)/length;
  return [...points.slice(0,-1),[end[0]+(before[0]-end[0])*back,end[1]+(before[1]-end[1])*back]];
}
export const ARROW_RUN=12/10,HERO_ARROW_RUN=14/12,PULSE_HALF_WIDTH=3;

const escape=text=>String(text).replaceAll('&','&amp;').replaceAll('<','&lt;').replaceAll('>','&gt;');
const round=value=>Math.round(value*10)/10;
const pointsAttribute=points=>points.map(([x,y])=>`${round(x)},${round(y)}`).join(' ');

/** A scene being composed: `mode` 1 loops one four-second flow; 0 runs four stages of four seconds, each unit in its `phase`. */
export function createScene({width,height,mode=1}) {
  const mesh=createBuilder(),words=[],marks=[],pictogram=[];
  return {
    mesh,width,height,mode,
    /** Words, in the landing page's classes: `flow-repository`, `flow-label`, `flow-meta`, or `kit-code`. */
    label([x,y],text,kind='flow-label',anchor='middle'){words.push(`<text class="${kind}" x="${round(x)}" y="${round(y)}" text-anchor="${anchor}">${escape(text)}</text>`);},
    /** A chevron at the end of a pipe whose last segment runs from `before` to `end`. */
    arrowhead(before,end){
      const angle=Math.atan2(end[1]-before[1],end[0]-before[0]),arm=7;
      const at=sign=>[end[0]-Math.cos(angle+sign*.87)*arm*1.3,end[1]-Math.sin(angle+sign*.87)*arm*1.3];
      marks.push(`<polyline class="kit-arrowhead" points="${pointsAttribute([at(1),end,at(-1)])}"/>`);
    },
    /** A broken link or a false match. */
    cross([x,y],size=6){marks.push(`<path class="kit-cross" d="M${round(x-size)} ${round(y-size)}L${round(x+size)} ${round(y+size)}M${round(x+size)} ${round(y-size)}L${round(x-size)} ${round(y+size)}"/>`);},
    /** Something a rule flags. */
    flag([x,y]){marks.push(`<path class="kit-flag-pole" d="M${round(x)} ${round(y)}V${round(y-30)}"/><path class="kit-flag" d="M${round(x)} ${round(y-30)}l18 6-18 6z"/>`);},
    /** A shape for the drawing shown without WebGL. */
    sketch(element){pictogram.push(element);},
    finish(){
      const svg=`<svg class="diagram-svg kit-scene" viewBox="0 0 ${width} ${height}" aria-hidden="true"><g class="kit-pictogram">${pictogram.join('')}</g><g class="kit-marks">${marks.join('')}</g>${words.join('')}</svg>`;
      return {mesh:mesh.finish(),svg,size:[width,height],mode};
    },
  };
}

// Where a plate of half-sizes `w`×`h` at `at` meets a pipe: its left, right, top and bottom corners.
function corners([x,y],w,h,lift=0) {
  return {
    left:[x-(w+h)/2*.83,y+(h-w)/2*.36-lift],
    right:[x+(w+h)/2*.83,y+(w-h)/2*.36-lift],
    top:[x+(h-w)/2*.83,y-(w+h)/2*.36-lift],
    bottom:[x+(w-h)/2*.83,y+(w+h)/2*.36-lift],
  };
}
function sketchPlate(scene,at,w,h,quiet) {
  const c=corners(at,w,h);
  scene.sketch(`<polygon class="kit-sketch${quiet?' kit-sketch--quiet':''}" points="${pointsAttribute([c.top,c.right,c.bottom,c.left])}"/>`);
}

/**
 * A repository: a plate printed with its symbols. `seed` is when the pulse
 * reaches it (0 to 1 of the flow); `changed` stacks it, as a commit does;
 * `quiet` keeps it out of the flow. Its name goes above and `note` below, or
 * both above with `labels: 'above'`, leaving room for pipes underneath.
 */
export function repository(scene,{at,name,note,seed=.5,phase=0,changed=false,quiet=false,labels='around'}) {
  const {mesh}=scene,kind=quiet?6:1;
  mesh.plate(-42,-34,84,68,0,at,phase,seed,kind);
  for(let row=0;row<5;row++)for(let col=0;col<6;col++){
    mesh.plate(-33+col*11,-26+row*11,8,8,4+(col%3)*2,at,phase,seed+(row+col)*.015,kind);
  }
  if(changed)for(let layer=1;layer<3;layer++)mesh.plate(-42,-34,84,68,layer*7,at,phase,seed,kind);
  sketchPlate(scene,at,84,68,quiet);
  const c=corners(at,84,68,changed?7:0);
  const nameClass=quiet?'flow-repository kit-quiet':'flow-repository',noteClass=quiet?'flow-meta kit-quiet':'flow-meta';
  if(labels==='above'){
    if(name)scene.label([at[0],c.top[1]-(note?40:16)],name,nameClass);
    if(note)scene.label([at[0],c.top[1]-16],note,noteClass);
    return c;
  }
  if(name)scene.label([at[0],c.top[1]-16],name,nameClass);
  if(note)scene.label([at[0],c.bottom[1]+26],note,noteClass);
  return c;
}

/** A function, operation or check: a small stack of plates. Its words go above and below; with `labels: 'right'` beside it, for a list; with `labels: 'above'` both above, leaving room for pipes underneath. */
export function node(scene,{at,name,note,w=82,h=46,layers=3,seed=.5,phase=0,quiet=false,code=false,labels='around'}) {
  const {mesh}=scene;
  for(let layer=0;layer<layers;layer++)mesh.plate(-w/2,-h/2,w,h,layer*4,at,phase,quiet?seed:seed+layer*.02,quiet?6:1);
  sketchPlate(scene,at,w,h,quiet);
  const c=corners(at,w,h,(layers-1)*4);
  const nameClass=`${code?'kit-code':'flow-label'}${quiet?' kit-quiet':''}`,noteClass=`flow-meta${quiet?' kit-quiet':''}`;
  if(labels==='right'){
    const x=corners(at,w,h).right[0]+14;
    if(name)scene.label([x,at[1]-2],name,nameClass,'start');
    if(note)scene.label([x,at[1]+18],note,noteClass,'start');
  }else if(labels==='above'){
    if(name)scene.label([at[0],c.top[1]-(note?36:14)],name,nameClass);
    if(note)scene.label([at[0],c.top[1]-14],note,noteClass);
  }else{
    if(name)scene.label([at[0],c.top[1]-14],name,nameClass);
    if(note)scene.label([at[0],corners(at,w,h).bottom[1]+22],note,noteClass);
  }
  return corners(at,w,h,4);
}

/** Lemma's index: one wide stack the repositories feed. */
export function stack(scene,{at,half=56,layers=4,name='lemma',note,seed=.9,phase=0}) {
  for(let layer=0;layer<layers;layer++)scene.mesh.plate(-half,-half,half*2,half*2,layer*9,at,phase,seed+layer*.02);
  sketchPlate(scene,at,half*2,half*2,false);
  const c=corners(at,half*2,half*2,(layers-1)*9);
  if(name)scene.label([at[0],corners(at,half*2,half*2).bottom[1]+26],name,'flow-repository');
  if(note)scene.label([at[0],corners(at,half*2,half*2).bottom[1]+50],note,'flow-meta');
  return {...c,left:[c.left[0],c.left[1]+13],right:[c.right[0],c.right[1]+13]};
}

/** The route of a pipe: level, a short slope, level (`step`); down then across (`down`); or down, across and up again, passing under what lies between (`under`, `low` deep). */
function route(from,to,shape,low) {
  if(shape==='down')return [from,[from[0],to[1]],to];
  if(shape==='under')return [from,[from[0],low],[to[0],low],to];
  const span=to[0]-from[0];
  return [from,[from[0]+span*.3,from[1]],[from[0]+span*.55,to[1]],to];
}
// One lane of a route: three lanes run four units apart, nested around corners.
function lane(points,offset,shape,from,to) {
  if(shape==='under')return points.map(([x,y],index)=>index===0||index===3?[x+offset,y]:[x+offset,y+offset]);
  if(shape!=='down')return points.map(([x,y])=>[x,y+offset]);
  const side=from[0]<to[0]?-1:1;
  return [[points[0][0]+offset,points[0][1]],[points[1][0]+offset,points[1][1]+side*offset],[points[2][0],points[2][1]+side*offset]];
}

/**
 * A pipe from one unit to another: three lanes and a pulse crossing `span` of
 * the flow. `broken` stops the lanes and the pulse where the link fails and
 * marks it; `faint` draws a bare trace, as for a text-search match. Without
 * `lanes` only the pulse is drawn, to run a route again in a later phase;
 * without `pulse` only the lanes, for a link nothing travels along.
 */
export function pipe(scene,{from,to,shape='step',span=[0,1],phase=0,seed=.4,broken=false,faint=false,arrow=true,lanes=true,pulse=true,low}) {
  const {mesh}=scene;
  const end=shape==='under'?[to[0],to[1]+4]:[to[0]+(from[0]<to[0]?-4:4),to[1]];
  const points=route(from,end,shape,low??Math.max(from[1],to[1])+60);
  scene.sketch(`<polyline class="kit-sketch kit-sketch--line${faint?' kit-sketch--quiet':''}" points="${pointsAttribute(points)}"/>`);
  if(faint){
    mesh.path(points,.8,0,phase,.7);
    return points;
  }
  if(broken){
    const cut=Math.ceil(points.length/2);
    for(let offset=-4;offset<=4;offset+=4){
      const laneRoute=lane(points,offset,shape,from,end);
      mesh.path(laneRoute.slice(0,cut),1.2,4,phase,.35+offset*.03);
      mesh.path(laneRoute.slice(cut-1),.7,0,phase,.7);
    }
    mesh.path(points.slice(0,cut),6,7,phase,seed,[span[0],(span[0]+span[1])/2]);
    const at=points[cut-1],next=points[cut];
    scene.cross([(at[0]+next[0])/2,(at[1]+next[1])/2],7);
    return points;
  }
  if(lanes)for(let offset=-4;offset<=4;offset+=4){
    const laneRoute=lane(points,offset,shape,from,end);
    mesh.path(arrow?stopAtArrow(laneRoute,offset,ARROW_RUN):laneRoute,1.1,4,phase,.35+offset*.03);
  }
  if(pulse)mesh.path(arrow?stopAtArrow(points,PULSE_HALF_WIDTH,ARROW_RUN):points,6,7,phase,seed,span);
  if(arrow&&lanes)scene.arrowhead(points[points.length-2],points[points.length-1]);
  return points;
}

/** The fields a caller received, settling in once the flow reaches them. */
export function fields(scene,{at,count=6,phase=0}) {
  const side=7;
  for(let cell=0;cell<count;cell++){
    const along=-((count-1)*13)/2+cell*13-side/2;
    const quadCorners=[[along,0],[along+side,0],[along+side,side],[along,side]].map(([u,v])=>scene.mesh.iso(u,v,0,at));
    const seed=.2+cell*.13;
    scene.mesh.quad(quadCorners,3,phase,seed,[seed,seed],[side,side]);
  }
}
