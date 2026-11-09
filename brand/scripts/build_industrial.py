"""Generate the industrial direction's tokens, logo variants, and campaign art.

Requires fonttools. Font licenses remain in ../fonts/licenses.
"""
import json
from pathlib import Path
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.varLib.instancer import instantiateVariableFont

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'industrial'
T = json.loads((OUT / 'tokens.json').read_text())
C = T['color']
fonts = {}
for key, file, weight in [('display','BarlowCondensed-SemiBold.ttf',None),('sans','Geist-Variable.ttf',550),('mono','GeistMono-Variable.ttf',400)]:
    font=TTFont(ROOT/'fonts'/file)
    if weight: font=instantiateVariableFont(font, {'wght':weight}, inplace=False)
    fonts[key]=font

def text(value,x,y,size=16,face='sans',color=None,spacing=0):
    f=fonts[face]; gs=f.getGlyphSet(); cmap=f.getBestCmap(); scale=size/f['head'].unitsPerEm; parts=[]
    for char in value:
        glyph=cmap.get(ord(char),'.notdef');pen=SVGPathPen(gs);gs[glyph].draw(pen)
        parts.append(f'<path transform="translate({x:.3f} {y}) scale({scale:.6f} {-scale:.6f})" d="{pen.getCommands()}"/>')
        x+=gs[glyph].width*scale+spacing
    return f'<g fill="{color or C["white"]}">{"".join(parts)}</g>'

def mark(x,y,size,color):
    return f'<g transform="translate({x} {y}) scale({size/32})" fill="{color}"><path d="M2 2H10V22H30V30H2Z"/><path d="M18 2H30V14H22V10H18Z"/></g>'

def logo(x=0,y=0,size=36,light=False):
    return mark(x,y,size,C['signal'] if not light else C['signalDark'])+text('lemma',x+size*1.35,y+size*.83,size*.95,'sans',C['white'] if not light else C['graphite'])

def svg(w,h,title,body):
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" role="img" aria-label="{title}"><title>{title}</title>{body}</svg>\n'

def rect(x,y,w,h,fill,stroke=None):
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{fill}"'+(f' stroke="{stroke}"' if stroke else '')+'/>'

def grid(w,h):
    return f'<defs><pattern id="grid" width="32" height="32" patternUnits="userSpaceOnUse"><path d="M32 0H0V32" fill="none" stroke="{C["line"]}" stroke-width=".5"/></pattern><filter id="grain"><feTurbulence type="fractalNoise" baseFrequency=".8" numOctaves="3" stitchTiles="stitch"/><feColorMatrix type="saturate" values="0"/></filter></defs>'+rect(0,0,w,h,C['graphite'])+rect(0,0,w,h,'url(#grid)')+f'<rect width="{w}" height="{h}" opacity=".055" filter="url(#grain)"/>'

def build():
    lines=['/* Generated from industrial/tokens.json by build_industrial.py. */']
    for name,file,weight in [('Barlow Condensed','BarlowCondensed-SemiBold.woff2','600'),('Geist','Geist-Variable.woff2','100 900'),('Geist Mono','GeistMono-Variable.woff2','100 900')]:
        lines.append(f"@font-face {{ font-family:'{name}'; src:url('../fonts/{file}') format('woff2'); font-weight:{weight}; font-style:normal; font-display:swap; }}")
    lines.append(':root {')
    for section in ['color','font','space','radius','motion']:
        lines.extend(f'  --lm-{section}-{k}: {v};' for k,v in T[section].items())
    lines.extend(f'  --lm-{k}: {C.get(v,v)};' for k,v in T['theme']['dark'].items())
    lines.append('  color-scheme: dark;\n}')
    lines.append('[data-lm-theme="light"] {')
    lines.extend(f'  --lm-{k}: {C.get(v,v)};' for k,v in T['theme']['light'].items())
    lines.append('  color-scheme: light;\n}')
    lines.append('@media (prefers-reduced-motion: reduce) { :root { --lm-motion-fast:0ms; --lm-motion-normal:0ms; --lm-motion-trace:0ms; } }')
    (OUT/'tokens.css').write_text('\n'.join(lines)+'\n')
    for name,light in [('lockup-dark.svg',False),('lockup-light.svg',True)]:
        (OUT/'logos'/name).write_text(svg(238,46,'Lemma',logo(0,4,40,light)))
    (OUT/'logos'/'mark-signal.svg').write_text(svg(32,32,'Lemma scope mark',mark(0,0,32,C['signal'])))
    (OUT/'logos'/'favicon.svg').write_text(svg(32,32,'Lemma',rect(0,0,32,32,C['signal'])+mark(4,4,24,C['graphite'])))
    (OUT/'texture.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg" width="160" height="160" viewBox="0 0 160 160"><filter id="n"><feTurbulence type="fractalNoise" baseFrequency=".85" numOctaves="3" stitchTiles="stitch"/><feColorMatrix type="saturate" values="0"/></filter><rect width="100%" height="100%" opacity=".65" filter="url(#n)"/></svg>\n')
    body=grid(1200,630)+logo(54,43,37)
    body+=text('PRECISION FOR CONNECTED CODE.',54,139,12,'mono',C['steel'],1)
    body+=text('MOVE FAST.',49,277,137,'display')+text('KNOW THE IMPACT.',49,411,137,'display',C['signal'])
    body+=text('Catch breaking changes across services before you merge.',54,476,22,'sans',C['alloy'])
    body+=rect(54,545,1092,1,C['line'])+text('FOR PEOPLE + CODING AGENTS',54,584,12,'mono',C['steel'],1)+text('CLI / MCP / CI',965,584,12,'mono',C['steel'])
    # A compact circuit field, with one marked tolerance point.
    for row in range(7):
        for col in range(5):
            x=873+col*54;y=75+row*38
            body+=rect(x,y,38,20,C['panel'],C['signal'] if (row,col)==(3,2) else C['line'])
    body+=f'<path d="M1000 85V199H1140" fill="none" stroke="{C["signal"]}" stroke-width="2"/>'
    (OUT/'marketing'/'social-card.svg').write_text(svg(1200,630,'Lemma. Move fast. Know the impact.',body))
    print('Generated industrial tokens, logos, texture, and social card.')

if __name__=='__main__':build()
