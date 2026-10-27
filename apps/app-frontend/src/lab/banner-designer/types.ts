import type { DyeColor } from './colors.ts'
import type { BannerPatternId } from './patterns.ts'

export interface BannerLayer {
    id: string
    pattern: BannerPatternId
    color: DyeColor
}

export interface BannerDesign {
    baseColor: DyeColor
    /** Patterns in application order: later layers paint over earlier ones. */
    layers: BannerLayer[]
}

let layerSequence = 0

export function createBannerLayer(pattern: BannerPatternId, color: DyeColor): BannerLayer {
    layerSequence += 1
    return { id: `banner-layer-${layerSequence}`, pattern, color }
}

export function cloneBannerDesign(design: BannerDesign): BannerDesign {
    return {
        baseColor: design.baseColor,
        layers: design.layers.map((layer) => createBannerLayer(layer.pattern, layer.color)),
    }
}

export function designsMatch(left: BannerDesign, right: BannerDesign): boolean {
    return (
        left.baseColor === right.baseColor &&
        left.layers.length === right.layers.length &&
        left.layers.every(
            (layer, index) =>
                layer.pattern === right.layers[index].pattern &&
                layer.color === right.layers[index].color,
        )
    )
}
