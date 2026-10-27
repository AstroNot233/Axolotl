import assert from 'node:assert/strict'
import test from 'node:test'

import {
    clearRecentBanners,
    loadExportPreference,
    loadRecentBanners,
    recordRecentBanner,
    removeRecentBanner,
    saveExportPreference,
} from './storage.ts'
import { type BannerDesign, createBannerLayer } from './types.ts'

function createStorage() {
    const data = new Map<string, string>()
    return {
        getItem(key: string) {
            return data.get(key) ?? null
        },
        setItem(key: string, value: string) {
            data.set(key, value)
        },
        removeItem(key: string) {
            data.delete(key)
        },
    }
}

function useStorage() {
    Object.defineProperty(globalThis, 'localStorage', {
        value: createStorage(),
        configurable: true,
    })
}

function design(baseColor: BannerDesign['baseColor'], layers = 1): BannerDesign {
    return {
        baseColor,
        layers: Array.from({ length: layers }, () => createBannerLayer('stripe_center', 'black')),
    }
}

test('recent banners deduplicate identical designs and keep the newest first', () => {
    useStorage()
    recordRecentBanner(design('white'))
    recordRecentBanner(design('red'))
    recordRecentBanner(design('white'))

    const records = loadRecentBanners()
    assert.equal(records.length, 2)
    assert.equal(records[0].design.baseColor, 'white')
    assert.equal(records[1].design.baseColor, 'red')
})

test('recent banners retain at most six entries', () => {
    useStorage()
    const colors: BannerDesign['baseColor'][] = [
        'white',
        'orange',
        'magenta',
        'light_blue',
        'yellow',
        'lime',
        'pink',
        'gray',
    ]
    for (const color of colors) recordRecentBanner(design(color))

    const records = loadRecentBanners()
    assert.equal(records.length, 6)
    assert.equal(records[0].design.baseColor, 'gray')
    assert.equal(records[5].design.baseColor, 'magenta')
})

test('recent banners drop records that no longer parse', () => {
    useStorage()
    recordRecentBanner(design('blue'))
    const raw = JSON.parse(localStorage.getItem('axolotl:lab:banner-designer:recent:v1') ?? '[]')
    raw.push({ id: 'broken', savedAt: 1, design: { baseColor: 'chartreuse', layers: [] } })
    raw.push({ id: 'legacy', savedAt: 1, design: { baseColor: 'blue', layers: [{}] } })
    localStorage.setItem('axolotl:lab:banner-designer:recent:v1', JSON.stringify(raw))

    const records = loadRecentBanners()
    assert.equal(records.length, 1)
    assert.equal(records[0].design.baseColor, 'blue')
})

test('recent banners can be removed individually or cleared', () => {
    useStorage()
    recordRecentBanner(design('white'))
    const [record] = recordRecentBanner(design('black'))

    assert.equal(removeRecentBanner(record.id).length, 1)
    assert.equal(loadRecentBanners().length, 1)
    assert.deepEqual(clearRecentBanners(), [])
    assert.equal(loadRecentBanners().length, 0)
})

test('a broken store degrades to an empty list', () => {
    useStorage()
    localStorage.setItem('axolotl:lab:banner-designer:recent:v1', 'not json')
    assert.deepEqual(loadRecentBanners(), [])
})

test('the export preference survives a reload', () => {
    useStorage()
    assert.equal(loadExportPreference(), null)
    saveExportPreference({ format: 'data', version: 'legacy' })
    assert.deepEqual(loadExportPreference(), { format: 'data', version: 'legacy' })
})

test('an unknown export preference falls back to the defaults', () => {
    useStorage()
    localStorage.setItem(
        'axolotl:lab:banner-designer:export:v1',
        JSON.stringify({ format: 'schematic', version: 'modern' }),
    )
    assert.equal(loadExportPreference(), null)

    localStorage.setItem(
        'axolotl:lab:banner-designer:export:v1',
        JSON.stringify({ format: 'give', version: '1.20.4' }),
    )
    assert.equal(loadExportPreference(), null)

    localStorage.setItem('axolotl:lab:banner-designer:export:v1', 'not json')
    assert.equal(loadExportPreference(), null)
})
