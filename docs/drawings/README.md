# Drawings

## `cosmos.svg`

代理 (*dairi*) in a small cosmos: the only drawing in this repository. The Nix files in
`drawings/` write it in two formats from one geometry; the flake's `cosmos-art` package holds the
output:

| File | Command | What it is |
|---|---|---|
| `cosmos.svg` | `just drawing` | The README drawing |
| `cosmos.desmos.json` | `just drawing` | The same drawing as a Desmos graph state |

The tests run in `nix flake check` (the `drawing` check, `lib.runTests` in `drawings/default.nix`).
They check that the committed files are the output of `cosmos-art`, that both characters have
their strokes in the standard order, that the orbit closes, and that every Desmos expression,
read back from its LaTeX, stays within 0.01 of the line or star it stands for.

**What it shows.** The two characters 代理, brushed in their sixteen strokes in the standard order,
among stars, inside a tilted orbit with three bodies on it: a planet in front and two smaller
ones, agents going round their runtime. The orbit is heavier in front of the characters and
lighter behind them.

- 代 (*dai*, 5 strokes), "to stand in for": the falling stroke and the vertical of 亻, then 弋: its
  rising horizontal, the long curved stroke that ends in a hook, and the dot.
- 理 (*ri*, 11 strokes), "reason, to put in order": the four strokes of 王, the last one rising and
  swept out, then 里: the box (left side, then top and right side in one turned stroke, the two
  inner horizontals), the long vertical, and the two horizontals under it.

**Why 代理.** It is the Japanese word for an agent in the old sense, one who acts in someone's
place (代理人, *dairinin*, is the person; 代理店 an agency). RAX's tasks are agents in that sense:
each is sent to act for its user. 代 alone means "to stand in for", and 理, "reason", is the
reasoning the agent brings.

**How it is drawn.** As piabrota/nix-os draws its 无, and as pirsig draws its 無: each stroke is a
list of hand-placed points with a brush width at each. A Catmull-Rom spline runs through them; each
piece is a cubic Bézier and each Bézier is one Desmos parametric expression,
`(a+bt+ct^{2}+dt^{3}, …)` for `0 ≤ t ≤ 1`, cut in four so the width changes smoothly. The pen is
`drawings/pen.nix`, nix-os's pen with one addition, dots, for the stars (a Desmos point each); the
drawing is `drawings/dairi.nix`. Both are pure Nix, evaluated without a build. The orbit is four
quarter circles (cubic Béziers with the usual kappa), stretched into an ellipse and tilted by −4°;
the bright stars are two crossed strokes that taper at both ends.

**The look is [picasso](https://github.com/h0ffmann/picasso)'s**: one ink, `#1f2328` on light pages
and `#e6edf3` on dark ones (`prefers-color-scheme`), round ends, nothing else. To see the Desmos
expressions over the geometry, run `nix build .#cosmos-art` and open `result/cosmos-check.svg`:
the geometry in blue, each expression sampled in orange inside it.

### Open the Desmos file

The file is a Desmos graph state (`version` 11). Desmos uses y up, so each y value is `300 - y` of
the SVG (the canvas is 520 × 300).

1. Open `https://www.desmos.com/calculator`.
2. Open the browser console.
3. Run `Calc.setState(<the content of cosmos.desmos.json>)`.

⚠ Not checked in Desmos itself; the check is local, as in nix-os and pirsig.

### Credits

- The stroke order of 代 and 理 follows [KanjiVG](https://kanjivg.tagaini.net/) (`kanji/04ee3.svg`
  and `kanji/07406.svg`, © Ulrich Apel, CC BY-SA 3.0), used as a reference: the points and widths
  here are placed by hand, and no KanjiVG path is copied.
- The pen and the method are piabrota/nix-os's (`drawings/pen.nix`), itself pirsig's
  (`crates/pirsig-drawings`, at `450e4da`) ported to Nix; the SVG-plus-Desmos check comes from
  there through piabrota/alerts.
- The look follows [picasso](https://github.com/h0ffmann/picasso).
