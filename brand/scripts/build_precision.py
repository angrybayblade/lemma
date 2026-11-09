"""Generate the Swiss / architectural direction from its source tokens.

Uses the already licensed Geist fonts and the shared outlined-type helpers.
"""
import json
from pathlib import Path
from build_assets import label, mark, svg

ROOT=Path(__file__).resolve().parents[1]
OUT=ROOT/'precision'
T=json.loads((OUT/'tokens.json').read_text())
C=T['color']

def rectangle(x,y,w,h,fill,stroke=None):
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{fill}"'+(f' stroke="{stroke}"' if stroke else '')+'/>'

def logo(x=0,y=0,size=36):
    return mark(x,y,size,C['red'])+label('lemma',x+size*1.35,y+size*.83,size*.95,'medium',C['graphite'])

def build():
    lines=['/* Generated from precision/tokens.json by build_precision.py. */']
    for family,file in [('Geist','Geist-Variable.woff2'),('Geist Mono','GeistMono-Variable.woff2')]:
        lines.append(f"@font-face {{ font-family:'{family}'; src:url('../fonts/{file}') format('woff2'); font-weight:100 900; font-style:normal; font-display:swap; }}")
    lines.append(':root {')
    for section in ['color','font','space','radius','motion']:
        lines.extend(f'  --lm-{section}-{k}: {v};' for k,v in T[section].items())
    lines.extend(f'  --lm-{k}: {C.get(v,v)};' for k,v in T['theme']['light'].items())
    lines.append('  color-scheme: light;\n}')
    lines.append('@media(prefers-reduced-motion:reduce) { :root { --lm-motion-fast:0ms; --lm-motion-normal:0ms; } }')
    (OUT/'tokens.css').write_text('\n'.join(lines)+'\n')
    (OUT/'logos/lockup.svg').write_text(svg(238,46,'Lemma',logo(0,4,40)))
    (OUT/'logos/mark.svg').write_text(svg(32,32,'Lemma scope mark',mark(0,0,32,C['red'])))
    (OUT/'logos/favicon.svg').write_text(svg(32,32,'Lemma',rectangle(0,0,32,32,C['red'])+mark(4,4,24,C['white'])))
    (OUT/'grain.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200"><filter id="grain"><feTurbulence type="fractalNoise" baseFrequency=".7" numOctaves="3" stitchTiles="stitch"/><feColorMatrix type="saturate" values="0"/></filter><rect width="100%" height="100%" filter="url(#grain)" opacity=".4"/></svg>\n')
    body=rectangle(0,0,1200,630,C['paper'])+logo(54,43,38)
    body+=label('A framework for connected code.',54,149,17,'sans',C['secondary'])
    body+=label('Move fast.',48,285,104,'sans',C['graphite'],-4.5)
    body+=label('With precision.',48,397,104,'sans',C['red'],-4.5)
    body+=label('Find what a change breaks. Before you merge.',54,466,23,'sans',C['graphite'])
    body+=f'<path d="M54 550H1146" stroke="{C["rule"]}"/>'
    body+=label('For people and coding agents.',54,590,15,'sans',C['secondary'])
    # A measured modular construction; no simulated interface.
    for row in range(6):
        for col in range(4):
            x=875+col*48;y=175+row*35
            body+=rectangle(x,y,34,23,'none',C['rule'])
    body+=rectangle(971,280,34,23,'none',C['red'])+f'<path d="M988 303V418H1120" fill="none" stroke="{C["red"]}"/>'+rectangle(1114,412,12,12,C['red'])
    (OUT/'marketing/social-card.svg').write_text(svg(1200,630,'Lemma. Move fast. With precision.',body))
    print('Built precision tokens, logos, grain, and sharing artwork.')

if __name__=='__main__': build()
