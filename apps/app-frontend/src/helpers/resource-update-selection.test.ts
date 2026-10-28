import type { ContentItem } from '@modrinth/ui'
import { expect, it } from 'vitest'

import type { InstallJobSnapshot } from './install.ts'
import {
    confirmedContentGroupKey,
    duplicateGroupUpdateEntries,
    type ResourceEntryReview,
    reviewResourceEntryChanges,
} from './resource-update-selection.ts'

function item(entry: string, overrides: Partial<ContentItem> = {}): ContentItem {
    return {
        id: entry,
        instanceEntryId: entry,
        instanceResourceId: 'confirmed',
        project_type: 'mod',
        file_path: `mods/${entry}.jar`,
        instanceMaterializationState: 'present',
        resourceUpdate: { project: { source: 'modrinth', projectId: 'project' }, releaseId: 'new' },
        ...overrides,
    } as ContentItem
}

it('duplicate group update includes every actual updatable entry across sources and versions', () => {
    const anchor = item('a')
    const copies = [
        anchor,
        item('b', { origin_provider: 'curseforge' }),
        item('b'),
        item('c', { resourceUpdate: null }),
        item('missing', { instanceMaterializationState: 'missing' }),
        item('other', { instanceResourceId: 'another' }),
    ]
    expect(duplicateGroupUpdateEntries(copies, anchor)).toEqual(['a', 'b'])
    expect(confirmedContentGroupKey(item('unverified', { instanceResourceId: undefined }))).toBe(
        null,
    )
})

it('world scopes and content types stay independent', () => {
    const a = item('a', { project_type: 'datapack', file_path: 'saves/one/datapacks/a.zip' })
    const b = item('b', { project_type: 'datapack', file_path: 'saves/two/datapacks/b.zip' })
    const c = item('c', {
        project_type: 'datapack',
        file_path: 'saves/one/datapacks/c.zip.disabled',
    })
    expect(duplicateGroupUpdateEntries([a, b, c, item('mod')], a)).toEqual(['a', 'c'])
    expect(confirmedContentGroupKey(item('invalid', { project_type: 'datapack' }))).toBe(null)
})

it('single, selected, all and duplicate updates retain exact entries and dependency preferences for review', async () => {
    for (const entryIds of [
        ['single'],
        ['a', 'b'],
        ['a', 'b', 'c'],
        ['copy-a', 'copy-b', 'copy-a'],
    ]) {
        for (const installDependencies of [false, true]) {
            let reviewed: ResourceEntryReview | undefined
            const tracked: string[] = []
            const result = await reviewResourceEntryChanges(
                {
                    instanceId: 'instance',
                    instanceName: 'Example',
                    entryIds,
                    release: null,
                    installDependencies,
                },
                async (input) => {
                    reviewed = input
                    expect(tracked).toEqual([])
                    return { job_id: 'reviewed' } as InstallJobSnapshot
                },
                (job) => {
                    tracked.push(job.job_id)
                },
            )
            expect(result).toBe(true)
            expect(reviewed?.entryIds).toEqual([...new Set(entryIds)])
            expect(reviewed?.installDependencies).toBe(installDependencies)
            expect(reviewed?.intent).toBe('update')
            expect(tracked).toEqual(['reviewed'])
        }
    }
})

it('cancelled and failed previews do not track a job or alter selected entries', async () => {
    const input = {
        instanceId: 'instance',
        instanceName: 'Example',
        entryIds: ['entry'],
        release: null,
        installDependencies: false,
    }
    let tracked = false
    const track = () => {
        tracked = true
    }
    expect(await reviewResourceEntryChanges(input, async () => null, track)).toBe(false)
    await expect(
        reviewResourceEntryChanges(
            input,
            async () => {
                throw new Error('source unavailable')
            },
            track,
        ),
    ).rejects.toThrow()
    expect(tracked).toBe(false)
    expect(input.entryIds).toEqual(['entry'])
})
