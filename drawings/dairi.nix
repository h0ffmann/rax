# The RAX drawing: 代理 (*dairi*, acting in another's place: an agent, a proxy) brushed in its
# sixteen strokes, in the standard order, in a small cosmos: stars, and an orbit round the two
# characters with three bodies on it, agents going round their runtime.
#
# 代理 is the Japanese word for an agent who acts for someone (代理人, *dairinin*, is the
# person), and the word RAX's tasks live up to: each one is sent to act in its user's place.
# 代 is "to stand in for", 理 "reason, to put in order".
#
# The kanji are drawn as piabrota/nix-os draws its 无: each stroke is a brush line through
# hand-placed points with a width at each point. Horizontals and verticals stop firmly (*tome*),
# falling strokes sweep out to a point (*harai*), hooks lift (*hane*). The stroke order follows
# KanjiVG; the points are drawn here.
{ lib, pen }:
let
  inherit (builtins) elemAt;

  # The five strokes of 代, in writing order, in a 0–100 box: [x y brush width at that point].
  dai = [
    # 1 ㇒ the falling stroke of 亻, swept out to the lower left
    [
      [ 29.8 22.5 6.6 ]
      [ 29.7 26.2 6.4 ]
      [ 28.4 31.0 5.8 ]
      [ 24.0 41.5 4.6 ]
      [ 17.5 52.5 3.0 ]
      [ 10.5 61.5 0.8 ]
    ]
    # 2 ㇑ the vertical of 亻
    [
      [ 24.4 45.0 6.6 ]
      [ 25.9 48.6 5.6 ]
      [ 26.1 62.0 4.6 ]
      [ 26.1 82.0 4.4 ]
      [ 26.1 94.0 5.4 ]
      [ 25.8 96.0 6.0 ]
    ]
    # 3 ㇐ the horizontal of 弋, rising to the right
    [
      [ 39.5 44.1 6.0 ]
      [ 43.5 44.4 4.8 ]
      [ 55.0 42.2 3.4 ]
      [ 72.0 37.6 3.2 ]
      [ 84.0 35.2 3.6 ]
      [ 88.4 35.0 5.4 ]
      [ 89.6 36.3 6.0 ]
    ]
    # 4 ㇂ the long curved stroke of 弋, down to the right, lifted in a hook at the bottom
    [
      [ 53.2 13.5 6.8 ]
      [ 55.6 16.5 6.2 ]
      [ 56.6 22.0 5.6 ]
      [ 60.0 45.0 4.8 ]
      [ 67.0 65.0 4.4 ]
      [ 77.0 81.0 4.0 ]
      [ 90.0 92.0 3.8 ]
      [ 95.8 92.8 4.4 ]
      [ 97.6 89.5 4.2 ]
      [ 97.3 85.5 1.6 ]
    ]
    # 5 ㇔ the dot of 弋
    [
      [ 71.8 15.4 4.0 ]
      [ 76.0 17.4 5.6 ]
      [ 80.2 20.6 6.8 ]
      [ 82.3 22.9 6.2 ]
    ]
  ];

  # The eleven strokes of 理, in writing order, in the same box.
  ri = [
    # 1 ㇐ the top horizontal of 王
    [
      [ 11.8 27.8 5.6 ]
      [ 15.0 28.1 4.4 ]
      [ 27.0 26.5 3.2 ]
      [ 36.3 24.9 3.4 ]
      [ 39.4 24.7 5.0 ]
      [ 40.4 25.9 5.6 ]
    ]
    # 2 ㇑ the vertical of 王
    [
      [ 26.3 30.3 6.0 ]
      [ 27.6 32.6 5.2 ]
      [ 28.0 50.0 4.4 ]
      [ 28.0 70.0 4.4 ]
      [ 28.0 77.0 5.0 ]
    ]
    # 3 ㇐ the middle horizontal of 王
    [
      [ 15.3 52.1 5.2 ]
      [ 19.5 52.3 4.0 ]
      [ 30.0 50.2 3.0 ]
      [ 36.3 48.4 3.2 ]
      [ 39.4 48.0 4.8 ]
    ]
    # 4 ㇀ the bottom of 王, rising to the right, swept out
    [
      [ 12.8 82.8 6.0 ]
      [ 15.0 83.6 5.6 ]
      [ 18.0 82.9 5.0 ]
      [ 28.0 78.4 3.4 ]
      [ 39.0 72.8 1.0 ]
    ]
    # 5 ㇑ the left side of 里's box
    [
      [ 46.0 18.5 6.0 ]
      [ 47.8 21.6 5.2 ]
      [ 49.6 33.0 4.2 ]
      [ 51.0 46.0 4.0 ]
      [ 51.8 54.6 5.0 ]
    ]
    # 6 ㇕ the top and the right side of the box, in one stroke turned at the corner
    [
      [ 48.8 20.8 5.0 ]
      [ 62.0 18.6 3.4 ]
      [ 80.0 16.1 3.2 ]
      [ 88.5 15.3 3.8 ]
      [ 92.2 16.4 5.6 ]
      [ 92.6 19.6 5.2 ]
      [ 91.2 32.0 4.2 ]
      [ 89.6 45.0 4.0 ]
      [ 88.7 52.0 4.6 ]
    ]
    # 7 ㇐ the inner horizontal of the box
    [
      [ 51.2 35.6 4.2 ]
      [ 60.0 34.7 3.2 ]
      [ 80.0 32.7 3.0 ]
      [ 90.3 32.1 3.4 ]
    ]
    # 8 ㇐ the bottom of the box
    [
      [ 52.7 52.3 4.2 ]
      [ 62.0 51.6 3.2 ]
      [ 80.0 49.9 3.0 ]
      [ 87.9 49.3 3.4 ]
    ]
    # 9 ㇑ the long vertical of 里, through the box
    [
      [ 67.8 20.8 6.0 ]
      [ 69.0 23.0 5.2 ]
      [ 69.3 45.0 4.4 ]
      [ 69.3 75.0 4.4 ]
      [ 69.3 87.8 5.0 ]
    ]
    # 10 ㇐ the short horizontal under the box
    [
      [ 52.2 69.3 5.2 ]
      [ 56.5 69.7 4.0 ]
      [ 70.0 68.2 3.0 ]
      [ 82.0 66.6 3.2 ]
      [ 87.0 66.3 4.6 ]
      [ 88.2 67.4 5.2 ]
    ]
    # 11 ㇐ the long bottom horizontal
    [
      [ 40.1 91.0 6.2 ]
      [ 45.0 91.6 4.8 ]
      [ 62.0 89.8 3.4 ]
      [ 85.0 87.5 3.2 ]
      [ 96.0 87.6 5.0 ]
      [ 98.6 88.0 5.6 ]
      [ 99.4 89.4 6.2 ]
    ]
  ];

  # The canvas, wide, with the two kanji side by side: each 0–100 box scaled by `scale` about its
  # point (54, 55), put at `centre`. The widths of the strokes are times `weight`.
  w = 520.0;
  h = 300.0;
  scale = 1.86;
  weight = 1.9;
  place = centre: p: [
    (pen.fma (elemAt p 0 - 54.0) scale (elemAt centre 0))
    (pen.fma (elemAt p 1 - 55.0) scale (elemAt centre 1))
  ];
  kanji = centre: strokes: lib.concatMap
    (s: pen.brush (map (place centre) s) (map (p: elemAt p 2 * weight) s))
    strokes;

  # The orbit: an ellipse of half axes `rx` × `ry` about the middle of the canvas, tilted by
  # `tilt` (its cosine and sine, as Nix has no trigonometry). It is four quarter circles, each a
  # cubic Bézier (with the usual kappa), stretched and turned, then cut in `parts` so the line can
  # be heavier in front of the kanji (low on the canvas) and lighter behind them.
  orbit = rec {
    cx = 260.0;
    cy = 152.0;
    rx = 254.0;
    ry = 128.0;
    tilt = { cos = 0.997564050; sin = -0.069756474; }; # -4°
    kappa = 0.5522847498;
    parts = 6;
    # A point of the unit circle, on the canvas.
    put = u: [
      (cx + tilt.cos * rx * elemAt u 0 - tilt.sin * ry * elemAt u 1)
      (cy + tilt.sin * rx * elemAt u 0 + tilt.cos * ry * elemAt u 1)
    ];
    # The four quarters of the unit circle, turning from (1, 0) through (0, 1): down, on a canvas.
    quarters = map
      (q: map (p: [ (elemAt q 0 * elemAt p 0 - elemAt q 1 * elemAt p 1) (elemAt q 1 * elemAt p 0 + elemAt q 0 * elemAt p 1) ])
        [ [ 1.0 0.0 ] [ 1.0 kappa ] [ kappa 1.0 ] [ 0.0 1.0 ] ])
      [ [ 1.0 0.0 ] [ 0.0 1.0 ] [ (-1.0) 0.0 ] [ 0.0 (-1.0) ] ];
    # From 0.6 at the back of the orbit to 2.0 at its front.
    widthAt = v: 1.3 + 0.7 * v;
    marks = lib.concatMap
      (q: lib.genList
        (j:
          let
            t0 = j * 1.0 / parts;
            t1 = (j + 1) * 1.0 / parts;
            unit = pen.part q t0 t1;
          in
          {
            bezier = map put unit;
            width = widthAt (elemAt (pen.at q ((t0 + t1) / 2.0)) 1);
          })
        parts)
      quarters;
    # The point of the orbit a share `t` along quarter `k`.
    point = k: t: put (pen.at (elemAt quarters k) t);
  };

  # A bright star: four points of light, two thin crossed strokes that taper at both ends.
  sparkle = c: len: width:
    let
      at = dx: dy: [ (elemAt c 0 + dx) (elemAt c 1 + dy) ];
    in
    pen.brush [ (at (-len) 0.0) c (at len 0.0) ] [ 0.2 width 0.2 ]
    ++ pen.brush [ (at 0.0 (-len)) c (at 0.0 len) ] [ 0.2 width 0.2 ];

  # The stars, placed by hand in the sky round the kanji: [x y radius].
  stars = [
    [ 18.0 22.0 1.0 ]
    [ 46.0 58.0 0.7 ]
    [ 31.0 112.0 1.0 ]
    [ 14.0 178.0 0.8 ]
    [ 40.0 236.0 1.0 ]
    [ 22.0 281.0 0.7 ]
    [ 70.0 18.0 0.7 ]
    [ 95.0 44.0 1.4 ]
    [ 64.0 262.0 1.4 ]
    [ 108.0 286.0 0.8 ]
    [ 128.0 12.0 1.0 ]
    [ 170.0 30.0 0.7 ]
    [ 198.0 292.0 0.8 ]
    [ 160.0 248.0 1.8 ]
    [ 214.0 16.0 1.0 ]
    [ 236.0 44.0 0.7 ]
    [ 258.0 92.0 1.0 ]
    [ 262.0 162.0 0.7 ]
    [ 254.0 214.0 1.2 ]
    [ 244.0 290.0 0.7 ]
    [ 268.0 8.0 0.8 ]
    [ 300.0 24.0 1.0 ]
    [ 286.0 270.0 0.8 ]
    [ 324.0 284.0 1.4 ]
    [ 348.0 10.0 0.7 ]
    [ 394.0 26.0 1.2 ]
    [ 372.0 260.0 0.7 ]
    [ 420.0 278.0 1.0 ]
    [ 442.0 8.0 0.8 ]
    [ 470.0 30.0 1.4 ]
    [ 452.0 250.0 1.6 ]
    [ 480.0 286.0 0.7 ]
    [ 500.0 70.0 1.0 ]
    [ 486.0 118.0 0.7 ]
    [ 502.0 176.0 1.2 ]
    [ 478.0 214.0 0.8 ]
    [ 508.0 240.0 0.7 ]
    [ 6.0 70.0 0.8 ]
  ];

  # The bodies on the orbit: a planet in front, two smaller agents going round with it.
  bodies = [
    { at = orbit.point 0 0.8; r = 6.5; }
    { at = orbit.point 2 0.3; r = 3.4; }
    { at = orbit.point 3 0.55; r = 2.6; }
  ];
in
{
  strokes = { inherit dai ri; };
  inherit (orbit) quarters;

  drawing = pen.drawing {
    inherit w h;
    label = "代理 (dairi, an agent: one who acts in another's place) brushed in its sixteen strokes, among stars, inside an orbit";
    desc = "Drawn from the Desmos expressions of cosmos.desmos.json: cubic Béziers through hand-placed points, each piece at its brush width, and points for the stars.";
    marks = kanji [ 150.0 145.0 ] dai
      ++ kanji [ 372.0 145.0 ] ri
      ++ orbit.marks
      ++ sparkle [ 160.0 248.0 ] 7.0 1.6
      ++ sparkle [ 470.0 30.0 ] 6.0 1.4
      ++ sparkle [ 95.0 44.0 ] 6.0 1.4
      ++ sparkle [ 452.0 250.0 ] 6.5 1.5;
    dots = map (s: { at = [ (elemAt s 0) (elemAt s 1) ]; r = elemAt s 2; }) stars ++ bodies;
  };
}
