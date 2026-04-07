/**
 * Pixel-art mouse-pointer for sessions originating from Cursor Agent.
 * Sized and tinted with the same `size` / `color` API as `OwlIcon` so
 * `SessionList` can swap between them without any other layout change.
 *
 * 10x10 grid; `1` is the body, `0` is transparent. Classic top-left
 * arrow with a "select finger" tail in the lower right so it reads as
 * a cursor at small sizes (~22-26px).
 */

const PIXELS: ReadonlyArray<ReadonlyArray<number>> = [
  [1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
  [1, 1, 0, 0, 0, 0, 0, 0, 0, 0],
  [1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
  [1, 1, 1, 1, 0, 0, 0, 0, 0, 0],
  [1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
  [1, 1, 1, 1, 1, 1, 0, 0, 0, 0],
  [1, 1, 1, 1, 1, 1, 1, 0, 0, 0],
  [1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
  [1, 1, 0, 1, 1, 1, 0, 0, 0, 0],
  [0, 0, 0, 0, 1, 1, 0, 0, 0, 0],
];

const COLS = PIXELS[0].length;
const ROWS = PIXELS.length;

interface CursorIconProps {
  size?: number;
  color?: string;
  className?: string;
}

export function CursorIcon({
  size = 22,
  color = "#e8743c",
  className,
}: CursorIconProps) {
  const cells: React.ReactNode[] = [];
  for (let y = 0; y < ROWS; y++) {
    for (let x = 0; x < COLS; x++) {
      if (PIXELS[y][x] === 0) continue;
      cells.push(
        <rect
          key={`${x}-${y}`}
          x={x}
          y={y}
          width={1.02}
          height={1.02}
          fill={color}
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
