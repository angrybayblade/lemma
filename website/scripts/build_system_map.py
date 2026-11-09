"""Draw an architectural view of software relationships, with one exact change.

The drawing is conceptual, not a captured result or a visual product interface.
"""
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
PAPER='#F0F1EB';LINE='#B3BDB0';FAINT='#D1D7CC';INK='#606960';RED='#A9342E'

def project(u,v,level):
    return 102+u+v*.55, 193-u*.38+v*.55+level

def point(u,v,level):
    x,y=project(u,v,level);return f'{x:.2f},{y:.2f}'

def path(points,level,stroke=LINE,width=.8,extra=''):
    coords=' '.join(point(u,v,level) for u,v in points)
    return f'<polyline points="{coords}" fill="none" stroke="{stroke}" stroke-width="{width}" {extra}/>'

parts=['''<svg class="relationship-drawing" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 620 510" role="img" aria-labelledby="drawing-title drawing-desc"><title id="drawing-title">A framework of connected repositories</title><desc id="drawing-desc">Two architectural layers represent related repositories. A small response-field change is marked in red and connected to the caller that still reads it. The surrounding modules show system context.</desc>''']
# Fine, unobtrusive construction lines give a common registration to the layers.
for u,v in [(0,0),(330,0),(330,220),(0,220),(198,88)]:
    a=project(u,v,0);b=project(u,v,128)
    parts.append(f'<path d="M{a[0]:.2f} {a[1]:.2f}L{b[0]:.2f} {b[1]:.2f}" stroke="{FAINT}" stroke-width=".65" stroke-dasharray="2 4"/>')
# Lower layer first: room-like groupings express modules, without console chrome.
for level in [128,0]:
    outline=[(0,0),(330,0),(330,220),(0,220),(0,0)]
    parts.append(path(outline,level,LINE,1))
    for u in range(22,330,22):parts.append(path([(u,0),(u,220)],level,FAINT,.55))
    for v in range(22,220,22):parts.append(path([(0,v),(330,v)],level,FAINT,.55))
    for x,y,w,h in [(0,0,88,66),(110,0,110,66),(242,0,88,88),(0,88,66,88),(88,88,110,88),(220,110,110,66),(0,198,132,22),(154,198,176,22)]:
        parts.append(path([(x,y),(x+w,y),(x+w,y+h),(x,y+h),(x,y)],level,INK,.85))
    for u,v in [(8,8),(118,8),(96,96),(228,118)]:
        parts.append(path([(u,v),(u+35,v)],level,INK,.7))
        parts.append(path([(u,v+6),(u+22,v+6)],level,LINE,.6))
# Exact source and caller locations, registered to the same architectural system.
source=[(198,66),(220,66),(220,88),(198,88),(198,66)]
caller=[(110,132),(132,132),(132,154),(110,154),(110,132)]
parts.append('<g class="change-point">'+path(source,0,RED,1.5)+'</g>')
parts.append('<g class="caller-point">'+path(caller,128,RED,1.5)+'</g>')
sx,sy=project(209,77,0);tx,ty=project(121,143,128)
parts.append(f'<path class="relationship-line" d="M{sx:.2f} {sy:.2f}V{ty:.2f}H{tx:.2f}" fill="none" stroke="{RED}" stroke-width="1.25"/>')
parts.append(f'<circle class="source-contact" cx="{sx:.2f}" cy="{sy:.2f}" r="3.5" fill="{RED}"/><circle class="caller-contact" cx="{tx:.2f}" cy="{ty:.2f}" r="3.5" fill="{RED}"/>')
# Short leaders and labels sit outside the architecture rather than on fake panels.
parts.append(f'<path d="M{sx:.2f} {sy:.2f}H550" stroke="{RED}" stroke-width=".65"/><text x="562" y="{sy+4:.2f}" class="drawing-index">01</text>')
parts.append(f'<path d="M{tx:.2f} {ty:.2f}H40" stroke="{RED}" stroke-width=".65"/><text x="16" y="{ty+4:.2f}" class="drawing-index">02</text>')
parts.append('</svg>')
(ROOT/'src/system-map.svg').write_text(''.join(parts)+'\n')
print('Built architectural relationship drawing.')
