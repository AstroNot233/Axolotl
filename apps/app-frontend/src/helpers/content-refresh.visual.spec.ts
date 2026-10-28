import { expect, it, vi } from 'vitest'

import {
    applyContentRefreshEvent,
    type ContentRefreshEvent,
    type ContentRefreshState,
    type ContentRefreshTransport,
    createContentRefreshSubscription,
} from './content-refresh'

function state(sequence = 0): ContentRefreshState {
    return {
        sequence,
        task: null,
        snapshot: {
            instanceId: 'instance',
            revision: 1,
            pack: null,
            warnings: [],
            pendingManualDownloads: [],
            items: [
                {
                    fileId: 'file',
                    entryId: 'entry',
                    memberId: null,
                    expectedRelativePath: 'mods/a.jar',
                    ownershipKind: 'user_added',
                    materializationState: 'present',
                    overrideKind: 'none',
                    required: true,
                    projectType: 'mod',
                    provider: null,
                    providerProjectId: null,
                    providerReleaseId: null,
                    dependency: null,
                    capabilities: {
                        canToggle: true,
                        canDelete: true,
                        canUpdate: false,
                        canChangeVersion: true,
                        canRestorePackDefault: false,
                    },
                    content: {
                        id: 'hash',
                        file_name: 'a.jar',
                        file_path: 'mods/a.jar',
                        size: 1,
                        enabled: true,
                        project_type: 'mod',
                    },
                },
            ],
        },
    }
}

function event(sequence: number): Exclude<ContentRefreshEvent, { kind: 'resync' }> {
    return {
        sequence,
        instanceId: 'instance',
        taskId: 'task',
        task: {
            id: 'task',
            mode: 'normal',
            running: true,
            scanned: 1,
            checked: 1,
            failed: 0,
            unresolved: 0,
            errors: [],
        },
        patch: {
            kind: 'entry',
            entryId: 'entry',
            fileId: 'file',
            sha1: 'hash',
            update: {
                candidate: {
                    project: { source: 'curseforge', projectId: '123' },
                    releaseId: '456',
                },
                failed: false,
                unresolved: false,
            },
        },
    }
}

function fixture(initial = state()) {
    let listener!: (event: ContentRefreshEvent) => void
    let frame: (() => void) | undefined
    const unlisten = vi.fn()
    const read = vi.fn(async () => initial)
    const start = vi.fn(async () => event(1).task)
    const transport: ContentRefreshTransport = {
        listen: vi.fn(async (callback) => {
            listener = callback
            return unlisten
        }),
        read,
        start,
        schedule: (callback) => {
            frame = callback
            return () => {
                frame = undefined
            }
        },
    }
    const change = vi.fn()
    const error = vi.fn()
    const subscription = createContentRefreshSubscription('instance', change, error, transport)
    return {
        subscription,
        change,
        error,
        transport,
        read,
        unlisten,
        receive: (event: ContentRefreshEvent) => listener(event),
        async flush() {
            const callback = frame
            frame = undefined
            callback?.()
            await vi.waitFor(() => expect(error).not.toHaveBeenCalled())
            await Promise.resolve()
            await Promise.resolve()
        },
    }
}

it('subscribes before reading and replays events arriving during the state request', async () => {
    const test = fixture()
    let resolve!: (state: ContentRefreshState) => void
    test.read.mockImplementationOnce(
        () =>
            new Promise((done) => {
                resolve = done
            }),
    )
    const connecting = test.subscription.connect()
    await vi.waitFor(() => expect(test.read).toHaveBeenCalledOnce())
    expect(test.transport.listen).toHaveBeenCalledOnce()
    test.receive(event(1))
    resolve(state())
    await connecting
    await test.flush()
    expect(test.change.mock.lastCall?.[0].snapshot.items[0].resourceUpdate?.releaseId).toBe('456')
    test.subscription.dispose()
})

it('sorts and deduplicates events in one frame and reloads local state for gaps', async () => {
    const test = fixture()
    await test.subscription.connect()
    test.receive(event(2))
    test.receive(event(1))
    test.receive(event(1))
    await test.flush()
    expect(test.change.mock.lastCall?.[0].sequence).toBe(2)
    expect(test.read).toHaveBeenCalledOnce()
    test.read.mockResolvedValueOnce(state(5))
    test.receive(event(5))
    await test.flush()
    await vi.waitFor(() => expect(test.read).toHaveBeenCalledTimes(2))
    expect(test.transport.start).not.toHaveBeenCalled()
    test.subscription.dispose()
})

it('disposal only unsubscribes and a late state response cannot publish', async () => {
    const test = fixture()
    let resolve!: (state: ContentRefreshState) => void
    test.read.mockImplementationOnce(
        () =>
            new Promise((done) => {
                resolve = done
            }),
    )
    const connecting = test.subscription.connect()
    await vi.waitFor(() => expect(test.read).toHaveBeenCalledOnce())
    test.subscription.dispose()
    resolve(state())
    await connecting
    expect(test.unlisten).toHaveBeenCalledOnce()
    expect(test.change).not.toHaveBeenCalled()
})

it('metadata preserves candidates while an explicit null clears them and stale files are ignored', () => {
    const first = applyContentRefreshEvent(state(), event(1))
    const metadata = event(2)
    metadata.patch = {
        kind: 'entry',
        entryId: 'entry',
        fileId: 'file',
        sha1: 'hash',
        presentation: {
            ...first.snapshot.items[0].content!,
            project: { id: 'project', title: 'Updated name', slug: 'project' },
        },
    }
    const updated = applyContentRefreshEvent(first, metadata)
    expect(updated.snapshot.items[0].resourceUpdate?.releaseId).toBe('456')
    expect(updated.snapshot.items[0].content?.project?.title).toBe('Updated name')
    const stale = event(3)
    if (stale.patch.kind === 'entry') stale.patch.fileId = 'old-file'
    expect(applyContentRefreshEvent(updated, stale).snapshot.items[0]).toBe(
        updated.snapshot.items[0],
    )
    const clear = event(4)
    if (clear.patch.kind === 'entry') clear.patch.update!.candidate = null
    expect(applyContentRefreshEvent(updated, clear).snapshot.items[0].capabilities.canUpdate).toBe(
        false,
    )
})
