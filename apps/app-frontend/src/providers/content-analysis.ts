import {
    resourceAnalysisClose,
    resourceAnalysisOpen,
    resourceAnalysisUpdate,
    type ResourceOperationPlan,
    type ResourceOperationProgress,
    resourcePrepareOperation,
    type ResourceSelection,
} from '@/helpers/resources'

export type ContentAnalysisInput = {
    instanceId: string
    context: string
    selections: ResourceSelection[]
    installDependencies: boolean
}

export type ContentAnalysisReview = {
    signal: AbortSignal
    /** Stops the cart's analysis while it is still running. The selection is kept. */
    cancel: () => void
    prepare: (
        signal: AbortSignal,
        onProgress: (progress: ResourceOperationProgress) => void,
        selections?: ResourceSelection[],
    ) => Promise<ResourceOperationPlan>
}

/**
 * Keeps what a review opened later needs to catch up: the nodes still shown
 * and the latest phase or round, which goes last so it replays last.
 */
function remember(
    progress: Map<string, ResourceOperationProgress>,
    value: ResourceOperationProgress,
) {
    if (value.kind === 'node') progress.set(`node:${value.node.id}`, value)
    else if (value.kind === 'nodeRemoved') progress.delete(`node:${value.nodeId}`)
    else {
        progress.delete('status')
        progress.set('status', value)
    }
}

const selectionKey = (input: ContentAnalysisInput) =>
    JSON.stringify([input.instanceId, input.selections, input.installDependencies])

/**
 * The cart owns background work; a review borrows its current request.
 * Canceling stops that work and closes its session. The same selection is not
 * analyzed again until the selection changes or a review asks for it.
 */
export function createContentAnalysis() {
    let current: {
        key: string
        input: ContentAnalysisInput
        controller: AbortController
        session: Promise<string>
        pending: Promise<ResourceOperationPlan>
        settled: boolean
        revision: number
        progress: Map<string, ResourceOperationProgress>
        listeners: Set<(progress: ResourceOperationProgress) => void>
    } | null = null
    let session: { context: string; id: Promise<string> } | null = null
    let revision = 0
    let latest: ContentAnalysisInput | null = null
    let canceledSelection: string | null = null

    function clear() {
        // Detach before aborting: abort listeners may call back into cancel(),
        // which must not mistake this replacement for a user cancel.
        const replaced = current
        current = null
        replaced?.controller.abort()
        const previous = session
        session = null
        if (previous) void previous.id.then(resourceAnalysisClose).catch(() => {})
    }

    function update(input: ContentAnalysisInput | null) {
        latest = input
        if (!input?.selections.length) {
            canceledSelection = null
            clear()
            return
        }
        if (canceledSelection === selectionKey(input)) return
        canceledSelection = null
        const key = JSON.stringify(input)
        if (current?.key === key) return
        const replaced = current
        current = null
        replaced?.controller.abort()
        const context = JSON.stringify([input.instanceId, input.context])
        if (session?.context !== context) {
            clear()
            session = { context, id: resourceAnalysisOpen(input.instanceId) }
        }
        const id = session!.id
        const controller = new AbortController()
        const progress = new Map<string, ResourceOperationProgress>()
        const listeners = new Set<(progress: ResourceOperationProgress) => void>()
        const selectionRevision = ++revision
        const pending = id.then((sessionId) => {
            controller.signal.throwIfAborted()
            return resourceAnalysisUpdate(
                sessionId,
                selectionRevision,
                input.selections,
                input.installDependencies,
                controller.signal,
                (value) => {
                    if (controller.signal.aborted || value.selectionRevision !== selectionRevision)
                        return
                    remember(progress, value)
                    for (const listener of listeners) listener(value)
                },
            )
        })
        const analysis = {
            key,
            input,
            controller,
            session: id,
            pending,
            settled: false,
            revision: selectionRevision,
            progress,
            listeners,
        }
        // Errors stay with this analysis and are retried by the review action.
        void pending
            .catch(() => {})
            .finally(() => {
                analysis.settled = true
            })
        current = analysis
    }

    /** Stops the running analysis and keeps the selection out of automatic restarts. */
    function cancel() {
        if (!current) return
        canceledSelection = selectionKey(current.input)
        clear()
    }

    function review(): ContentAnalysisReview | undefined {
        // Opening a review is an explicit retry of a canceled selection.
        if (canceledSelection !== null && latest) {
            canceledSelection = null
            update(latest)
        }
        const analysis = current
        if (!analysis) return
        return {
            signal: analysis.controller.signal,
            cancel() {
                if (current === analysis && !analysis.settled) cancel()
            },
            async prepare(signal, onProgress, selections) {
                signal.throwIfAborted()
                analysis.controller.signal.throwIfAborted()
                for (const value of analysis.progress.values()) onProgress(value)
                analysis.listeners.add(onProgress)
                let stopWaiting = () => {}
                const canceled = new Promise<never>((_, reject) => {
                    const abort = () => reject(new DOMException('Analysis canceled', 'AbortError'))
                    signal.addEventListener('abort', abort, { once: true })
                    analysis.controller.signal.addEventListener('abort', abort, { once: true })
                    stopWaiting = () => {
                        signal.removeEventListener('abort', abort)
                        analysis.controller.signal.removeEventListener('abort', abort)
                    }
                })
                const check = () => {
                    signal.throwIfAborted()
                    analysis.controller.signal.throwIfAborted()
                    if (current !== analysis) throw new DOMException('Cart changed', 'AbortError')
                }
                try {
                    // Reuse in-flight work. Closing the review does not cancel
                    // the cart's independent subscription.
                    await Promise.race([analysis.pending.catch(() => {}), canceled])
                    check()
                    const sessionId = await analysis.session.catch(() => undefined)
                    check()
                    const prepare = (analysisSession?: {
                        sessionId: string
                        selectionRevision: number
                    }) =>
                        resourcePrepareOperation(
                            analysis.input.instanceId,
                            'install',
                            selections ?? analysis.input.selections,
                            analysis.input.installDependencies,
                            signal,
                            undefined,
                            undefined,
                            onProgress,
                            true,
                            analysisSession,
                        )
                    try {
                        return await prepare(
                            sessionId
                                ? { sessionId, selectionRevision: analysis.revision }
                                : undefined,
                        )
                    } catch (error) {
                        check()
                        if (String(error).includes('analysis session expired'))
                            return await prepare()
                        throw error
                    }
                } finally {
                    stopWaiting()
                    analysis.listeners.delete(onProgress)
                }
            },
        }
    }

    return { update, review, cancel, clear }
}
