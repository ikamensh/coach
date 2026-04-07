/**
 * Pixel-art owl mascot for sessions. Drawn from a 10x9 grid of cells —
 * `1` is the body color, `2` is the eye color, `0` is transparent. Tinted
 * via the `color` prop so callers can dim the idle/away variant.
 */

const PIXELS: ReadonlyArray<ReadonlyArray<number>> = [
  [0, 1, 1, 1, 1, 1, 1, 1, 1, 0],
  [1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
  [1, 1, 2, 2, 1, 1, 2, 2, 1, 1],
  [1, 1, 2, 2, 1, 1, 2, 2, 1, 1],
  [1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
  [1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
  [1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
  [0, 1, 1, 0, 0, 0, 0, 1, 1, 0],
  [0, 1, 0, 0, 0, 0, 0, 0, 1, 0],
];

const COLS = PIXELS[0].length;
const ROWS = PIXELS.length;

interface OwlIconProps {
  size?: number;
  color?: string;
  eyeColor?: string;
  className?: string;
}

export function OwlIcon({
  size = 22,
  color = "#e8743c",
  eyeColor = "#1a1a1f",
  className,
}: OwlIconProps) {
  const cells: React.ReactNode[] = [];
  for (let y = 0; y < ROWS; y++) {
    for (let x = 0; x < COLS; x++) {
      const v = PIXELS[y][x];
      if (v === 0) continue;
      cells.push(
        <rect
          key={`${x}-${y}`}
          x={x}
          y={y}
          width={1.02}
          height={1.02}
          fill={v === 2 ? eyeColor : color}
        />,
      );
    }
  }
  return (
    <svg
      width={size}
      height={(size * ROWS) / COLS}
      viewBox={`0 0 ${COLS} ${ROWS}`}
      shapeRendering="crispEdges"
      className={className}
      aria-hidden="true"
    >
      {cells}
    </svg>
  );
}
