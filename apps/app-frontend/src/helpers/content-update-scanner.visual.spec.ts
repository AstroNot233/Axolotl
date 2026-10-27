import { expect, it, vi } from 'vitest'

import { createContentUpdateScanner } from './content-update-scanner'
import type { InstanceContentData } from './instance-content'
import { resourceCheckUpdates, type ResourceUpdateProgress } from './resources'

vi.mock('./resources', () => ({ resourceCheckUpdates: vi.fn() }))

type CheckResult = Awaited<ReturnType<typeof resourceCheckUpdates>>
const item = (entryId: string) => ({
    instanceEntryId: entryId,
    instanceMaterializationState: 'present',
    file_path: `mods/${entryId}.jar`,
    resourceUpdate: null,
})
function data(revision = 1) {
    return {
        path: 'instance',
        snapshot: { revision, warnings: [] },
        contentItems: [item('fast'), item('slow')],
        linkedContentItems: [],
    } as unknown as InstanceContentData
}
const release = { project: { source: 'modrinth', projectId: 'fast' }, releaseId: 'new' } as const

function pendingCheck() {
    let report!: (progress: ResourceUpdateProgress) => void
    let finish!: (result: CheckResult) => void
    vi.mocked(resourceCheckUpdates).mockImplementationOnce(
        (_path, _signal, options) =>
            new Promise((resolve) => {
                report = options!.onProgress!
                finish = resolve
            }),
    )
    return {
        report: (progress: ResourceUpdateProgress) => report(progress),
        finish: (result: CheckResult) => finish(result),
    }
}

it('hands over each checked entry before the slowest one finishes and settles with the final scan', async () => {
    vi.mocked(resourceCheckUpdates).mockReset()
    const check = pendingCheck()
    const commit = vi.fn()
    const onEntry = vi.fn()
    const scanner = createContentUpdateScanner(commit, onEntry)
    void scanner.scan(data())
    expect(resourceCheckUpdates).toHaveBeenCalledWith('instance', expect.any(AbortSignal), {
        entryIds: ['fast', 'slow'],
        onProgress: expect.any(Function),
    })
    check.report({ entryId: 'fast', release, failed: false, unresolved: false })
    expect(onEntry).toHaveBeenCalledWith({
        instanceId: 'instance',
        revision: 1,
        progress: { entryId: 'fast', release, failed: false, unresolved: false },
        checked: 1,
        total: 2,
    })
    expect(commit).not.toHaveBeenCalled()

    const settled = scanner.settled(data())
    check.finish({
        entries: ['fast'],
        releases: { fast: release },
        unresolved: [],
        failures: [],
        failedEntries: [],
    })
    const scanned = await settled
    expect(
        scanned?.contentItems.find((entry) => entry.instanceEntryId === 'fast')?.resourceUpdate,
    ).toEqual(release)
    expect(commit).toHaveBeenCalledOnce()
    expect(await scanner.settled(data(2))).toBeUndefined()
})

it('drops entries from a scan that a newer snapshot replaced', async () => {
    vi.mocked(resourceCheckUpdates).mockReset()
    const stale = pendingCheck()
    const onEntry = vi.fn()
    const scanner = createContentUpdateScanner(vi.fn(), onEntry)
    void scanner.scan(data(1))
    const replaced = scanner.settled(data(1))
    pendingCheck()
    void scanner.scan(data(2))
    stale.report({ entryId: 'fast', release, failed: false, unresolved: false })
    expect(onEntry).not.toHaveBeenCalled()
    stale.finish({ entries: [], releases: {}, unresolved: [], failures: [], failedEntries: [] })
    expect(await replaced).toBeUndefined()
    scanner.cancel()
})
