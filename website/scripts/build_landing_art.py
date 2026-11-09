"""Build original, geometric SVG illustrations for Lemma's agent landing page."""

from pathlib import Path
import json
from build_change_story_art import build_change_story

ROOT = Path(__file__).resolve().parents[1]


def svg(body: str, name: str, description: str, viewbox: str = "0 0 600 540", detail: str = "A conceptual diagram, not captured application output.") -> str:
    return f'<svg xmlns="http://www.w3.org/2000/svg" class="diagram-svg" viewBox="{viewbox}" fill="none" role="img" aria-labelledby="{name}-title {name}-desc"><title id="{name}-title">{description}</title><desc id="{name}-desc">{detail}</desc>{body}</svg>\n'


def modules(x: int, y: int, columns: int, rows: int, pitch: int = 28) -> str:
    return ''.join(f'<rect class="module" x="{x + column * pitch}" y="{y + row * pitch}" width="16" height="16"/>' for row in range(rows) for column in range(columns))


def build() -> None:
    # Four connected stages, clockwise. Every stage remains legible without motion;
    # the timed accents and directional packets share one CSS animation cycle.
    hero = '''<g class="loop-stage" data-loop-stage="propose" style="--stage:0">
<path class="loop-rule" d="M34 20h40"/><path class="loop-stage-accent" d="M34 20h40"/>
<text x="34" y="44" class="loop-eyebrow">01 / Your agent</text><text x="34" y="78" class="loop-title">Propose a change.</text>
<g class="loop-pictogram" transform="translate(150 178) scale(.88) translate(-140 -158)"><path class="loop-glyph" d="M76 106h82l24 24v66H76zM158 106v24h24M94 146h36M94 170h36M152 151v24M140 163h24"/></g>
</g>
<g class="loop-stage" data-loop-stage="impact" style="--stage:1">
<path class="loop-rule" d="M378 20h40"/><path class="loop-stage-accent" d="M378 20h40"/>
<text x="378" y="44" class="loop-eyebrow">02 / Lemma</text><text x="378" y="78" class="loop-title">Trace its impact.</text>
<g class="loop-pictogram" transform="translate(510 178) scale(.88) translate(-460 -158)"><path class="loop-glyph" d="M402 150h24l30-30h32M426 150l30 30h32M524 120h26v30h-26M524 180h26v-30"/>
<rect x="366" y="132" width="36" height="36" class="loop-source"/><rect x="488" y="102" width="36" height="36" class="module"/><rect x="488" y="162" width="36" height="36" class="module"/>
<path class="context-line" d="M373 140h22M373 148h14M495 110h22M495 118h14M495 170h22M495 178h14"/></g>
</g>
<g class="loop-stage" data-loop-stage="verify" style="--stage:2">
<path class="loop-rule" d="M378 481h40"/><path class="loop-stage-accent" d="M378 481h40"/>
<text x="378" y="505" class="loop-eyebrow">03 / Lemma</text><text x="378" y="539" class="loop-title">Verify requirements.</text>
<g class="loop-pictogram" transform="translate(510 386) scale(.88) translate(-140 -470)"><path class="loop-glyph" d="M94 434h90M94 466h64M94 498h90M52 433l8 8 16-16M52 465l8 8 16-16M52 497h24"/></g>
</g>
<g class="loop-stage" data-loop-stage="simulate" style="--stage:3">
<path class="loop-rule" d="M34 481h40"/><path class="loop-stage-accent" d="M34 481h40"/>
<text x="34" y="505" class="loop-eyebrow">04 / Lemma</text><text x="34" y="539" class="loop-title">Simulate behavior.</text>
<g class="loop-pictogram" transform="translate(150 386) scale(-.88 .88) translate(-460 -470)"><path class="loop-glyph" d="M370 470h32l30-30h66M402 470h96M402 470l30 30h66"/>
<path class="loop-glyph" d="m486 430 12 10-12 10m0 10 12 10-12 10m0 10 12 10-12 10"/>
<rect x="510" y="432" width="16" height="16" class="module"/><rect x="510" y="462" width="16" height="16" class="module"/><rect x="510" y="492" width="16" height="16" class="module"/></g>
</g>
<g class="loop-connectors">
<path class="loop-route" d="M245 178h159m-14-12 14 12-14 12M510 256v52m-12-14 12 14 12-14M409 386H250m14-12-14 12 14 12M150 308v-52m-12 14 12-14 12 14"/>
<path class="loop-arrowheads" d="M390 166l14 12-14 12M498 294l12 14 12-14M264 374l-14 12 14 12M138 270l12-14 12 14"/>
<path class="signal loop-signal" data-loop-edge="propose-impact" style="--stage:0" pathLength="100" d="M245 178h159"/>
<path class="signal loop-signal" data-loop-edge="impact-verify" style="--stage:1" pathLength="100" d="M510 256v52"/>
<path class="signal loop-signal" data-loop-edge="verify-simulate" style="--stage:2" pathLength="100" d="M409 386H250"/>
<path class="signal loop-signal" data-loop-edge="simulate-propose" style="--stage:3" pathLength="100" d="M150 308v-52"/>

</g>
<g class="loop-brand" transform="translate(330 282) scale(1.4) translate(-300 -316)"><path d="M278 320v-24h24M298 336h24v-24M290 323l20-20" stroke="currentColor" stroke-width="4.8"/></g>'''
    (ROOT / 'src/hero.svg').write_text(svg(
        hero, 'hero-flow', 'The agent development loop', '0 0 680 566',
        'Clockwise: 1. Your agent proposes a code change. 2. Lemma traces its impact across services and repositories. 3. Lemma verifies the requirements. 4. Lemma simulates declared behavior scenarios and returns evidence and findings to the agent for the next change. Conceptual illustration; evidence applies to the checked inputs and revision.',
    ))

    build_change_story(ROOT, svg)

    texture = (ROOT / 'design-system/assets/matte-metal.svg').read_text()
    texture_body = texture.split('>', 1)[1].rsplit('</svg>', 1)[0]
    social = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630" role="img" aria-label="Lemma. Move fast. See what breaks. Contract checks, impact analysis, and behavior simulations."><defs><pattern id="social-metal" width="352" height="352" patternUnits="userSpaceOnUse">{texture_body}</pattern></defs><rect width="1200" height="630" fill="#A3B0B5"/><rect width="1200" height="630" fill="url(#social-metal)" style="mix-blend-mode:soft-light"/><rect x="860" width="340" height="630" fill="#D3D5D0"/><path d="M56 80V56h24M75 95h24V71M68 86l18-18" fill="none" stroke="#182521" stroke-width="5"/><g fill="#182521" font-family="Geist,Arial,Helvetica,sans-serif"><text x="116" y="88" font-size="40" letter-spacing="-2">lemma</text><text x="56" y="268" font-size="96" letter-spacing="-5">Move fast.</text><text x="56" y="370" font-size="96" letter-spacing="-5">See what breaks.</text><text x="56" y="520" font-size="22">Contract checks · Cross-repo impact</text><text x="56" y="554" font-size="22">Behavior simulations for agent-driven development</text></g><path d="m925 356 210-210M925 146h210v210" fill="none" stroke="#182521" stroke-width="16"/><path d="M56 402h700" stroke="#B99A86" stroke-width="6"/></svg>'''
    colors = json.loads((ROOT / 'design-system/tokens.json').read_text())['color']
    for original, name in {'#A3B0B5':'steel', '#D3D5D0':'silver', '#182521':'ink', '#B99A86':'copper'}.items():
        social = social.replace(original, colors[name])
    (ROOT / 'assets/social-card.svg').write_text(social)
    print('Built hero, workflow, verification, and sharing SVGs.')


if __name__ == '__main__':
    build()
