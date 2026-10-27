import assert from 'node:assert/strict'
import test from 'node:test'

import type { DyeColor } from './colors.ts'
import { describeColorUsage, replaceBannerColors } from './recolor.ts'
import { type BannerDesign, createBannerLayer } from './types.ts'

function design(
    baseColor: DyeColor,
    layers: [Parameters<typeof createBannerLayer>[0], DyeColor][],
) {
    return {
        baseColor,
        layers: layers.map(([pattern, color]) => createBannerLayer(pattern, color)),
    } satisfies BannerDesign
}

test('color usage lists each painted color once, in dye order', () => {
    const usage = describeColorUsage(
        design('white', [
            ['stripe_center', 'red'],
            ['cross', 'white'],
            ['border', 'red'],
        ]),
    )

    assert.deepEqual(usage, [
        { color: 'white', layers: 1, base: true },
        { color: 'red', layers: 2, base: false },
    ])
})

test('color usage reports a plain banner as its base color only', () => {
    assert.deepEqual(describeColorUsage(design('blue', [])), [
        { color: 'blue', layers: 0, base: true },
    ])
})

test('replacing a color rewrites every layer painted in it', () => {
    const before = design('white', [
        ['stripe_center', 'red'],
        ['cross', 'blue'],
        ['border', 'red'],
    ])

    const after = replaceBannerColors(before, { colors: ['red'], target: 'green' })

    assert.deepEqual(
        after.layers.map((layer) => layer.color),
        ['green', 'blue', 'green'],
    )
    assert.equal(after.baseColor, 'white')
})

test('replacing several colors at once leaves the others untouched', () => {
    const before = design('white', [
        ['stripe_center', 'red'],
        ['cross', 'blue'],
        ['border', 'black'],
    ])

    const after = replaceBannerColors(before, { colors: ['red', 'blue'], target: 'lime' })

    assert.deepEqual(
        after.layers.map((layer) => layer.color),
        ['lime', 'lime', 'black'],
    )
})

test('the base follows a replacement unless it is excluded', () => {
    const before = design('white', [['stripe_center', 'white']])

    const included = replaceBannerColors(before, { colors: ['white'], target: 'black' })
    assert.equal(included.baseColor, 'black')
    assert.equal(included.layers[0].color, 'black')

    const excluded = replaceBannerColors(before, {
        colors: ['white'],
        target: 'black',
        includeBase: false,
    })
    assert.equal(excluded.baseColor, 'white')
    assert.equal(excluded.layers[0].color, 'black')
})

test('a base color that is not being replaced keeps its color', () => {
    const before = design('yellow', [['stripe_center', 'red']])

    const after = replaceBannerColors(before, { colors: ['red'], target: 'black' })

    assert.equal(after.baseColor, 'yellow')
})

test('a replacement that changes nothing returns the original design', () => {
    const before = design('white', [['stripe_center', 'red']])

    assert.equal(replaceBannerColors(before, { colors: ['red'], target: 'red' }), before)
    assert.equal(replaceBannerColors(before, { colors: ['blue'], target: 'red' }), before)
    assert.equal(
        replaceBannerColors(before, { colors: ['white'], target: 'red', includeBase: false }),
        before,
    )
})

test('replacing keeps layer identities so the layer list stays stable', () => {
    const before = design('white', [['stripe_center', 'red']])

    const after = replaceBannerColors(before, { colors: ['red'], target: 'green' })

    assert.equal(after.layers[0].id, before.layers[0].id)
    assert.equal(after.layers[0].pattern, 'stripe_center')
})
