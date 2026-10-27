import { invoke } from '@tauri-apps/api/core'
import { expect, it, vi } from 'vitest'

import { analyzeMod } from '@/lab/mod-translation/backend'

import { abortable } from './abortable'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }))
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }))

it('rejects at once on abort and hands the pending request to onAbort', async () => {
    const request = new Promise<string>(() => {})
    const onAbort = vi.fn()
    const controller = new AbortController()
    const waiting = abortable(request, controller.signal, onAbort)
    controller.abort()
    await expect(waiting).rejects.toMatchObject({ name: 'AbortError' })
    expect(onAbort).toHaveBeenCalledExactlyOnceWith(request)
})

it('treats an already aborted signal as an abort instead of waiting', async () => {
    const controller = new AbortController()
    controller.abort()
    const onAbort = vi.fn()
    await expect(
        abortable(new Promise(() => {}), controller.signal, onAbort),
    ).rejects.toMatchObject({ name: 'AbortError' })
    expect(onAbort).toHaveBeenCalledOnce()
})

it('asks the native mod analysis to stop when its caller aborts', async () => {
    vi.mocked(invoke).mockImplementation((command) =>
        command === 'plugin:mod-translation|mod_translation_analyze'
            ? new Promise(() => {})
            : Promise.resolve(undefined),
    )
    const controller = new AbortController()
    const analysis = analyzeMod('C:/mods/a.jar', controller.signal)
    controller.abort()
    await expect(analysis).rejects.toMatchObject({ name: 'AbortError' })
    const { requestId } = vi.mocked(invoke).mock.calls[0][1] as { requestId: string }
    expect(invoke).toHaveBeenCalledWith('plugin:mod-translation|mod_translation_cancel_analysis', {
        requestId,
    })
})
