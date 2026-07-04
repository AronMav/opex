import * as React from "react"

// Соотношение сторон исходной гравюры (обрезка по контуру, 584x487)
const RATIO = 0.8339

/**
 * Фирменный знак — гравюрный грецкий орех (растровая маска из референса).
 * Красится через currentColor (CSS mask), поэтому автоматически подстраивается
 * под тему: задайте цвет классом (напр. text-primary), размер — пропом size.
 */
function WalnutMark({
  size = 20,
  className,
}: {
  size?: number
  className?: string
}) {
  return (
    <span
      aria-hidden="true"
      data-slot="walnut-mark"
      className={className}
      style={{
        display: "inline-block",
        flexShrink: 0,
        width: size,
        height: Math.round(size * RATIO),
        backgroundColor: "currentColor",
        WebkitMaskImage: "url(/walnut-mark.png)",
        maskImage: "url(/walnut-mark.png)",
        WebkitMaskSize: "contain",
        maskSize: "contain",
        WebkitMaskRepeat: "no-repeat",
        maskRepeat: "no-repeat",
        WebkitMaskPosition: "center",
        maskPosition: "center",
      }}
    />
  )
}

export { WalnutMark }
