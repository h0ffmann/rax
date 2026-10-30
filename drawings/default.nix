# The README drawing, 代理 in a small cosmos, in pure Nix: `svg`, `desmos` and `checkSvg` are the
# files' text, `failures` the tests that failed (lib.runTests), empty when all pass.
{ lib }:
let
  pen = import ./pen.nix { inherit lib; };
  cosmos = import ./dairi.nix { inherit lib pen; };
  near = a: b: (if a < b then b - a else a - b) < 1.0e-9;
  bz = [ [ 0.0 0.0 ] [ 10.0 30.0 ] [ 40.0 (-10.0) ] [ 50.0 20.0 ] ];
  inherit (builtins) elemAt length;
  first = s: elemAt s 0;
  last = s: elemAt s (length s - 1);
  dai = elemAt cosmos.strokes.dai;
  ri = elemAt cosmos.strokes.ri;
  col = k: p: elemAt p k;
in
{
  inherit (cosmos.drawing) svg desmos checkSvg;

  failures = lib.runTests {
    testNumbersAreShort = {
      expr = map pen.num [ 12.0 (-3.5) 0.1234 (-0.0001) ];
      expected = [ "12" "-3.5" "0.123" "0" ];
    };
    testAPolynomialReadsBack = {
      expr = let s = pen.poly [ 12.5 (-3.0) 0.0 0.25 ]; in [ s (pen.parsePoly s) ];
      expected = [ "12.5-3t+0.25t^{3}" [ 12.5 (-3.0) 0.0 0.25 ] ];
    };
    testAPartIsTheSameCurve = {
      expr = builtins.all
        (k:
          let
            s = k / 4.0;
            p = pen.at (pen.part bz 0.25 0.75) s;
            q = pen.at bz (0.5 * s + 0.25);
          in
          near (col 0 p) (col 0 q) && near (col 1 p) (col 1 q))
        (lib.range 0 4);
      expected = true;
    };
    testThePowerFormIsTheBezier = {
      # Midpoint of a cubic Bézier: (p0 + 3p1 + 3p2 + p3) / 8.
      expr = let cx = elemAt (pen.coeffs bz) 0; in
        near (pen.evalPoly cx 0.5) 25.0 && near (pen.evalPoly cx 1.0) 50.0;
      expected = true;
    };
    testDaiHasItsFiveStrokesInOrder = {
      expr = [
        (length cosmos.strokes.dai)
        # 亻 first: the falling stroke sweeps down to the left, then the vertical stands under it
        (col 0 (last (dai 0)) < col 0 (first (dai 0)))
        (col 1 (first (dai 1)) > col 1 (first (dai 0)))
        # the horizontal of 弋 rises to the right
        (col 1 (last (dai 2)) < col 1 (first (dai 2)))
        # the long stroke ends low on the right, lifted above its lowest point: the hook
        (col 0 (last (dai 3)) > col 0 (first (dai 3))
          && col 1 (last (dai 3)) < lib.foldl' lib.max 0.0 (map (col 1) (dai 3)))
        # the dot is last, up on the right
        (col 0 (first (dai 4)) > col 0 (first (dai 3)) && col 1 (last (dai 4)) < col 1 (first (dai 2)))
      ];
      expected = [ 5 true true true true true ];
    };
    testRiHasItsElevenStrokesInOrder = {
      expr = [
        (length cosmos.strokes.ri)
        # 王 before 里: its four strokes all on the left
        (builtins.all (s: lib.foldl' lib.max 0.0 (map (col 0) s) < 42.0) (lib.take 4 cosmos.strokes.ri))
        # the last stroke of 王 rises to the right
        (col 1 (last (ri 3)) < col 1 (first (ri 3)))
        # the box opens with its left side, then its top turned down the right side
        (col 0 (last (ri 5)) > col 0 (first (ri 4)) && col 1 (last (ri 5)) > col 1 (first (ri 5)) + 25.0)
        # the long bottom horizontal closes the character, lowest and widest of 里
        (col 1 (first (ri 10)) > lib.foldl' lib.max 0.0 (map (col 1) (lib.concatLists (lib.take 10 cosmos.strokes.ri))))
      ];
      expected = [ 11 true true true true ];
    };
    testTheOrbitIsClosed = {
      expr = let q = cosmos.quarters; in
        builtins.all
          (k: near (col 0 (last (elemAt q k))) (col 0 (first (elemAt q (lib.mod (k + 1) 4))))
            && near (col 1 (last (elemAt q k))) (col 1 (first (elemAt q (lib.mod (k + 1) 4)))))
          (lib.range 0 3);
      expected = true;
    };
    # Every Desmos expression, read back from its LaTeX, stays within 0.01 of what it stands for.
    testTheDesmosFileIsTheDrawing = {
      expr = cosmos.drawing.worstSquared < 0.01 * 0.01;
      expected = true;
    };
  };
}
