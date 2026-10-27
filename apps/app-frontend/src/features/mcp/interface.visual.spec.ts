import { beforeEach, expect, it, vi } from 'vitest'
import { createMemoryHistory, createRouter } from 'vue-router'

import { startMcpUiBridge } from './interface'

const mocks = vi.hoisted(() => ({
    invoke: vi.fn(),
    execute: vi.fn(),
    stop: vi.fn(),
    unlisten: vi.fn(),
    receive: undefined as
        undefined | ((event: { payload: { request_id: string } }) => Promise<void>),
}))
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }))
vi.mock('@tauri-apps/api/event', () => ({
    listen: vi.fn(async (_name, callback) => {
        mocks.receive = callback
        return mocks.unlisten
    }),
}))
vi.mock('./interface-dom', () => ({
    createMcpUiController: () => ({ execute: mocks.execute, stop: mocks.stop }),
}))

beforeEach(() => {
    vi.clearAllMocks()
    mocks.invoke.mockReset()
    mocks.execute.mockReset()
})
const router = () => createRouter({ history: createMemoryHistory(), routes: [] })

it('requires a native claim before executing and responds with the same request ID', async () => {
    const stop = await startMcpUiBridge(router())
    const action = { kind: 'click', view_id: 'view', element_id: 'element-1' }
    mocks.invoke
        .mockResolvedValueOnce({ action, scope: 'server-verified-session' })
        .mockResolvedValueOnce(undefined)
    mocks.execute.mockResolvedValue({ performed: true })
    await mocks.receive!({ payload: { request_id: 'request' } })
    expect(mocks.execute).toHaveBeenCalledExactlyOnceWith(action, 'server-verified-session')
    expect(mocks.invoke.mock.calls[0]).toEqual([
        'plugin:mcp|mcp_ui_claim',
        { requestId: 'request' },
    ])
    expect(mocks.invoke.mock.calls[1]).toEqual([
        'plugin:mcp|mcp_ui_respond',
        { requestId: 'request', result: { ok: true, data: { performed: true } } },
    ])
    stop()
    expect(mocks.unlisten).toHaveBeenCalledOnce()
    expect(mocks.stop).toHaveBeenCalledOnce()
})

it('never executes a revoked or expired native request', async () => {
    const stop = await startMcpUiBridge(router())
    mocks.invoke.mockRejectedValueOnce(new Error('Control session ended'))
    await mocks.receive!({ payload: { request_id: 'expired' } })
    expect(mocks.execute).not.toHaveBeenCalled()
    expect(mocks.invoke).toHaveBeenCalledOnce()
    stop()
})

it('does not execute a request whose claim completes after the bridge stopped', async () => {
    const stop = await startMcpUiBridge(router())
    let resolve: (value: unknown) => void = () => {}
    mocks.invoke.mockReturnValueOnce(
        new Promise((done) => {
            resolve = done
        }),
    )
    const request = mocks.receive!({ payload: { request_id: 'pending' } })
    stop()
    resolve({ action: { kind: 'context' }, scope: 'old-session' })
    await request
    expect(mocks.execute).not.toHaveBeenCalled()
    await mocks.receive!({ payload: { request_id: 'late' } })
    expect(mocks.invoke).toHaveBeenCalledOnce()
})
