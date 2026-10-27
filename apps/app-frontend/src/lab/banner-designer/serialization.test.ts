import assert from 'node:assert/strict'
import { describe, it } from 'node:test'

import { describeBannerMaterials } from './materials.ts'
import {
    BannerParseError,
    parseBannerText,
    toDesignJson,
    toGiveCommand,
    toItemData,
    toPatternsComponent,
    unsupportedPatterns,
} from './serialization.ts'
import { type BannerDesign, createBannerLayer } from './types.ts'

const design: BannerDesign = {
    baseColor: 'white',
    layers: [createBannerLayer('stripe_bottom', 'red'), createBannerLayer('creeper', 'black')],
}

function codes(run: () => unknown): string {
    try {
        run()
    } catch (error) {
        if (error instanceof BannerParseError) return error.code
        throw error
    }
    return 'none'
}

describe('banner export', () => {
    it('writes the modern component syntax', () => {
        assert.equal(
            toPatternsComponent(design),
            '[{pattern:"minecraft:stripe_bottom",color:"red"},{pattern:"minecraft:creeper",color:"black"}]',
        )
        assert.equal(
            toGiveCommand(design),
            '/give @p minecraft:white_banner[minecraft:banner_patterns=[{pattern:"minecraft:stripe_bottom",color:"red"},{pattern:"minecraft:creeper",color:"black"}]] 1',
        )
    })

    it('omits the component block for a plain banner', () => {
        const plain: BannerDesign = { baseColor: 'blue', layers: [] }
        assert.equal(toItemData(plain), '')
        assert.equal(toGiveCommand(plain), '/give @p minecraft:blue_banner 1')
    })

    it('writes the pre-1.20.5 item NBT with short pattern codes', () => {
        assert.equal(
            toItemData(design, { version: 'legacy' }),
            '{BlockEntityTag:{Base:0b,Patterns:[{Pattern:"bs",Color:14b},{Pattern:"cre",Color:15b}]}}',
        )
        assert.equal(
            toGiveCommand(design, { version: 'legacy', target: '@s', count: 2 }),
            '/give @s minecraft:white_banner{BlockEntityTag:{Base:0b,Patterns:[{Pattern:"bs",Color:14b},{Pattern:"cre",Color:15b}]}} 2',
        )
    })

    it('writes a design file that round trips', () => {
        const parsed = parseBannerText(toDesignJson(design))
        assert.equal(parsed.baseColor, 'white')
        assert.deepEqual(
            parsed.layers.map((layer) => [layer.pattern, layer.color]),
            [
                ['stripe_bottom', 'red'],
                ['creeper', 'black'],
            ],
        )
    })
})

describe('banner export version support', () => {
    it('flags patterns the legacy item data cannot carry', () => {
        const newer: BannerDesign = {
            baseColor: 'white',
            layers: [createBannerLayer('flow', 'red'), createBannerLayer('stripe_top', 'blue')],
        }
        assert.deepEqual(unsupportedPatterns(newer, 'legacy'), ['flow'])
        assert.deepEqual(unsupportedPatterns(newer, 'modern'), [])
    })

    it('keeps the 1.13 pattern set and later 1.16 to 1.20 additions in legacy', () => {
        const older: BannerDesign = {
            baseColor: 'white',
            layers: [
                createBannerLayer('bricks', 'red'),
                createBannerLayer('piglin', 'black'),
                createBannerLayer('globe', 'blue'),
            ],
        }
        assert.deepEqual(unsupportedPatterns(older, 'legacy'), [])
    })

    it('reports each unsupported pattern once', () => {
        const repeated: BannerDesign = {
            baseColor: 'white',
            layers: [
                createBannerLayer('guster', 'red'),
                createBannerLayer('guster', 'blue'),
                createBannerLayer('flow', 'black'),
            ],
        }
        assert.deepEqual(unsupportedPatterns(repeated, 'legacy'), ['guster', 'flow'])
    })

    it('allows 1.16 to 1.20 patterns in legacy but flags the 1.20.5 ones', () => {
        const mixed: BannerDesign = {
            baseColor: 'white',
            layers: [
                createBannerLayer('stripe_top', 'red'),
                createBannerLayer('piglin', 'black'),
                createBannerLayer('globe', 'blue'),
                createBannerLayer('flow', 'white'),
            ],
        }
        assert.deepEqual(unsupportedPatterns(mixed, 'modern'), [])
        assert.deepEqual(unsupportedPatterns(mixed, 'legacy'), ['flow'])
    })

    it('writes one short code per legacy layer', () => {
        const design: BannerDesign = {
            baseColor: 'light_gray',
            layers: [
                createBannerLayer('stripe_top', 'red'),
                createBannerLayer('diagonal_up_right', 'blue'),
            ],
        }
        assert.equal(
            toItemData(design, { version: 'legacy' }),
            '{BlockEntityTag:{Base:8b,Patterns:[{Pattern:"ts",Color:14b},{Pattern:"rd",Color:11b}]}}',
        )
    })
})

describe('banner import', () => {
    it('reads the modern give command', () => {
        const parsed = parseBannerText(
            '/give @p minecraft:red_banner[minecraft:banner_patterns=[{pattern:"minecraft:stripe_top",color:"blue"}]] 1',
        )
        assert.equal(parsed.baseColor, 'red')
        assert.deepEqual(
            parsed.layers.map((layer) => [layer.pattern, layer.color]),
            [['stripe_top', 'blue']],
        )
    })

    it('reads component data without a command', () => {
        const parsed = parseBannerText(
            '[minecraft:banner_patterns=[{pattern:"minecraft:skull",color:"white"}]]',
        )
        assert.equal(parsed.baseColor, 'white')
        assert.equal(parsed.layers[0].pattern, 'skull')
    })

    it('reads legacy item NBT', () => {
        const parsed = parseBannerText(
            '{BlockEntityTag:{Base:14b,Patterns:[{Pattern:"curly_border",Color:0b}]}}',
        )
        assert.equal(parsed.baseColor, 'red')
        assert.equal(parsed.layers[0].pattern, 'curly_border')
        assert.equal(parsed.layers[0].color, 'white')
    })

    it('falls back to the item id when the legacy NBT has no base color', () => {
        const parsed = parseBannerText(
            '/give @p minecraft:lime_banner{BlockEntityTag:{Patterns:[{Pattern:"bricks",Color:4b}]}}',
        )
        assert.equal(parsed.baseColor, 'lime')
        assert.equal(parsed.layers[0].color, 'yellow')
    })

    it('reads the 1.12 give command, which keeps the data tag last', () => {
        const parsed = parseBannerText(
            '/give @p minecraft:banner 1 8 {BlockEntityTag:{Base:8,Patterns:[{Pattern:"ts",Color:14}]}}',
        )
        assert.equal(parsed.baseColor, 'light_gray')
        assert.equal(parsed.layers[0].pattern, 'stripe_top')
        assert.equal(parsed.layers[0].color, 'red')
    })

    it('rejects unknown patterns and colors', () => {
        assert.equal(
            codes(() => parseBannerText('')),
            'empty',
        )
        assert.equal(
            codes(() => parseBannerText('hello')),
            'unsupported',
        )
        assert.equal(
            codes(() =>
                parseBannerText('[minecraft:banner_patterns=[{pattern:"nope",color:"red"}]]'),
            ),
            'invalid-pattern',
        )
        assert.equal(
            codes(() =>
                parseBannerText('[minecraft:banner_patterns=[{pattern:"cross",color:"pink2"}]]'),
            ),
            'invalid-color',
        )
        assert.equal(
            codes(() => parseBannerText('{broken')),
            'invalid-data',
        )
    })

    it('caps imported layers at the survival limit', () => {
        const layers = Array.from(
            { length: 8 },
            (_, index) => `{Pattern:"cross",Color:${index}b}`,
        ).join(',')
        const parsed = parseBannerText(`{BlockEntityTag:{Base:0b,Patterns:[${layers}]}}`)
        assert.equal(parsed.layers.length, 6)
    })
})

describe('banner materials', () => {
    it('counts dyes, pattern items and the banner itself', () => {
        const materials = describeBannerMaterials(design)
        assert.equal(materials.bannerItem, 'minecraft:white_banner')
        assert.deepEqual(materials.dyes, [
            { color: 'red', count: 1, item: 'minecraft:red_dye' },
            { color: 'black', count: 1, item: 'minecraft:black_dye' },
        ])
        assert.deepEqual(materials.patternItems, ['minecraft:creeper_banner_pattern'])
    })

    it('merges repeated layer colors without counting the base color', () => {
        const materials = describeBannerMaterials({
            baseColor: 'red',
            layers: [createBannerLayer('cross', 'red'), createBannerLayer('border', 'red')],
        })
        assert.equal(materials.bannerItem, 'minecraft:red_banner')
        assert.deepEqual(materials.dyes, [{ color: 'red', count: 2, item: 'minecraft:red_dye' }])
        assert.deepEqual(materials.patternItems, [])
    })

    it('lists each pattern item once', () => {
        const materials = describeBannerMaterials({
            baseColor: 'white',
            layers: [
                createBannerLayer('creeper', 'black'),
                createBannerLayer('creeper', 'red'),
                createBannerLayer('globe', 'blue'),
            ],
        })
        assert.deepEqual(materials.patternItems, [
            'minecraft:creeper_banner_pattern',
            'minecraft:globe_banner_pattern',
        ])
    })
})
