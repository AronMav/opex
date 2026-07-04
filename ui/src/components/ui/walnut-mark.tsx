import * as React from "react"

// Соотношение сторон исходной гравюры (обрезка по контуру, 584x487)
const RATIO = 0.8339
// Поднимайте версию при замене public/walnut-mark.png — nginx кэширует
// статику ~20ч, без этого клиенты видят старую гравюру до истечения кэша.
const MASK_URL = "/walnut-mark.png?v=2"

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
        WebkitMaskImage: `url(${MASK_URL})`,
        maskImage: `url(${MASK_URL})`,
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
