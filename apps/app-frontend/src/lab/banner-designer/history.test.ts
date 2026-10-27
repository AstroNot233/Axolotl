import assert from 'node:assert/strict'
import test from 'node:test'

import { emptyHistory, MAX_HISTORY, recordHistory, redoHistory, undoHistory } from './history.ts'
import { type BannerDesign, createBannerLayer } from './types.ts'

function design(baseColor: BannerDesign['baseColor']): BannerDesign {
    return { baseColor, layers: [createBannerLayer('stripe_center', 'black')] }
}

test('undo walks back through every recorded state', () => {
    let history = emptyHistory()
    let current = design('white')

    history = recordHistory(history, current)
    current = design('red')
    history = recordHistory(history, current)
    current = design('blue')

    const first = undoHistory(history, current)
    assert.ok(first)
    assert.equal(first.design.baseColor, 'red')

    const second = undoHistory(first.history, first.design)
    assert.ok(second)
    assert.equal(second.design.baseColor, 'white')

    assert.equal(undoHistory(second.history, second.design), null)
})

test('redo replays the states that were undone', () => {
    let history = emptyHistory()
    history = recordHistory(history, design('white'))
    const undone = undoHistory(history, design('red'))
    assert.ok(undone)

    const redone = redoHistory(undone.history, undone.design)
    assert.ok(redone)
    assert.equal(redone.design.baseColor, 'red')
    assert.equal(redoHistory(redone.history, redone.design), null)
})

test('a new edit drops the redo stack', () => {
    let history = emptyHistory()
    history = recordHistory(history, design('white'))
    const undone = undoHistory(history, design('red'))
    assert.ok(undone)

    const edited = recordHistory(undone.history, design('blue'))
    assert.deepEqual(edited.future, [])
    assert.equal(redoHistory(edited, design('green')), null)
})

test('the history keeps at most MAX_HISTORY snapshots', () => {
    let history = emptyHistory()
    for (let index = 0; index < MAX_HISTORY + 10; index += 1) {
        history = recordHistory(history, design('white'))
    }
    assert.equal(history.past.length, MAX_HISTORY)

    // Undoing everything still stops at the oldest kept snapshot.
    let steps = 0
    let step = undoHistory(history, design('red'))
    while (step) {
        steps += 1
        step = undoHistory(step.history, step.design)
    }
    assert.equal(steps, MAX_HISTORY)
})

test('undo and redo return null when there is nothing to restore', () => {
    const history = emptyHistory()
    assert.equal(undoHistory(history, design('white')), null)
    assert.equal(redoHistory(history, design('white')), null)
})
