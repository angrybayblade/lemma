"""Generate labeled illustrations for the configured change, with code kept in prose."""
from html import escape
import json


def build_change_story(root, wrap_svg):
    story = json.loads((root / 'assets/change-story.json').read_text())
    (root / 'src/change-story.js').write_text('export const changeStory = ' + json.dumps(story, ensure_ascii=False, indent=2) + ';\n')

    def text(x, y, value, cls='flow-label', extra=''):
        return f'<text x="{x}" y="{y}" class="{cls}" {extra}>{escape(str(value))}</text>'

    replay_detail = 'Illustrative replay through billing, orders and a caller, for the change to orders’ error helper. With the change, orders drops the retry flag; with the fix, it passes it on. Both declared behavior cases are simulated and checked. Repositories supply their own declarations and checker.'
    verify = text(300, 62, 'orders/handlers.py', 'flow-repository', 'text-anchor="middle"')
    verify += '<g class="replay-pictogram"><path class="module" d="M202 140l98-42 98 42-98 42zM202 150l98 42 98-42"/></g>'
    # Columns as diagrams-webgl.js draws them: center, plate width and depth.
    nodes = [(85, 82, 46), (300, 120, 56), (510, 92, 50)]
    verify += ''.join(text(x, 232, label, 'flow-meta', 'text-anchor="middle"') for (x, _, _), label in zip(nodes, ['Billing response', 'Orders handler', 'Caller receives']))

    def corner(node, y, side):
        x, w, h = node
        return x + side * (w + h) / 2 * .83, y + side * (w - h) / 2 * .36 - 4

    def pipe(start, end):
        span = end[0] - start[0]
        return [start, (start[0] + span * .3, start[1]), (start[0] + span * .55, end[1]), end]

    def points(path):
        return 'M' + 'L'.join(f'{x:.1f} {y:.1f}' for x, y in path)

    def arrow(tip):
        return f'M{tip[0] - 12:.1f} {tip[1] - 10:.1f}l12 10-12 10'

    def arrow_left(tip):
        return f'M{tip[0] + 12:.1f} {tip[1] - 10:.1f}l-12 10 12 10'

    def centered(x, y, value, cls):
        return text(x, y, value, cls, 'text-anchor="middle"')

    # The two repositories as diagrams-webgl.js draws them, the same in every scene.
    plate_y, billing_x, web_x = 240, 150, 450

    def plate_corner(x, side):
        return x + side * 63, plate_y + side * 3

    def repository(x, name, role):
        return (f'<g class="workflow-pictogram"><path class="module" d="M{x - 63} {plate_y}l63-27 63 27-63 27z"/></g>'
                + centered(x, plate_y + 62, name, 'flow-repository') + centered(x, plate_y + 86, role, 'flow-meta'))

    def pipe_fallback(path):
        return f'<g class="workflow-pictogram"><path class="route" d="{points(path)}"/></g>'

    end = plate_corner(web_x, -1)
    contract = pipe(plate_corner(billing_x, 1), (end[0] - 4, end[1]))
    gap = ((contract[1][0] + contract[2][0]) / 2, (contract[1][1] + contract[2][1]) / 2)
    impact = ('<g class="config-scene config-scene--impact">' + pipe_fallback(contract)
              + f'<path class="workflow-arrowheads" d="{arrow(contract[-1])}"/>'
              + f'<path class="workflow-mark" d="M{gap[0] - 7:.1f} {gap[1] - 7:.1f}l14 14m0-14-14 14"/>'
              + centered(300, 190, 'POST /v1/charges', 'flow-meta')
              + repository(billing_x, 'billing', 'Changed its contract') + repository(web_x, 'web', 'Call breaks')
              + centered(300, 420, 'No link declared. Read from the code.', 'flow-label') + '</g>')
    # Both repositories feeding one Lemma index, as createSetupMesh draws them.
    repository_y, store_x, store_y, store_run = 130, 300, 370, 99.6
    pipe_y = store_y - 13

    def setup_repository(x, name):
        return (f'<g class="workflow-pictogram"><path class="module" d="M{x - 63} {repository_y}l63-27 63 27-63 27z"/>'
                + f'<path class="route" d="M{x} {repository_y + 27}V{pipe_y}H{store_x + (-1 if x < store_x else 1) * (store_run + 4)}"/></g>'
                + centered(x, 60, name, 'flow-repository') + centered(x, 84, 'mod.lm', 'flow-meta'))

    setup = ('<g class="config-scene config-scene--setup">' + setup_repository(110, 'billing') + setup_repository(490, 'web')
             + f'<g class="workflow-pictogram"><path class="module" d="M{store_x - store_run} {store_y - 27}l{store_run} -43 {store_run} 43-{store_run} 43z"/></g>'
             + f'<path class="workflow-arrowheads" d="{arrow((store_x - store_run - 4, pipe_y))}{arrow_left((store_x + store_run + 4, pipe_y))}"/>'
             + f'<g class="workflow-brand" transform="translate({store_x} {store_y - 27}) scale(1.5 .9) translate(-300 -316)"><path d="M278 320v-24h24M298 336h24v-24M290 323l20-20"/></g>'
             + centered(store_x, 456, 'lemma', 'flow-repository') + centered(store_x, 480, 'One index for the whole workspace', 'flow-meta') + '</g>')
    end = plate_corner(billing_x, 1)
    call = pipe(plate_corner(web_x, -1), (end[0] + 4, end[1]))
    flag_x, flag_y = 300, (call[1][1] + call[2][1]) / 2
    model = ('<g class="config-scene config-scene--model">' + pipe_fallback(call)
             + f'<path class="workflow-arrowheads" d="{arrow_left(call[-1])}"/>'
             + f'<path class="workflow-mark" d="M{flag_x} {flag_y:.1f}v-38"/><path class="workflow-flag" d="M{flag_x} {flag_y - 38:.1f}l24 8-24 8z"/>'
             + centered(300, 186, 'POST /v1/charges · deprecated', 'flow-meta')
             + repository(billing_x, 'billing', 'Serves the operation') + repository(web_x, 'web', 'Still calls it')
             + centered(300, 420, 'Every call to it is flagged', 'flow-label') + '</g>')
    workflow_detail = 'Three illustrations of the quickstart system, repositories billing and web. A change to billing breaks the call in web, which no one declared. Lemma reads both repositories into one index for the workspace. A requirement flags the call from web to a deprecated operation on billing.'
    (root / 'src/workflow.svg').write_text(wrap_svg(impact + setup + model, 'workflow-flow', 'A finding in web, the setup behind it, and a declared requirement.', detail=workflow_detail))

    for index, case in enumerate(story['behavior']['cases']):
        value = str(case).lower()
        y = 288 + index * 120
        billing, orders, caller = nodes
        into_orders = corner(orders, y, -1)
        into_caller = corner(caller, y, -1)
        incoming = pipe(corner(billing, y, 1), (into_orders[0] - 4, into_orders[1]))
        outgoing = pipe(corner(orders, y, 1), (into_caller[0] - 4, into_caller[1]))
        verify += '<g class="replay-pictogram">' + ''.join(f'<rect x="{x - w / 2 * .9:.1f}" y="{y - 30:.1f}" width="{w * .9:.1f}" height="44" class="module"/>' for x, w, _ in nodes) + '</g>'
        labels = [text(billing[0], y + 56, value, 'flow-label', 'text-anchor="middle"'), text(orders[0], y + 56, 'Place order', 'flow-label', 'text-anchor="middle"')]
        verify += ''.join(labels) + text(caller[0], y + 56, value, 'replay-output', f'text-anchor="middle" data-replay-value="{value}"')
        verify += f'<path class="route replay-route" d="{points(incoming)}{arrow(incoming[-1])}{points(outgoing)}{arrow(outgoing[-1])}"/>'
        middle = ((outgoing[1][0] + outgoing[2][0]) / 2, (outgoing[1][1] + outgoing[2][1]) / 2)
        verify += f'<path class="replay-arrowheads" d="{arrow(incoming[-1])}{arrow(outgoing[-1])}"/><path class="replay-break" d="M{middle[0] - 6:.1f} {middle[1] - 6:.1f}l12 12m0-12-12 12"/>'
    verify += text(300, 508, 'orders must pass on billing’s retry flag', 'flow-label', 'text-anchor="middle"')
    (root / 'src/verify.svg').write_text(wrap_svg(verify, 'verify-flow', 'Check that the retry flag reaches the caller in both declared cases.', '0 0 600 540', replay_detail))
