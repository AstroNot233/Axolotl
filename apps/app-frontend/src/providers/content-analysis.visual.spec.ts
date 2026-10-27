import { expect, it, vi } from 'vitest'

import {
    resourceAnalysisClose,
    resourceAnalysisOpen,
    resourceAnalysisUpdate,
    type ResourceOperationPlan,
    type ResourceOperationProgress,
    resourcePrepareOperation,
    type ResourceSelection,
} from '@/helpers/resources'

import { createContentAnalysis } from './content-analysis'

vi.mock('@/helpers/resources', () => ({
    resourceAnalysisOpen: vi.fn().mockResolvedValue('session'),
    resourceAnalysisClose: vi.fn().mockResolvedValue(undefined),
    resourceAnalysisUpdate: vi.fn(),
    resourcePrepareOperation: vi.fn(),
}))

const plan = { id: 'prepared', nodes: [] } as unknown as ResourceOperationPlan
function input(project = 'first', instanceId = 'instance') {
    return {
        instanceId,
        context: '1.21.1/neoforge',
        installDependencies: true,
        selections: [
            {
                id: project,
                resource: { kind: 'source', project: { source: 'modrinth', projectId: project } },
                release: { kind: 'automatic' },
                resourceType: 'mod',
                scope: { kind: 'instance' },
            },
        ] as ResourceSelection[],
    }
}
const tick = async () => {
    await Promise.resolve()
    await Promise.resolve()
}

it('starts on addition and review reuses the running analysis and its progress', async () => {
    vi.clearAllMocks()
    let finish!: (value: ResourceOperationPlan) => void
    let report!: (value: ResourceOperationProgress) => void
    vi.mocked(resourceAnalysisUpdate).mockImplementationOnce((...args) => {
        report = args[5]
        return new Promise((resolve) => {
            finish = resolve
        })
    })
    vi.mocked(resourcePrepareOperation).mockResolvedValue(plan)
    const cart = createContentAnalysis()
    const value = input()
    cart.update(value)
    await tick()
    expect(resourceAnalysisUpdate).toHaveBeenCalledTimes(1)
    const progress = { node: { id: 'first' } } as ResourceOperationProgress
    report(progress)
    const onProgress = vi.fn()
    const prepared = cart.review()!.prepare(new AbortController().signal, onProgress)
    await tick()
    expect(onProgress).toHaveBeenCalledWith(progress)
    expect(resourcePrepareOperation).not.toHaveBeenCalled()
    finish(plan)
    await expect(prepared).resolves.toBe(plan)
    expect(resourceAnalysisUpdate).toHaveBeenCalledTimes(1)
    expect(resourcePrepareOperation).toHaveBeenCalledWith(
        'instance',
        'install',
        value.selections,
        true,
        expect.any(AbortSignal),
        undefined,
        undefined,
        onProgress,
        true,
        { sessionId: 'session', selectionRevision: 1 },
    )
    cart.clear()
})

it('changing selections cancels stale work and keeps the same session for incremental analysis', async () => {
    vi.clearAllMocks()
    vi.mocked(resourceAnalysisUpdate).mockResolvedValue(plan)
    const cart = createContentAnalysis()
    cart.update(input())
    await tick()
    const obsolete = cart.review()!
    const signal = vi.mocked(resourceAnalysisUpdate).mock.calls[0][4]
    cart.update(input('second'))
    await tick()
    expect(signal.aborted).toBe(true)
    await expect(obsolete.prepare(new AbortController().signal, vi.fn())).rejects.toThrow()
    expect(resourcePrepareOperation).not.toHaveBeenCalled()
    expect(resourceAnalysisOpen).toHaveBeenCalledTimes(1)
    expect(vi.mocked(resourceAnalysisUpdate).mock.calls[1][1]).toBe(2)
    cart.clear()
})

it('closing a review stops waiting without canceling the cart, and reopening reuses it', async () => {
    vi.clearAllMocks()
    let finish!: (value: ResourceOperationPlan) => void
    vi.mocked(resourceAnalysisUpdate).mockImplementationOnce(
        () =>
            new Promise((resolve) => {
                finish = resolve
            }),
    )
    vi.mocked(resourcePrepareOperation).mockResolvedValue(plan)
    const cart = createContentAnalysis()
    cart.update(input())
    await tick()
    const controller = new AbortController()
    const waiting = cart.review()!.prepare(controller.signal, vi.fn())
    controller.abort()
    await expect(waiting).rejects.toThrow()
    expect(vi.mocked(resourceAnalysisUpdate).mock.calls[0][4].aborted).toBe(false)
    finish(plan)
    await expect(cart.review()!.prepare(new AbortController().signal, vi.fn())).resolves.toBe(plan)
    expect(resourceAnalysisUpdate).toHaveBeenCalledTimes(1)
    cart.clear()
})

it('clearing or switching instance closes even an opening session and cannot promote late results', async () => {
    vi.clearAllMocks()
    let finish!: (value: string) => void
    vi.mocked(resourceAnalysisOpen).mockImplementationOnce(
        () =>
            new Promise((resolve) => {
                finish = resolve
            }),
    )
    vi.mocked(resourceAnalysisUpdate).mockResolvedValue(plan)
    const cart = createContentAnalysis()
    cart.update(input())
    const obsolete = cart.review()!
    cart.update(input('second', 'another'))
    finish('old-session')
    await tick()
    await expect(obsolete.prepare(new AbortController().signal, vi.fn())).rejects.toThrow()
    expect(resourceAnalysisClose).toHaveBeenCalledWith('old-session')
    expect(resourcePrepareOperation).not.toHaveBeenCalled()
    expect(
        vi.mocked(resourceAnalysisUpdate).mock.calls.every((call) => call[2][0].id === 'second'),
    ).toBe(true)
    cart.clear()
})

it('a failed background request is retried when opening the preview without dropping selections', async () => {
    vi.clearAllMocks()
    vi.mocked(resourceAnalysisUpdate).mockRejectedValueOnce(new Error('offline'))
    vi.mocked(resourcePrepareOperation).mockResolvedValue(plan)
    const cart = createContentAnalysis()
    const value = input()
    cart.update(value)
    await tick()
    await expect(cart.review()!.prepare(new AbortController().signal, vi.fn())).resolves.toBe(plan)
    expect(vi.mocked(resourcePrepareOperation).mock.calls[0][2]).toEqual(value.selections)
    cart.clear()
})

it('canceling stops the cart analysis, closes its session and does not restart the same selection', async () => {
    vi.clearAllMocks()
    vi.mocked(resourceAnalysisUpdate).mockImplementation(() => new Promise(() => {}))
    const cart = createContentAnalysis()
    cart.update(input())
    await tick()
    const review = cart.review()!
    const waiting = review.prepare(new AbortController().signal, vi.fn())
    review.cancel()
    await expect(waiting).rejects.toThrow()
    expect(review.signal.aborted).toBe(true)
    expect(vi.mocked(resourceAnalysisUpdate).mock.calls[0][4].aborted).toBe(true)
    await tick()
    expect(resourceAnalysisClose).toHaveBeenCalledWith('session')
    cart.update({ ...input(), context: 'a download finished' })
    await tick()
    expect(resourceAnalysisUpdate).toHaveBeenCalledTimes(1)
    cart.update(input('second'))
    await tick()
    expect(resourceAnalysisUpdate).toHaveBeenCalledTimes(2)
    cart.clear()
})

it('opening a review after a cancel starts a new analysis of the kept selection', async () => {
    vi.clearAllMocks()
    vi.mocked(resourceAnalysisUpdate)
        .mockImplementationOnce(() => new Promise(() => {}))
        .mockResolvedValueOnce(plan)
    vi.mocked(resourcePrepareOperation).mockResolvedValue(plan)
    const cart = createContentAnalysis()
    const value = input()
    cart.update(value)
    await tick()
    cart.cancel()
    const review = cart.review()!
    expect(review.signal.aborted).toBe(false)
    await expect(review.prepare(new AbortController().signal, vi.fn())).resolves.toBe(plan)
    expect(resourceAnalysisOpen).toHaveBeenCalledTimes(2)
    expect(vi.mocked(resourceAnalysisUpdate).mock.calls[1][2]).toEqual(value.selections)
    cart.clear()
})

it('a finished analysis survives a review cancel', async () => {
    vi.clearAllMocks()
    vi.mocked(resourceAnalysisUpdate).mockResolvedValue(plan)
    const cart = createContentAnalysis()
    cart.update(input())
    await tick()
    await tick()
    const review = cart.review()!
    review.cancel()
    expect(review.signal.aborted).toBe(false)
    expect(resourceAnalysisClose).not.toHaveBeenCalled()
    cart.clear()
})
