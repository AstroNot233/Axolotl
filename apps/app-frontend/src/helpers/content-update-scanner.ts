import {
    type InstanceContentData,
    refreshInstanceContentUpdates,
    updateCheckEntryIds,
} from './instance-content'
import type { ResourceUpdateProgress } from './resources'

/** One checked entry, tagged with the instance and snapshot revision it was checked for. */
export type ContentUpdateEntry = {
    instanceId: string
    revision: number
    progress: ResourceUpdateProgress
    checked: number
    total: number
}

type ScannedSnapshot = Pick<InstanceContentData, 'path' | 'snapshot'>
const scanKey = (data: ScannedSnapshot) => JSON.stringify([data.path, data.snapshot.revision])

/**
 * A page owns one whole-instance scan; metadata viewport changes never restart it.
 * Each entry reaches `onEntry` as soon as it has been checked, and `commit`
 * receives the finished scan.
 */
export function createContentUpdateScanner(
    commit: (data: InstanceContentData) => void,
    onEntry?: (entry: ContentUpdateEntry) => void,
) {
    let current:
        | {
              key: string
              started: number
              controller: AbortController
              pending: Promise<void>
              result?: InstanceContentData
          }
        | undefined

    function cancel() {
        current?.controller.abort()
        current = undefined
    }

    function scan(data: InstanceContentData): Promise<void> {
        const key = scanKey(data)
        if (current?.key === key && Date.now() - current.started < 5 * 60_000) {
            if (current.result) commit(current.result)
            return current.pending
        }
        cancel()
        const controller = new AbortController()
        const total = updateCheckEntryIds(data).length
        let checked = 0
        const pending = refreshInstanceContentUpdates(data, controller.signal, {
            onProgress: (progress) => {
                if (controller.signal.aborted || current?.controller !== controller) return
                checked += 1
                onEntry?.({
                    instanceId: data.path,
                    revision: data.snapshot.revision,
                    progress,
                    checked: Math.min(checked, total),
                    total,
                })
            },
        }).then((updated) => {
            if (!controller.signal.aborted && current?.controller === controller) {
                current.result = updated
                commit(updated)
            }
        })
        current = { key, started: Date.now(), controller, pending }
        return pending
    }

    /**
     * The finished scan of this snapshot, waiting for one that is still running.
     * Undefined when no scan of this snapshot exists or it was replaced.
     */
    async function settled(data: ScannedSnapshot): Promise<InstanceContentData | undefined> {
        const scan = current
        if (scan?.key !== scanKey(data)) return undefined
        await scan.pending.catch(() => {})
        return current === scan ? scan.result : undefined
    }

    return { scan, cancel, settled }
}
